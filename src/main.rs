mod audio;
mod battery;
mod board;
mod commands;
mod dashboard;
mod datetime;
mod epaper;
mod i2c_bus;
mod power;
mod rtc;
mod shtc3;
mod wifi;

use std::borrow::Borrow;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use audio::Audio;
use battery::Battery;
use board::BoardPower;
use commands::Command;
use dashboard::{DashboardData, Framebuffer};
use epaper::{Epaper, FRAMEBUFFER_SIZE};
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
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use i2c_bus::I2cBus;
use rtc::Rtc;
use shtc3::Shtc3;
use wifi::{WifiCredentials, WifiManager};

const FULL_REFRESH_AFTER_PARTIALS: u8 = 30;

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
    let mut wifi = WifiManager::new(peripherals.modem, system_loop, nvs)?;
    match wifi.connect_saved() {
        Ok(true) => log::info!("Connected using saved Wi-Fi credentials"),
        Ok(false) => log::info!("No saved Wi-Fi; configure it over USB"),
        Err(error) => log::warn!("Saved Wi-Fi connection failed: {error:#}"),
    }

    let commands = commands::start_usb_console()?;
    println!("READY Rust e-paper dashboard");
    println!("{}", commands::help_text());

    let mut framebuffer = Framebuffer::new();
    let mut panel_initialized = false;
    let mut partial_refreshes = 0_u8;
    let mut force_refresh = true;
    let mut last_minute = None;
    let mut next_clock_poll = Instant::now();
    let mut next_wifi_poll = Instant::now();

    loop {
        if Instant::now() >= next_clock_poll {
            match rtc.read(&mut i2c) {
                Ok(now) => {
                    let minute = (now.year, now.month, now.day, now.hour, now.minute);
                    if last_minute != Some(minute) {
                        last_minute = Some(minute);
                        force_refresh = true;
                    }
                    // Re-read just after the next RTC minute boundary instead
                    // of waking the CPU and I2C bus once per second.
                    next_clock_poll =
                        Instant::now() + Duration::from_secs(u64::from(60_u8 - now.second));
                }
                Err(error) => {
                    log::warn!("RTC polling failed: {error:#}");
                    next_clock_poll = Instant::now() + Duration::from_secs(5);
                }
            }
        }

        if Instant::now() >= next_wifi_poll {
            match wifi.poll() {
                Ok(changed) => force_refresh |= changed,
                Err(error) => log::warn!("Wi-Fi polling failed: {error:#}"),
            }
            // Share the RTC deadline instead of waking the CPU separately for
            // link observation. RTC failures already have a bounded retry.
            next_wifi_poll = next_clock_poll;
        }

        // E-paper refreshes take seconds. Keep consuming the bounded USB/PCM
        // queue until the stream ends so the host never stalls behind a redraw.
        if force_refresh && !audio.is_pcm_active() {
            let data = collect_dashboard_data(&rtc, &climate_sensor, &mut i2c, &mut battery, &wifi);
            dashboard::render(&mut framebuffer, &data);

            let needs_full = !panel_initialized || partial_refreshes >= FULL_REFRESH_AFTER_PARTIALS;
            let refresh_result = if needs_full {
                epaper
                    .init_full()
                    .and_then(|()| epaper.display_base(framebuffer.bytes()))
                    .and_then(|()| epaper.init_partial())
            } else {
                epaper.display_partial(framebuffer.bytes())
            };

            match refresh_result {
                Ok(()) => {
                    panel_initialized = true;
                    partial_refreshes = if needs_full { 0 } else { partial_refreshes + 1 };
                    log::info!(
                        "Dashboard refreshed: time={}, temp={}, humidity={}, wifi={}, battery={}",
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
                        data.battery
                            .map(|value| format!("{}% ({:.2} V)", value.percent, value.voltage_v))
                            .unwrap_or_else(|| "not detected".into()),
                    );
                }
                Err(error) => log::error!("Dashboard refresh failed: {error:#}"),
            }
            force_refresh = false;
        }

        // Block until either USB work arrives or the RTC needs attention.
        // The channel wakes immediately for commands and PCM chunks, while an
        // idle dashboard now has no periodic 10 Hz application wake-up.
        let next_poll = next_clock_poll.min(next_wifi_poll);
        let wait = next_poll.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(Ok(command)) => {
                let clock_changed = matches!(&command, Command::TimeSet(_));
                force_refresh |= handle_command(command, &rtc, &mut i2c, &mut wifi, &mut audio);
                if clock_changed {
                    next_clock_poll = Instant::now();
                }
            }
            Ok(Err(error)) => println!("ERR {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("USB command task stopped"),
        }
    }
}

fn collect_dashboard_data<'d, C, M>(
    rtc: &Rtc,
    climate_sensor: &Shtc3,
    i2c: &mut I2cBus<'_>,
    battery: &mut Battery<'d, C, M>,
    wifi: &WifiManager,
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
        .read()
        .inspect_err(|error| log::warn!("Battery read failed: {error:#}"))
        .ok()
        .flatten();
    let wifi_status = wifi
        .status()
        .inspect_err(|error| log::warn!("Wi-Fi status failed: {error:#}"))
        .ok();
    DashboardData {
        time,
        climate,
        battery,
        wifi_connected: wifi_status
            .as_ref()
            .map(|status| status.connected)
            .unwrap_or(false),
        wifi_ssid: wifi_status.and_then(|status| status.configured_ssid),
    }
}

fn handle_command(
    command: Command,
    rtc: &Rtc,
    i2c: &mut I2cBus<'_>,
    wifi: &mut WifiManager,
    audio: &mut Audio<'_>,
) -> bool {
    let refresh = matches!(
        &command,
        Command::TimeSet(_) | Command::WifiSet { .. } | Command::WifiClear | Command::Refresh
    );
    match command {
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
            "OK WIFI configured={} connected={} ip={}",
            status.configured_ssid.as_deref().unwrap_or("none"),
            status.connected,
            status.ip.as_deref().unwrap_or("none")
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
