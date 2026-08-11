mod audio;
mod battery;
mod board;
mod buttons;
mod commands;
mod dashboard;
mod datetime;
mod epaper;
mod events;
mod i2c_bus;
mod ink_stacks;
mod language;
mod location;
mod power;
mod rtc;
mod shtc3;
mod weather;
mod wifi;

use std::borrow::Borrow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use audio::Audio;
use battery::{Battery, BatteryStatus};
use board::BoardPower;
use commands::Command;
use dashboard::{DashboardData, DashboardScreen};
use embedded_graphics::geometry::Size;
use epaper::{Epaper, FRAMEBUFFER_SIZE, HEIGHT, WIDTH};
use esp_idf_hal::adc::oneshot::config::{AdcChannelConfig, Calibration};
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::adc::{attenuation, AdcChannel};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{AnyInputPin, PinDriver, Pull};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::spi::{config, Dma, SpiDeviceDriver, SpiDriverConfig};
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
use rtc::Rtc;
use shtc3::Shtc3;
use weather::{WeatherService, FAILURE_RETRY_INTERVAL, REFRESH_INTERVAL};
use wifi::{WifiCredentials, WifiManager};

const FULL_REFRESH_AFTER_PARTIALS: u8 = 30;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();
    log::info!("Starting modular Rust e-paper dashboard");
    power::configure_dynamic_frequency_scaling()?;

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
    let rtc = Rtc::new();
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

    let spi_config = config::Config::new()
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
    let mut epaper = Epaper::new(spi, epaper_power, busy, reset, dc);

    let mut audio = Audio::new(pins.gpio46)?;

    let system_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let language_store = LanguageStore::new(nvs.clone())?;
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

    let mut weather = WeatherService::start(nvs.clone(), event_sender.clone())?;
    let mut wifi = WifiManager::new(peripherals.modem, system_loop, nvs)?;
    match wifi.connect_saved() {
        Ok(true) => log::info!("Connected using saved Wi-Fi credentials"),
        Ok(false) => log::info!("No saved Wi-Fi; configure it over USB"),
        Err(error) => log::warn!("Saved Wi-Fi connection failed: {error:#}"),
    }

    commands::start_usb_console(event_sender.clone())?;
    buttons::start(pins.gpio0, pins.gpio18, event_sender)?;
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
    let mut data = collect_dashboard_data(
        &rtc,
        &climate_sensor,
        &mut i2c,
        &mut battery,
        &wifi,
        weather.latest(),
        power::usb_host_connected(),
    );
    data.language = language;
    schedule_clock(&clock_timer, data.time)?;
    weather_timer.after(Duration::from_millis(1))?;

    loop {
        if render_requested && !audio.is_pcm_active() {
            dashboard::render(&mut next_frame, &data, screen);
            let changed = displayed_frame.changed_regions(&next_frame);
            let needs_full = force_full_refresh
                || !panel_initialized
                || partial_refreshes >= FULL_REFRESH_AFTER_PARTIALS;
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
                    log::info!(
                        "Dashboard refreshed: time={}, temp={}, humidity={}, wifi={}, rssi={}, weather={}, battery={}",
                        data.time
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unset".into()),
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
                                if data.battery.usb_powered { "USB" } else { "battery" }
                            ),
                            None if data.battery.usb_powered => "not detected (USB)".into(),
                            None => "not detected".into(),
                        },
                    );
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
            match event {
                AppEvent::Command(Ok(command)) => {
                    let clock_changed = matches!(&command, Command::TimeSet(_));
                    let explicit_refresh = matches!(&command, Command::Refresh);
                    let language_changed = matches!(&command, Command::LanguageSet(_));
                    match command {
                        Command::NextScreen => {
                            screen = screen.next();
                            render_requested = true;
                            force_full_refresh = true;
                        }
                        Command::PreviousScreen => {
                            screen = screen.previous();
                            render_requested = true;
                            force_full_refresh = true;
                        }
                        command => {
                            if handle_command(
                                command,
                                &rtc,
                                &mut i2c,
                                &mut wifi,
                                &mut audio,
                                &language_store,
                                &mut data.language,
                            ) {
                                refresh_dashboard_data(
                                    &mut data,
                                    &rtc,
                                    &climate_sensor,
                                    &mut i2c,
                                    &mut battery,
                                    &wifi,
                                    weather.latest(),
                                );
                                render_requested = true;
                            }
                        }
                    }
                    if explicit_refresh {
                        force_full_refresh = true;
                    }
                    if language_changed {
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
                        &rtc,
                        &climate_sensor,
                        &mut i2c,
                        &mut battery,
                        &wifi,
                        weather.latest(),
                    );
                    schedule_clock(&clock_timer, data.time)?;
                    render_requested = true;
                }
                AppEvent::WeatherDue => {
                    if wifi.is_connected() && weather.request() {
                        log::debug!("Weather refresh dispatched");
                    } else {
                        weather_timer.after(FAILURE_RETRY_INTERVAL)?;
                    }
                }
                AppEvent::WeatherCompleted(update) => {
                    let succeeded = update.is_ok();
                    if weather.complete(update) {
                        data.weather = weather.latest();
                        render_requested = true;
                    }
                    weather_timer.after(if succeeded {
                        REFRESH_INTERVAL
                    } else {
                        FAILURE_RETRY_INTERVAL
                    })?;
                }
                AppEvent::WifiChanged => {
                    let was_connected = data.wifi_connected;
                    update_wifi_data(&mut data, &wifi);
                    render_requested = true;
                    if wifi.is_connected() {
                        reconnect_timer.cancel()?;
                        if !was_connected {
                            weather_timer.after(Duration::from_millis(1))?;
                        }
                    } else {
                        reconnect_timer.after(RECONNECT_INTERVAL)?;
                    }
                }
                AppEvent::ReconnectDue => match wifi.reconnect_saved() {
                    Ok(true) => reconnect_timer.after(RECONNECT_INTERVAL)?,
                    Ok(false) => {}
                    Err(error) => {
                        log::warn!("Wi-Fi reconnect failed: {error:#}");
                        reconnect_timer.after(RECONNECT_INTERVAL)?;
                    }
                },
            }
        }
    }
}

fn schedule_clock(timer: &EspTimer<'_>, time: Option<datetime::DateTime>) -> Result<()> {
    let delay = time
        .map(|value| Duration::from_secs(u64::from(60_u8 - value.second)))
        .unwrap_or_else(|| Duration::from_secs(5));
    timer.after(delay)?;
    Ok(())
}

fn refresh_dashboard_data<'d, C, M>(
    data: &mut DashboardData,
    rtc: &Rtc,
    climate_sensor: &Shtc3,
    i2c: &mut I2cBus<'_>,
    battery: &mut Battery<'d, C, M>,
    wifi: &WifiManager,
    weather: Option<std::sync::Arc<weather::Weather>>,
) where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    let language = data.language;
    *data = collect_dashboard_data(
        rtc,
        climate_sensor,
        i2c,
        battery,
        wifi,
        weather,
        power::usb_host_connected(),
    );
    data.language = language;
}

fn update_wifi_data(data: &mut DashboardData, wifi: &WifiManager) {
    let status = wifi
        .status()
        .inspect_err(|error| log::warn!("Wi-Fi event status failed: {error:#}"))
        .ok();
    data.wifi_connected = status.as_ref().is_some_and(|status| status.connected);
    data.wifi_ssid = status
        .as_ref()
        .and_then(|status| status.configured_ssid.clone());
    data.wifi_signal_dbm = status.and_then(|status| status.signal_dbm);
}

fn collect_dashboard_data<'d, C, M>(
    rtc: &Rtc,
    climate_sensor: &Shtc3,
    i2c: &mut I2cBus<'_>,
    battery: &mut Battery<'d, C, M>,
    wifi: &WifiManager,
    weather: Option<std::sync::Arc<weather::Weather>>,
    usb_powered: bool,
) -> DashboardData
where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    let time = rtc
        .read(i2c)
        .inspect_err(|error| log::warn!("RTC read failed: {error:#}"))
        .ok();
    let climate = climate_sensor
        .read(i2c)
        .inspect_err(|error| log::warn!("Climate read failed: {error:#}"))
        .ok();
    let battery = battery
        .read(usb_powered)
        .inspect_err(|error| log::warn!("Battery read failed: {error:#}"))
        .unwrap_or_else(|_| BatteryStatus::unavailable(usb_powered));
    let wifi_status = wifi
        .status()
        .inspect_err(|error| log::warn!("Wi-Fi status failed: {error:#}"))
        .ok();
    let wifi_connected = wifi_status
        .as_ref()
        .map(|status| status.connected)
        .unwrap_or(false);
    let wifi_ssid = wifi_status
        .as_ref()
        .and_then(|status| status.configured_ssid.clone());
    let wifi_signal_dbm = wifi_status.as_ref().and_then(|status| status.signal_dbm);
    DashboardData {
        time,
        climate,
        battery,
        wifi_connected,
        wifi_ssid,
        wifi_signal_dbm,
        weather,
        language: Language::default(),
    }
}

fn handle_command(
    command: Command,
    rtc: &Rtc,
    i2c: &mut I2cBus<'_>,
    wifi: &mut WifiManager,
    audio: &mut Audio<'_>,
    language_store: &LanguageStore,
    language: &mut Language,
) -> bool {
    let refresh = matches!(
        &command,
        Command::TimeSet(_)
            | Command::LanguageSet(_)
            | Command::WifiSet { .. }
            | Command::WifiClear
            | Command::Refresh
    );
    match command {
        Command::NextScreen | Command::PreviousScreen => unreachable!("handled by main loop"),
        Command::Ping => println!("OK PONG"),
        Command::Help => println!("{}", commands::help_text()),
        Command::TimeGet => match rtc.read(i2c) {
            Ok(value) => println!("OK TIME {value}"),
            Err(error) => println!("ERR TIME {error:#}"),
        },
        Command::TimeSet(value) => match rtc.set(i2c, value) {
            Ok(()) => println!("OK TIME {value}"),
            Err(error) => println!("ERR TIME {error:#}"),
        },
        Command::TimeCalibrationGet => print_rtc_calibration(rtc, i2c),
        Command::TimeCalibrationSet(measured_drift_ppm) => {
            match rtc.calibrate(i2c, measured_drift_ppm) {
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
                *language = value;
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
        Command::Status => {
            match rtc.read(i2c) {
                Ok(value) => println!("OK TIME {value}"),
                Err(error) => println!("ERR TIME {error:#}"),
            }
            print_wifi_status(wifi);
        }
        Command::Refresh => println!("OK REFRESH queued"),
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
        Command::AudioPcmBegin {
            byte_count,
            sample_rate_hz,
            volume_percent,
        } => match audio.begin_pcm(i2c, byte_count, sample_rate_hz, volume_percent) {
            Ok(()) => println!(
                "OK AUDIO PCM READY bytes={byte_count} rate={sample_rate_hz}Hz volume={volume_percent}%"
            ),
            Err(error) => println!("ERR AUDIO PCM {error:#}"),
        },
        Command::AudioPcmData(data) => {
            match audio.write_pcm(i2c, &data) {
                Ok(byte_count) => println!("OK AUDIO PCM CHUNK bytes={byte_count}"),
                Err(error) => println!("ERR AUDIO PCM {error:#}"),
            }
        }
        Command::AudioPcmEnd => match audio.finish_pcm(i2c) {
            Ok(byte_count) => println!("OK AUDIO PCM played_bytes={byte_count}"),
            Err(error) => println!("ERR AUDIO PCM {error:#}"),
        },
    }
    refresh
}

fn print_rtc_calibration(rtc: &Rtc, i2c: &mut I2cBus<'_>) {
    match rtc.calibration(i2c) {
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
