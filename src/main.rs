mod audio;
mod battery;
mod board;
mod buttons;
mod clock;
mod commands;
mod config;
mod dashboard;
mod datetime;
mod epaper;
mod events;
mod i2c_bus;
mod ink_stacks;
mod language;
mod location;
mod notifications;
mod ntp;
mod ota;
mod power;
mod power_log;
mod rtc;
mod shtc3;
mod weather;
mod wifi;

use std::borrow::Borrow;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use audio::Audio;
use battery::{Battery, BatteryStatus};
use board::BoardPower;
use buttons::ButtonEvent;
use clock::Clock;
use commands::Command;
use dashboard::{updates, DashboardData, DashboardScreen};
use embedded_graphics::geometry::Size;
use epaper::{Epaper, FRAMEBUFFER_SIZE, HEIGHT, WIDTH};
use esp_idf_hal::adc::oneshot::config::{AdcChannelConfig, Calibration};
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::adc::{attenuation, AdcChannel};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{AnyInputPin, PinDriver, Pull};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::spi::{config as spi_config, Dma, SpiDeviceDriver, SpiDriverConfig};
use esp_idf_hal::units::FromValueType;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::netif::IpEvent;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::timer::{EspTaskTimerService, EspTimer};
use esp_idf_svc::wifi::WifiEvent;
use events::AppEvent;
use i2c_bus::I2cBus;
use ink_stacks::Framebuffer;
use language::{Language, LanguageStore};
use location::LocationService;
use notifications::BatteryNotificationSchedule;
use power_log::PowerLog;
use shtc3::Shtc3;
use weather::WeatherService;
use wifi::{WifiCredentials, WifiManager};

/// Everything a console command may need to read or change.
struct CommandContext<'a, 'i, 's> {
    clock: &'a mut Clock,
    i2c: &'a mut I2cBus<'i>,
    wifi: &'a mut WifiManager,
    audio: &'a mut Audio<'s>,
    language_store: &'a LanguageStore,
    ota_endpoint: &'a ota::EndpointStore,
    language: &'a mut Language,
    power_log: &'a mut PowerLog,
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();
    log::info!("Starting modular Rust e-paper dashboard");
    let mut power_policy = power::PowerPolicy::initialize()?;
    power::track_light_sleep()?;

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    // GPIO17 must stay high while the onboard I2C peripherals are in use.
    let _board_power = BoardPower::enable(pins.gpio17, pins.gpio42)?;

    // Waveshare's RTC and SHTC3 examples enable the EPD peripheral rail before
    // bringing up I2C. Keep this pin driver and move it into Epaper below.
    let mut epaper_power = PinDriver::output(pins.gpio6)?;
    epaper_power.set_low()?; // Peripheral rail is active low.
    FreeRtos::delay_ms(100);

    let mut i2c = I2cBus::new(
        peripherals.i2c0,
        pins.gpio47, // Shared RTC/SHTC3 SDA
        pins.gpio48, // Shared RTC/SHTC3 SCL
    )?;
    let (sda, scl) = i2c.line_levels();
    log::info!("I2C idle levels before reset: SDA={sda}, SCL={scl}");
    match i2c.reset() {
        Ok(()) => {
            let (sda, scl) = i2c.line_levels();
            log::info!("I2C idle levels after reset: SDA={sda}, SCL={scl}");
        }
        Err(error) => log::warn!("I2C bus reset failed: {error:#}"),
    }
    for (address, name) in [(0x51, "PCF85063 RTC"), (0x70, "SHTC3")] {
        match i2c.probe(address) {
            Ok(()) => log::info!("I2C probe found {name} at 0x{address:02x}"),
            Err(error) => log::warn!("I2C probe failed for {name} at 0x{address:02x}: {error:#}"),
        }
    }
    let mut clock = Clock::new();
    let climate_sensor = Shtc3::new();
    match climate_sensor.initialize(&mut i2c) {
        Ok(id) => log::info!("SHTC3 detected, ID 0x{id:04x}"),
        Err(error) => log::warn!("SHTC3 initialization failed: {error:#}"),
    }

    let adc = Arc::new(AdcDriver::new(peripherals.adc1)?);
    let adc_config = AdcChannelConfig {
        attenuation: attenuation::DB_12,
        calibration: Calibration::Curve,
        ..Default::default()
    };
    let battery_channel = AdcChannelDriver::new(adc, pins.gpio4, &adc_config)?;
    let mut battery = Battery::new(battery_channel);

    let spi_config = spi_config::Config::new()
        .baudrate(20.MHz().into())
        .write_only(true);
    let spi_driver_config = SpiDriverConfig::new().dma(Dma::Auto(FRAMEBUFFER_SIZE));
    let spi = SpiDeviceDriver::new_single(
        peripherals.spi2,
        pins.gpio12, // EPD SCLK
        pins.gpio13, // EPD MOSI / SDI
        None::<AnyInputPin>,
        Some(pins.gpio11), // EPD CS
        &spi_driver_config,
        &spi_config,
    )?;
    let busy = PinDriver::input(pins.gpio8, Pull::Floating)?;
    let mut reset = PinDriver::output(pins.gpio9)?;
    reset.set_high()?;
    let mut dc = PinDriver::output(pins.gpio10)?;
    dc.set_low()?;
    let mut epaper = Epaper::new(spi, epaper_power, busy, reset, dc)?;

    let mut audio = Audio::new(pins.gpio46)?;

    let system_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let language_store = LanguageStore::new(nvs.clone())?;
    let ota_endpoint_store = ota::EndpointStore::new(nvs.clone())?;
    let language = language_store.load()?;
    let (event_sender, events) = events::channel();

    let wifi_sender = event_sender.clone();
    let _wifi_subscription = system_loop.subscribe::<WifiEvent, _>(move |event| {
        if matches!(
            event,
            WifiEvent::StaStarted
                | WifiEvent::StaStopped
                | WifiEvent::StaConnected(_)
                | WifiEvent::StaDisconnected(_)
                | WifiEvent::StaBssRssiLow
        ) {
            let _ = wifi_sender.send(AppEvent::WifiChanged);
        }
    })?;
    let ip_sender = event_sender.clone();
    let _ip_subscription = system_loop.subscribe::<IpEvent, _>(move |_| {
        let _ = ip_sender.send(AppEvent::WifiChanged);
    })?;

    let timer_service = EspTaskTimerService::new()?;
    let clock_sender = event_sender.clone();
    let clock_timer = timer_service.timer(move || {
        let _ = clock_sender.send(AppEvent::ClockDue);
    })?;
    let weather_sender = event_sender.clone();
    let weather_timer = timer_service.timer(move || {
        let _ = weather_sender.send(AppEvent::WeatherDue);
    })?;
    let reconnect_sender = event_sender.clone();
    let reconnect_timer = timer_service.timer(move || {
        let _ = reconnect_sender.send(AppEvent::ReconnectDue);
    })?;
    let ota_confirmation_sender = event_sender.clone();
    let ota_confirmation_timer = timer_service.timer(move || {
        let _ = ota_confirmation_sender.send(AppEvent::OtaConfirmationExpired);
    })?;
    let ota_check_sender = event_sender.clone();
    let ota_check_timer = timer_service.timer(move || {
        let _ = ota_check_sender.send(AppEvent::OtaCheckDue);
    })?;
    let ota_restart_sender = event_sender.clone();
    let ota_restart_timer = timer_service.timer(move || {
        let _ = ota_restart_sender.send(AppEvent::OtaRestartDue);
    })?;
    let power_sample_sender = event_sender.clone();
    let power_sample_timer = timer_service.timer(move || {
        let _ = power_sample_sender.send(AppEvent::PowerSampleDue);
    })?;

    let mut location = LocationService::start(nvs.clone(), event_sender.clone())?;
    let mut weather = WeatherService::start(event_sender.clone())?;
    let ota_service = ota::Service::start(event_sender.clone())?;
    let mut wifi = WifiManager::new(peripherals.modem, system_loop, nvs)?;
    match wifi.connect_saved() {
        Ok(true) => log::info!("Connected using saved Wi-Fi credentials"),
        Ok(false) => log::info!("No saved Wi-Fi; configure it over USB"),
        Err(error) => log::warn!("Saved Wi-Fi connection failed: {error:#}"),
    }
    let _ntp_service = ntp::start(event_sender.clone())?;
    if wifi.is_connected() {
        location.request();
    }

    commands::start_usb_console(event_sender.clone())?;
    buttons::start(pins.gpio0, pins.gpio18, event_sender.clone())?;
    println!("READY Rust e-paper dashboard");
    println!("{}", commands::help_text());

    let display_size = Size::new(WIDTH as u32, HEIGHT as u32);
    let mut displayed_frame = Framebuffer::new(display_size);
    let mut next_frame = Framebuffer::new(display_size);
    let mut panel_initialized = false;
    let mut partial_refreshes = 0_u8;
    let mut render_requested = true;
    let mut force_full_refresh = false;
    let mut screen = DashboardScreen::Home;
    let mut ota_screen = updates::Screen::Hidden;
    let mut pending_update: Option<ota::UpdateInfo> = None;
    let mut ota_operation_active = false;
    let mut running_image_confirmation_checked = false;
    let mut system_time_synchronized = false;
    // The USB SOF monitor has had time to settle during peripheral setup.
    power_policy.refresh()?;
    let mut data = DashboardData::new(language);
    refresh_dashboard_data(
        &mut data,
        &mut clock,
        &climate_sensor,
        &mut i2c,
        &mut battery,
        &wifi,
        weather.latest(),
    );
    let mut battery_notifications = BatteryNotificationSchedule::new();
    queue_battery_notification(&mut battery_notifications, &data, &event_sender);
    schedule_clock(&clock_timer, data.time)?;
    let mut power_log = PowerLog::new();
    power_sample_timer.every(power_log::SAMPLE_INTERVAL)?;
    weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
    ota_check_timer.after(config::OTA_CHECK_STARTUP_DELAY)?;

    loop {
        if render_requested {
            dashboard::render(&mut next_frame, &data, screen, &ota_screen);
            let changed = displayed_frame.changed_regions(&next_frame);
            let needs_full = force_full_refresh
                || !panel_initialized
                || partial_refreshes >= config::FULL_REFRESH_AFTER_PARTIALS;
            let refresh_result = match (needs_full, changed.as_slice()) {
                (true, _) => epaper
                    .init_full()
                    .and_then(|()| epaper.display_base(next_frame.bytes()))
                    .and_then(|()| epaper.init_partial()),
                (false, regions @ [_, ..]) => {
                    log::info!(
                        "Visual diff: updating {} region(s): {regions:?}",
                        regions.len()
                    );
                    epaper.display_partial_windows(next_frame.bytes(), regions)
                }
                (false, []) => {
                    log::debug!("Render reconciled with no visual changes");
                    render_requested = false;
                    continue;
                }
            };

            match refresh_result {
                Ok(()) => {
                    std::mem::swap(&mut displayed_frame, &mut next_frame);
                    panel_initialized = true;
                    partial_refreshes = if needs_full { 0 } else { partial_refreshes + 1 };
                    force_full_refresh = false;
                    let (slot, sky) = dashboard::scene(&data);
                    log::info!(
                        "Dashboard refreshed: time={}, scene={}/{}, temp={}, humidity={}, wifi={}, rssi={}, weather={}, battery={}, eco={}, heap={}/{} KiB free/min",
                        data.time
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unset".into()),
                        slot.map_or("none", |slot| slot.label()),
                        sky.label(),
                        data.climate
                            .map(|value| format!("{:.1} C", value.temperature_c))
                            .unwrap_or_else(|| "unavailable".into()),
                        data.climate
                            .map(|value| format!("{:.0}%", value.humidity_percent))
                            .unwrap_or_else(|| "unavailable".into()),
                        if data.wifi_connected { "up" } else { "down" },
                        data.wifi_signal_dbm
                            .map(|value| format!("{value} dBm"))
                            .unwrap_or_else(|| "unavailable".into()),
                        data.weather
                            .as_deref()
                            .map(|value| format!("{:.1} C {}", value.temperature_c, value.condition()))
                            .unwrap_or_else(|| "unavailable".into()),
                        match data.battery.reading {
                            Some(value) => format!(
                                "{}% ({:.2} V, {})",
                                value.percent,
                                value.voltage_v,
                                data.power_source.label()
                            ),
                            None if data.power_source.is_external() => {
                                "not detected (PWR)".into()
                            }
                            None => "not detected".into(),
                        },
                        if data.power_source.is_external() { "off" } else { "on" },
                        unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } / 1024,
                        unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() } / 1024,
                    );
                    if !running_image_confirmation_checked {
                        running_image_confirmation_checked = true;
                        match ota::confirm_running_image() {
                            Ok(true) => {
                                log::info!("Confirmed healthy OTA image after first render")
                            }
                            Ok(false) => {}
                            Err(error) => {
                                log::error!("Confirming running OTA image failed: {error:#}")
                            }
                        }
                    }
                }
                Err(error) => log::error!("Dashboard refresh failed: {error:#}"),
            }
            render_requested = false;
        }

        // The application has no idle cadence: it sleeps here until a producer
        // emits an event, then drains the already-queued burst before rendering.
        let first_event = events
            .recv()
            .map_err(|_| anyhow::anyhow!("all application event producers stopped"))?;
        let mut pending = Some(first_event);
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => match events.try_recv() {
                    Ok(event) => event,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                },
            };
            if let Some(source) = power_policy.refresh()? {
                // Invalidate only source-dependent UI state. Battery voltage
                // keeps its normal minute sampling cadence.
                data.power_source = source;
                render_requested = true;
            }
            match event {
                AppEvent::Button(button) => match button {
                    ButtonEvent::CheckForUpdates
                        if !ota_screen.can_start_check() || ota_operation_active => {}
                    ButtonEvent::CheckForUpdates => {
                        ota_service.cancel();
                        ota_confirmation_timer.cancel()?;
                        pending_update = None;
                        ota_screen = updates::Screen::Checking;
                        render_requested = true;
                        force_full_refresh = true;
                        weather_timer.cancel()?;
                        if !data.wifi_connected {
                            ota_screen = updates::Screen::Failed {
                                message: "No Wi-Fi connection".into(),
                            };
                        } else {
                            match ota_endpoint_store.effective() {
                                Ok(Some(endpoint)) => {
                                    if ota_service.check(endpoint.url) {
                                        ota_operation_active = true;
                                    } else {
                                        ota_screen = updates::Screen::Failed {
                                            message: "OTA worker is unavailable".into(),
                                        };
                                    }
                                }
                                Ok(None) => {
                                    ota_screen = updates::Screen::Failed {
                                        message: "OTA endpoint is not configured".into(),
                                    };
                                }
                                Err(error) => {
                                    ota_screen = updates::Screen::Failed {
                                        message: format!("OTA endpoint: {error:#}"),
                                    };
                                }
                            }
                        }
                        // Nothing was dispatched, so no OTA completion event will
                        // restore the weather cadence cancelled just above.
                        if !ota_operation_active {
                            weather_timer.after(config::WEATHER_RETRY_INTERVAL)?;
                        }
                    }
                    ButtonEvent::Boot if ota_screen.can_accept() => {
                        ota_confirmation_timer.cancel()?;
                        if let Some(update) = pending_update.take() {
                            let version = update.version.clone();
                            let total = update.size;
                            ota_screen = updates::Screen::Downloading {
                                version,
                                downloaded: 0,
                                total,
                            };
                            render_requested = true;
                            if !ota_service.install(update) {
                                ota_screen = updates::Screen::Failed {
                                    message: "OTA worker is unavailable".into(),
                                };
                                if data.wifi_connected {
                                    weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                                }
                            } else {
                                ota_operation_active = true;
                                log::info!("OTA installation accepted ({total} bytes)");
                            }
                        }
                    }
                    ButtonEvent::Boot if !ota_screen.is_visible() => {
                        screen = screen.next();
                        render_requested = true;
                        force_full_refresh = true;
                    }
                    ButtonEvent::Boot => {}
                    ButtonEvent::Power
                        if matches!(
                            ota_screen,
                            updates::Screen::Finalizing { .. } | updates::Screen::Restarting { .. }
                        ) => {}
                    ButtonEvent::Power if ota_screen.is_visible() => {
                        if ota_screen.can_cancel() {
                            ota_service.cancel();
                        }
                        ota_confirmation_timer.cancel()?;
                        pending_update = None;
                        ota_screen = updates::Screen::Hidden;
                        render_requested = true;
                        force_full_refresh = true;
                        if data.wifi_connected {
                            weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                        }
                    }
                    ButtonEvent::Power => {
                        screen = screen.previous();
                        render_requested = true;
                        force_full_refresh = true;
                    }
                },
                AppEvent::Command(Ok(command)) => {
                    let clock_changed = matches!(&command, Command::TimeSet(_));
                    let explicit_refresh = matches!(&command, Command::Refresh);
                    let language_changed = matches!(&command, Command::LanguageSet(_));
                    if handle_command(
                        command,
                        &mut CommandContext {
                            clock: &mut clock,
                            i2c: &mut i2c,
                            wifi: &mut wifi,
                            audio: &mut audio,
                            language_store: &language_store,
                            ota_endpoint: &ota_endpoint_store,
                            language: &mut data.language,
                            power_log: &mut power_log,
                        },
                    ) {
                        refresh_dashboard_data(
                            &mut data,
                            &mut clock,
                            &climate_sensor,
                            &mut i2c,
                            &mut battery,
                            &wifi,
                            weather.latest(),
                        );
                        render_requested = true;
                    }
                    if explicit_refresh || language_changed {
                        force_full_refresh = true;
                    }
                    if clock_changed {
                        schedule_clock(&clock_timer, data.time)?;
                    }
                }
                AppEvent::Command(Err(error)) => println!("ERR {error}"),
                AppEvent::ClockDue => {
                    refresh_dashboard_data(
                        &mut data,
                        &mut clock,
                        &climate_sensor,
                        &mut i2c,
                        &mut battery,
                        &wifi,
                        weather.latest(),
                    );
                    queue_battery_notification(&mut battery_notifications, &data, &event_sender);
                    schedule_clock(&clock_timer, data.time)?;
                    render_requested = true;
                }
                AppEvent::PowerSampleDue => {
                    // Diagnostics only: deliberately leaves `render_requested`
                    // alone so the panel keeps its one-minute cadence.
                    match battery.read_millivolts() {
                        Ok(millivolts) => power_log.record(
                            millivolts,
                            power::source().is_external(),
                            power::slept_recently(),
                        ),
                        Err(error) => log::warn!("Battery sample failed: {error:#}"),
                    }
                }
                AppEvent::WeatherDue => {
                    if !wifi.is_connected() {
                        weather_timer.after(config::WEATHER_RETRY_INTERVAL)?;
                    } else if let Some(current_location) = location.latest() {
                        if weather.request(current_location) {
                            log::debug!("Weather refresh dispatched");
                        } else {
                            weather_timer.after(config::WEATHER_RETRY_INTERVAL)?;
                        }
                    } else if location.request() {
                        log::debug!("Location lookup dispatched before weather refresh");
                    } else {
                        weather_timer.after(config::WEATHER_RETRY_INTERVAL)?;
                    }
                }
                AppEvent::WeatherCompleted(update) => {
                    let succeeded = update.is_ok();
                    if weather.complete(update) {
                        data.weather = weather.latest();
                        render_requested = true;
                    }
                    weather_timer.after(if succeeded {
                        config::WEATHER_REFRESH_INTERVAL
                    } else {
                        config::WEATHER_RETRY_INTERVAL
                    })?;
                }
                AppEvent::LocationCompleted(update) => {
                    let previous_utc_offset = location
                        .latest()
                        .map(|location| location.utc_offset_seconds);
                    let succeeded = update.is_ok();
                    location.complete(update);
                    let current_location = location.latest();
                    let changed_utc_offset = current_location
                        .as_ref()
                        .map(|location| location.utc_offset_seconds)
                        .filter(|offset| {
                            system_time_synchronized && Some(*offset) != previous_utc_offset
                        });
                    if let Some(utc_offset_seconds) = changed_utc_offset {
                        let rtc_updated = synchronize_rtc_from_system_time(
                            &mut clock,
                            &mut i2c,
                            utc_offset_seconds,
                        );
                        if rtc_updated {
                            refresh_dashboard_data(
                                &mut data,
                                &mut clock,
                                &climate_sensor,
                                &mut i2c,
                                &mut battery,
                                &wifi,
                                weather.latest(),
                            );
                            schedule_clock(&clock_timer, data.time)?;
                            render_requested = true;
                        }
                    }
                    if succeeded && current_location.is_some() && wifi.is_connected() {
                        weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                    } else if current_location.is_none() {
                        weather_timer.after(config::WEATHER_RETRY_INTERVAL)?;
                    }
                }
                AppEvent::Notification(notification) => {
                    if ota_screen.is_visible() {
                        log::debug!(
                            "Suppressing {} notification while OTA screen is active",
                            notification.name()
                        );
                    } else if power::source().is_external() {
                        log::debug!(
                            "Suppressing {} notification after external power attached",
                            notification.name()
                        );
                    } else if let Err(error) = audio.play_notification(&mut i2c, notification) {
                        log::error!(
                            "Playing {} notification failed: {error:#}",
                            notification.name()
                        );
                    }
                }
                AppEvent::WifiChanged => {
                    let was_connected = data.wifi_connected;
                    update_wifi_data(&mut data, &wifi);
                    render_requested = true;
                    if wifi.is_connected() {
                        reconnect_timer.cancel()?;
                        if !was_connected {
                            weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                        }
                    } else {
                        reconnect_timer.after(config::WIFI_RECONNECT_INTERVAL)?;
                    }
                }
                AppEvent::ReconnectDue => match wifi.reconnect_saved() {
                    Ok(true) => reconnect_timer.after(config::WIFI_RECONNECT_INTERVAL)?,
                    Ok(false) => {}
                    Err(error) => {
                        log::warn!("Wi-Fi reconnect failed: {error:#}");
                        reconnect_timer.after(config::WIFI_RECONNECT_INTERVAL)?;
                    }
                },
                AppEvent::NtpSynchronized => {
                    system_time_synchronized = true;
                    if let Some(utc_offset_seconds) = location
                        .latest()
                        .map(|location| location.utc_offset_seconds)
                    {
                        if synchronize_rtc_from_system_time(
                            &mut clock,
                            &mut i2c,
                            utc_offset_seconds,
                        ) {
                            refresh_dashboard_data(
                                &mut data,
                                &mut clock,
                                &climate_sensor,
                                &mut i2c,
                                &mut battery,
                                &wifi,
                                weather.latest(),
                            );
                            schedule_clock(&clock_timer, data.time)?;
                            render_requested = true;
                        }
                    } else {
                        log::info!(
                            "NTP synchronized system time; waiting for location timezone before updating RTC"
                        );
                    }
                    if location.latest().is_some() {
                        if !location.refresh() {
                            log::warn!("Location worker is unavailable for timezone refresh");
                        }
                    } else if !location.request() {
                        log::warn!("Location worker is unavailable for timezone lookup");
                    }
                }
                AppEvent::Ota(update) => {
                    match update {
                        ota::WorkerEvent::Checked(Ok(ota::CheckResult::UpToDate))
                            if matches!(ota_screen, updates::Screen::Checking) =>
                        {
                            ota_operation_active = false;
                            data.update_available = false;
                            ota_screen = updates::Screen::UpToDate;
                            if data.wifi_connected {
                                weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                            }
                        }
                        ota::WorkerEvent::Checked(Ok(ota::CheckResult::Available(update)))
                            if matches!(ota_screen, updates::Screen::Checking) =>
                        {
                            ota_operation_active = false;
                            data.update_available = true;
                            ota_screen = updates::Screen::Available {
                                version: update.version.clone(),
                                size: update.size,
                            };
                            pending_update = Some(update);
                            ota_confirmation_timer.after(config::OTA_CONFIRMATION_TIMEOUT)?;
                        }
                        ota::WorkerEvent::Checked(Err(error))
                            if matches!(ota_screen, updates::Screen::Checking) =>
                        {
                            ota_operation_active = false;
                            ota_screen = updates::Screen::Failed { message: error };
                            if data.wifi_connected {
                                weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                            }
                        }
                        // Reached only by a background check, which leaves the
                        // dashboard alone and speaks through the badge.
                        ota::WorkerEvent::Checked(Ok(ota::CheckResult::UpToDate)) => {
                            ota_operation_active = false;
                            data.update_available = false;
                            log::info!("Background update check: already up to date");
                        }
                        ota::WorkerEvent::Checked(Ok(ota::CheckResult::Available(available))) => {
                            ota_operation_active = false;
                            data.update_available = true;
                            log::info!(
                                "Background update check: v{} available, hold BOOT+PWR to install",
                                available.version
                            );
                        }
                        ota::WorkerEvent::Checked(Err(error)) => {
                            ota_operation_active = false;
                            log::warn!("Background update check failed: {error}");
                        }
                        ota::WorkerEvent::Progress {
                            version,
                            downloaded,
                            total,
                        } if ota_screen.is_visible() => {
                            ota_screen = updates::Screen::Downloading {
                                version,
                                downloaded,
                                total,
                            };
                        }
                        ota::WorkerEvent::Finalizing { version } if ota_screen.is_visible() => {
                            ota_screen = updates::Screen::Finalizing { version };
                        }
                        ota::WorkerEvent::Installed { version } => {
                            ota_operation_active = false;
                            ota_screen = updates::Screen::Restarting { version };
                            force_full_refresh = true;
                            ota_restart_timer.after(config::OTA_RESTART_DELAY)?;
                        }
                        ota::WorkerEvent::InstallFailed(error) if ota_screen.is_visible() => {
                            ota_operation_active = false;
                            ota_screen = updates::Screen::Failed { message: error };
                            if data.wifi_connected {
                                weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                            }
                        }
                        ota::WorkerEvent::InstallFailed(_) => {
                            ota_operation_active = false;
                            if data.wifi_connected {
                                weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                            }
                        }
                        ota::WorkerEvent::Cancelled => {
                            ota_operation_active = false;
                            if ota_screen.is_visible() {
                                ota_screen = updates::Screen::Hidden;
                                force_full_refresh = true;
                            }
                            if data.wifi_connected {
                                weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                            }
                        }
                        _ => {}
                    }
                    render_requested = true;
                }
                AppEvent::OtaCheckDue => {
                    let attended = data
                        .time
                        .is_some_and(|time| notifications::is_attended_hour(time.hour));
                    let charged = data.power_source.is_external()
                        || data.battery.reading.is_none_or(|reading| {
                            reading.percent > config::WARNING_BATTERY_PERCENT
                        });
                    if !data.wifi_connected || ota_screen.is_visible() || ota_operation_active {
                        log::debug!("Background update check skipped: busy or offline");
                    } else if !attended {
                        log::debug!("Background update check skipped: outside attended hours");
                    } else if !charged {
                        log::info!("Background update check skipped: battery too low");
                    } else {
                        match ota_endpoint_store.effective() {
                            Ok(Some(endpoint)) => {
                                if ota_service.check(endpoint.url) {
                                    ota_operation_active = true;
                                    log::info!("Background update check dispatched");
                                } else {
                                    log::warn!("OTA worker unavailable for the background check");
                                }
                            }
                            Ok(None) => log::debug!("Background update check skipped: no endpoint"),
                            Err(error) => {
                                log::warn!("Background update check skipped: {error:#}");
                            }
                        }
                    }
                    // Re-armed here rather than on the result, so a check that
                    // never dispatches still schedules the next one.
                    ota_check_timer.after(config::OTA_CHECK_INTERVAL)?;
                }
                AppEvent::OtaConfirmationExpired => {
                    if matches!(ota_screen, updates::Screen::Available { .. }) {
                        pending_update = None;
                        ota_screen = updates::Screen::Hidden;
                        render_requested = true;
                        force_full_refresh = true;
                        if data.wifi_connected {
                            weather_timer.after(config::IMMEDIATE_EVENT_DELAY)?;
                        }
                    }
                }
                AppEvent::OtaRestartDue => {
                    if matches!(ota_screen, updates::Screen::Restarting { .. }) {
                        ota::restart();
                    }
                }
            }
        }
    }
}

fn synchronize_rtc_from_system_time(
    clock: &mut Clock,
    i2c: &mut I2cBus<'_>,
    utc_offset_seconds: i32,
) -> bool {
    let result = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(anyhow::Error::from)
        .and_then(|elapsed| {
            datetime::DateTime::from_unix_seconds(elapsed.as_secs(), utc_offset_seconds)
        })
        .and_then(|value| {
            clock.set(i2c, value)?;
            Ok(value)
        });
    match result {
        Ok(value) => {
            log::info!("RTC synchronized from NTP: {value}");
            true
        }
        Err(error) => {
            log::warn!("Updating RTC from NTP failed: {error:#}");
            false
        }
    }
}

fn queue_battery_notification(
    schedule: &mut BatteryNotificationSchedule,
    data: &DashboardData,
    event_sender: &events::EventSender,
) {
    let percent = data.battery.reading.map(|reading| reading.percent);
    if let Some(notification) = schedule.on_minute(data.time, percent, data.power_source) {
        let _ = event_sender.send(AppEvent::Notification(notification));
    }
}

fn schedule_clock(timer: &EspTimer<'_>, time: Option<datetime::DateTime>) -> Result<()> {
    let delay = time
        .map(|value| Duration::from_secs(u64::from(60_u8 - value.second)))
        .unwrap_or(config::CLOCK_RETRY_INTERVAL);
    timer.after(delay)?;
    Ok(())
}

/// Reads the sensors into `data`, leaving every field the hardware does not
/// own untouched.
fn refresh_dashboard_data<'d, C, M>(
    data: &mut DashboardData,
    clock: &mut Clock,
    climate_sensor: &Shtc3,
    i2c: &mut I2cBus<'_>,
    battery: &mut Battery<'d, C, M>,
    wifi: &WifiManager,
    weather: Option<Arc<weather::Weather>>,
) where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    // `Clock` carries the last reading forward, so a glitch on the bus cannot
    // blank the display, and it logs whatever it had to do.
    data.time = clock.now(i2c);
    data.climate = climate_sensor
        .read(i2c)
        .inspect_err(|error| log::warn!("Climate read failed: {error:#}"))
        .ok();
    data.battery = battery
        .read()
        .inspect_err(|error| log::warn!("Battery read failed: {error:#}"))
        .unwrap_or_else(|_| BatteryStatus::unavailable());
    data.power_source = power::source();
    data.weather = weather;
    update_wifi_data(data, wifi);
}

fn update_wifi_data(data: &mut DashboardData, wifi: &WifiManager) {
    let status = wifi
        .status()
        .inspect_err(|error| log::warn!("Wi-Fi status failed: {error:#}"))
        .ok();
    data.wifi_connected = status.as_ref().is_some_and(|status| status.connected);
    data.wifi_ssid = status
        .as_ref()
        .and_then(|status| status.configured_ssid.clone());
    data.wifi_signal_dbm = status.and_then(|status| status.signal_dbm);
}

fn handle_command(command: Command, context: &mut CommandContext<'_, '_, '_>) -> bool {
    let refresh = matches!(
        &command,
        Command::TimeSet(_)
            | Command::LanguageSet(_)
            | Command::WifiSet { .. }
            | Command::WifiClear
            | Command::Refresh
    );
    let CommandContext {
        clock,
        i2c,
        wifi,
        audio,
        language_store,
        ota_endpoint,
        language,
        power_log,
    } = context;
    match command {
        Command::Ping => println!("OK PONG"),
        Command::Help => println!("{}", commands::help_text()),
        Command::TimeGet => match clock.read_rtc(i2c) {
            Ok(value) => println!("OK TIME {value}"),
            Err(error) => println!("ERR TIME {error:#}"),
        },
        Command::TimeSet(value) => match clock.set(i2c, value) {
            Ok(()) => println!("OK TIME {value}"),
            Err(error) => println!("ERR TIME {error:#}"),
        },
        Command::TimeCalibrationGet => print_rtc_calibration(clock, i2c),
        Command::TimeCalibrationSet(measured_drift_ppm) => {
            match clock.calibrate(i2c, measured_drift_ppm) {
                Ok(calibration) => println!(
                    "OK TIME CALIBRATION measured_drift_ppm={measured_drift_ppm:.3} steps={} correction_ppm={:.3} residual_ppm={:.3} mode=normal",
                    calibration.offset_steps,
                    calibration.correction_ppm,
                    measured_drift_ppm + calibration.correction_ppm,
                ),
                Err(error) => println!("ERR TIME CALIBRATION {error:#}"),
            }
        }
        Command::LanguageGet => println!("OK LANGUAGE {}", language.code()),
        Command::LanguageSet(value) => match language_store.save(value) {
            Ok(()) => {
                **language = value;
                println!("OK LANGUAGE {}", value.code());
            }
            Err(error) => println!("ERR LANGUAGE {error:#}"),
        },
        Command::WifiSet { ssid, password } => {
            let credentials = WifiCredentials {
                ssid: ssid.clone(),
                password,
            };
            match wifi.set_credentials(credentials) {
                Ok(()) => println!("OK WIFI connected to \"{ssid}\""),
                Err(error) => println!(
                    "ERR WIFI credentials saved for \"{ssid}\", connection failed: {error:#}"
                ),
            }
        }
        Command::WifiClear => match wifi.clear() {
            Ok(()) => println!("OK WIFI cleared"),
            Err(error) => println!("ERR WIFI {error:#}"),
        },
        Command::WifiScan => print_wifi_scan(wifi),
        Command::WifiStatus => print_wifi_status(wifi),
        Command::OtaEndpointGet => print_ota_endpoint(ota_endpoint),
        Command::OtaEndpointSet(endpoint) => match ota_endpoint.set_override(&endpoint) {
            Ok(()) => println!("OK OTA ENDPOINT source=override url=\"{endpoint}\""),
            Err(error) => println!("ERR OTA ENDPOINT {error:#}"),
        },
        Command::OtaEndpointClear => match ota_endpoint.clear_override() {
            Ok(()) => println!("OK OTA ENDPOINT override=cleared"),
            Err(error) => println!("ERR OTA ENDPOINT {error:#}"),
        },
        Command::Status => {
            match clock.read_rtc(i2c) {
                Ok(value) => println!("OK TIME {value}"),
                Err(error) => println!("ERR TIME {error:#}"),
            }
            print_wifi_status(wifi);
        }
        Command::Refresh => println!("OK REFRESH queued"),
        Command::PowerLog => power_log.print(),
        Command::PowerLogClear => {
            power_log.clear();
            println!("OK POWER LOG cleared");
        }
        Command::AudioBeep {
            waveform,
            frequency_hz,
            duration_ms,
            volume_percent,
        } => match audio.play_tone(
            i2c,
            frequency_hz,
            duration_ms,
            volume_percent,
            waveform,
        ) {
            Ok(()) => println!(
                "OK AUDIO tone waveform={} frequency={frequency_hz}Hz duration={duration_ms}ms volume={volume_percent}%",
                waveform.name()
            ),
            Err(error) => println!("ERR AUDIO {error:#}"),
        },
    }
    refresh
}

fn print_rtc_calibration(clock: &Clock, i2c: &mut I2cBus<'_>) {
    match clock.calibration(i2c) {
        Ok(calibration) => println!(
            "OK TIME CALIBRATION steps={} correction_ppm={:.3} mode={}",
            calibration.offset_steps,
            calibration.correction_ppm,
            if calibration.fast_mode {
                "fast"
            } else {
                "normal"
            },
        ),
        Err(error) => println!("ERR TIME CALIBRATION {error:#}"),
    }
}

fn print_wifi_status(wifi: &WifiManager) {
    match wifi.status() {
        Ok(status) => println!(
            "OK WIFI configured={} connected={} ip={} rssi={}",
            status.configured_ssid.as_deref().unwrap_or("none"),
            status.connected,
            status.ip.as_deref().unwrap_or("none"),
            status
                .signal_dbm
                .map(|value| format!("{value}dBm"))
                .unwrap_or_else(|| "none".to_owned())
        ),
        Err(error) => println!("ERR WIFI {error:#}"),
    }
}

fn print_ota_endpoint(store: &ota::EndpointStore) {
    match store.effective() {
        Ok(Some(endpoint)) => println!(
            "OK OTA ENDPOINT source={} url=\"{}\"",
            endpoint.source.label(),
            endpoint.url
        ),
        Ok(None) => println!("OK OTA ENDPOINT source=none url=none"),
        Err(error) => println!("ERR OTA ENDPOINT {error:#}"),
    }
}

fn print_wifi_scan(wifi: &mut WifiManager) {
    match wifi.scan() {
        Ok(networks) => {
            println!("OK WIFI SCAN count={}", networks.len());
            for network in networks {
                let escaped_ssid = network.ssid.replace('\\', "\\\\").replace('"', "\\\"");
                println!(
                    "OK WIFI NETWORK ssid=\"{escaped_ssid}\" rssi={}dBm channel={} auth={}",
                    network.signal_dbm, network.channel, network.auth
                );
            }
        }
        Err(error) => println!("ERR WIFI SCAN {error:#}"),
    }
}
