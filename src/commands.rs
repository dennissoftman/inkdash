use std::io::{self, ErrorKind, Read};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::sys::{self, EspError};

use crate::audio::{Waveform, DEFAULT_DURATION_MS, DEFAULT_FREQUENCY_HZ, DEFAULT_VOLUME_PERCENT};
use crate::config;
use crate::dashboard::updates;
use crate::datetime::DateTime;
use crate::demo;
use crate::events::{AppEvent, EventSender};
use crate::language::Language;
use crate::power::PowerSource;
use crate::shtc3::ClimateReading;
use crate::weather::WeatherKind;

#[derive(Clone, Debug)]
pub enum Command {
    Ping,
    Help,
    TimeGet,
    TimeSet(DateTime),
    TimeCalibrationGet,
    TimeCalibrationSet(f32),
    LanguageGet,
    LanguageSet(Language),
    WifiSet {
        ssid: String,
        password: String,
    },
    WifiClear,
    WifiScan,
    WifiStatus,
    OtaEndpointGet,
    OtaEndpointSet(String),
    OtaEndpointClear,
    Status,
    Refresh,
    AudioBeep {
        waveform: Waveform,
        frequency_hz: u16,
        duration_ms: u16,
        volume_percent: u8,
    },
    Demo(demo::Action),
}

pub type CommandMessage = Result<Command, String>;

pub fn start_usb_console(sender: EventSender) -> Result<()> {
    let mut config = sys::usb_serial_jtag_driver_config_t {
        tx_buffer_size: 1024,
        rx_buffer_size: 1024,
    };
    EspError::convert(unsafe { sys::usb_serial_jtag_driver_install(&mut config) })
        .context("installing blocking USB Serial/JTAG driver")?;
    unsafe { usb_serial_jtag_vfs_use_driver() };

    let console_sender = sender.clone();
    thread::Builder::new()
        .name("usb-console".into())
        .stack_size(config::INPUT_TASK_STACK_SIZE)
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let mut read_buffer = [0_u8; 1024];
            let mut line = Vec::with_capacity(128);
            let mut overflowed = false;

            'read: loop {
                match input.read(&mut read_buffer) {
                    Ok(0) => {
                        let _ = console_sender
                            .send(AppEvent::Command(Err("USB input stream closed".into())));
                        break;
                    }
                    Ok(length) => {
                        let mut offset = 0;
                        while offset < length {
                            let byte = read_buffer[offset];
                            offset += 1;
                            match byte {
                                b'\r' | b'\n' => {
                                    if overflowed {
                                        if console_sender
                                            .send(AppEvent::Command(Err(
                                                "command exceeds 256 bytes".into(),
                                            )))
                                            .is_err()
                                        {
                                            break 'read;
                                        }
                                    } else if !line.is_empty() {
                                        let parsed = std::str::from_utf8(&line)
                                            .map_err(|_| "command is not valid UTF-8".to_owned())
                                            .and_then(|text| {
                                                parse(text).map_err(|error| format!("{error:#}"))
                                            });
                                        if console_sender.send(AppEvent::Command(parsed)).is_err() {
                                            break 'read;
                                        }
                                    }
                                    line.clear();
                                    overflowed = false;
                                }
                                0x08 | 0x7f if !overflowed => {
                                    line.pop();
                                }
                                byte if !overflowed && line.len() < 256 => line.push(byte),
                                _ => overflowed = true,
                            }
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = console_sender
                            .send(AppEvent::Command(Err(format!("USB input error: {error}"))));
                        break;
                    }
                }
            }
        })
        .context("starting USB command thread")?;
    Ok(())
}

unsafe extern "C" {
    fn usb_serial_jtag_vfs_use_driver();
}

pub fn help_text() -> &'static str {
    "Commands:\n\
     PING\n\
     TIME GET\n\
     TIME SET YYYY-MM-DD HH:MM:SS\n\
     TIME CALIBRATION GET\n\
     TIME CALIBRATION SET <measured_drift_ppm>\n\
     LANGUAGE GET\n\
     LANGUAGE SET <en|ru>\n\
     WIFI SET \"ssid\" \"password\"\n\
     WIFI SCAN\n\
     WIFI STATUS\n\
     WIFI CLEAR\n\
     OTA ENDPOINT GET\n\
     OTA ENDPOINT SET \"https://example.com/ota-manifest.json\"\n\
     OTA ENDPOINT CLEAR\n\
     STATUS\n\
     REFRESH\n\
     AUDIO BEEP [frequency_hz] [duration_ms] [volume_percent]\n\
     AUDIO TONE <sine|square|triangle> [frequency_hz] [duration_ms] [volume_percent]\n\
     DEMO UPDATE CHECKING|UPTODATE\n\
     DEMO UPDATE AVAILABLE <version> <bytes>\n\
     DEMO UPDATE DOWNLOADING <version> <downloaded> <total>\n\
     DEMO UPDATE FINALIZING|RESTARTING <version>\n\
     DEMO UPDATE FAILED \"message\"\n\
     DEMO UPDATE INSTALL [total_bytes] [step_ms]\n\
     DEMO DATA BATTERY <0-100|none>\n\
     DEMO DATA POWER <pwr|bat>\n\
     DEMO DATA WIFI <off|0-3> [\"ssid\"]\n\
     DEMO DATA WEATHER <sunny|cloudy|fog|rain|snow|storm|none> [temperature_c]\n\
     DEMO DATA INDOOR <temperature_c> <humidity_percent> | none\n\
     DEMO DATA BADGE <on|off>\n\
     DEMO DATA CLEAR\n\
     DEMO REFRESH <auto|full|partial>\n\
     DEMO STATUS\n\
     DEMO OFF\n\
     HELP\n\
     \n\
     Demo mode draws screens on demand; TIME SET moves the clock artwork's\n\
     time of day. Nothing in DEMO touches the network, the flash, or a real\n\
     update, and a reset ends it."
}

fn parse(line: &str) -> Result<Command> {
    let words = tokenize(line)?;
    let normalized: Vec<String> = words.iter().map(|word| word.to_ascii_uppercase()).collect();

    match normalized.as_slice() {
        [command] if command == "PING" => Ok(Command::Ping),
        [command] if command == "HELP" => Ok(Command::Help),
        [command] if command == "STATUS" => Ok(Command::Status),
        [command] if command == "REFRESH" => Ok(Command::Refresh),
        [group, action, rest @ ..] if group == "AUDIO" && action == "BEEP" => {
            parse_tone(Waveform::Sine, rest)
        }
        [group, action, waveform, rest @ ..] if group == "AUDIO" && action == "TONE" => {
            parse_tone(Waveform::parse(waveform)?, rest)
        }
        [group, action] if group == "TIME" && action == "GET" => Ok(Command::TimeGet),
        [group, calibration, action]
            if group == "TIME" && calibration == "CALIBRATION" && action == "GET" =>
        {
            Ok(Command::TimeCalibrationGet)
        }
        [group, calibration, action, drift]
            if group == "TIME" && calibration == "CALIBRATION" && action == "SET" =>
        {
            Ok(Command::TimeCalibrationSet(parse_number(
                drift,
                "measured RTC drift in ppm",
            )?))
        }
        [group, action, ..] if group == "TIME" && action == "SET" => {
            let value = match words.len() {
                3 => words[2].clone(),
                4 => format!("{} {}", words[2], words[3]),
                _ => bail!("usage: TIME SET YYYY-MM-DD HH:MM:SS"),
            };
            Ok(Command::TimeSet(DateTime::parse(&value)?))
        }
        [group, action] if group == "LANGUAGE" && action == "GET" => Ok(Command::LanguageGet),
        [group, action, value] if group == "LANGUAGE" && action == "SET" => {
            let language =
                Language::parse(value).context("language must be en/english or ru/russian")?;
            Ok(Command::LanguageSet(language))
        }
        [group, action] if group == "WIFI" && action == "STATUS" => Ok(Command::WifiStatus),
        [group, action] if group == "WIFI" && action == "SCAN" => Ok(Command::WifiScan),
        [group, action] if group == "WIFI" && action == "CLEAR" => Ok(Command::WifiClear),
        [group, endpoint, action]
            if group == "OTA" && endpoint == "ENDPOINT" && action == "GET" =>
        {
            Ok(Command::OtaEndpointGet)
        }
        [group, endpoint, action]
            if group == "OTA" && endpoint == "ENDPOINT" && action == "CLEAR" =>
        {
            Ok(Command::OtaEndpointClear)
        }
        [group, endpoint, action, _]
            if group == "OTA" && endpoint == "ENDPOINT" && action == "SET" =>
        {
            Ok(Command::OtaEndpointSet(words[3].clone()))
        }
        [group, ..] if group == "DEMO" => parse_demo(&normalized[1..], &words[1..]),
        [group, action, _, _] if group == "WIFI" && action == "SET" => {
            let ssid = words[2].clone();
            let password = words[3].clone();
            if ssid.is_empty() || ssid.len() > 32 {
                bail!("SSID must contain 1 to 32 bytes");
            }
            if password.len() > 64 {
                bail!("password must contain at most 64 bytes");
            }
            Ok(Command::WifiSet { ssid, password })
        }
        _ => bail!("unknown command; type HELP"),
    }
}

fn parse_tone(waveform: Waveform, values: &[String]) -> Result<Command> {
    if values.len() > 3 {
        bail!("tone accepts frequency, duration, and volume only");
    }
    let frequency_hz = parse_optional(values.first(), DEFAULT_FREQUENCY_HZ, "frequency")?;
    let duration_ms = parse_optional(values.get(1), DEFAULT_DURATION_MS, "duration")?;
    let volume_percent = parse_optional(values.get(2), DEFAULT_VOLUME_PERCENT, "volume")?;
    if !(100..=5_000).contains(&frequency_hz) {
        bail!("frequency must be between 100 and 5000 Hz");
    }
    if !(50..=5_000).contains(&duration_ms) {
        bail!("duration must be between 50 and 5000 ms");
    }
    if !(1..=80).contains(&volume_percent) {
        bail!("volume must be between 1 and 80 percent");
    }
    Ok(Command::AudioBeep {
        waveform,
        frequency_hz,
        duration_ms,
        volume_percent,
    })
}

/// `normalized` is upper-cased for matching; `words` is the same slice with the
/// case the operator typed, which is what a version, an SSID, or a message needs.
fn parse_demo(normalized: &[String], words: &[String]) -> Result<Command> {
    let action = match normalized {
        [action] if action == "OFF" => demo::Action::Off,
        [action] if action == "STATUS" => demo::Action::Status,
        [action, mode] if action == "REFRESH" => demo::Action::Refresh(match mode.as_str() {
            "AUTO" => demo::RefreshMode::Auto,
            "FULL" => demo::RefreshMode::Full,
            "PARTIAL" => demo::RefreshMode::Partial,
            _ => bail!("refresh must be auto, full, or partial"),
        }),
        [group, ..] if group == "UPDATE" => parse_demo_update(&normalized[1..], &words[1..])?,
        [group, ..] if group == "DATA" => parse_demo_data(&normalized[1..], &words[1..])?,
        _ => bail!("unknown DEMO command; type HELP"),
    };
    Ok(Command::Demo(action))
}

fn parse_demo_update(normalized: &[String], words: &[String]) -> Result<demo::Action> {
    let screen = match normalized {
        [state] if state == "CHECKING" => updates::Screen::Checking,
        [state] if state == "UPTODATE" => updates::Screen::UpToDate,
        [state, _, size] if state == "AVAILABLE" => updates::Screen::Available {
            version: demo_version(&words[1])?,
            size: parse_number(size, "size in bytes")?,
        },
        [state, _, downloaded, total] if state == "DOWNLOADING" => {
            let total: usize = parse_number(total, "total in bytes")?;
            let downloaded: usize = parse_number(downloaded, "downloaded bytes")?;
            if total == 0 {
                bail!("total must be more than zero bytes");
            }
            if downloaded > total {
                bail!("downloaded must not exceed the total");
            }
            updates::Screen::Downloading {
                version: demo_version(&words[1])?,
                downloaded,
                total,
            }
        }
        [state, _] if state == "FINALIZING" => updates::Screen::Finalizing {
            version: demo_version(&words[1])?,
        },
        [state, _] if state == "RESTARTING" => updates::Screen::Restarting {
            version: demo_version(&words[1])?,
        },
        [state, _] if state == "FAILED" => updates::Screen::Failed {
            message: demo_message(&words[1])?,
        },
        [state, rest @ ..] if state == "INSTALL" => {
            if rest.len() > 2 {
                bail!("usage: DEMO UPDATE INSTALL [total_bytes] [step_ms]");
            }
            let total = rest
                .first()
                .map(|value| parse_number(value, "total in bytes"))
                .transpose()?;
            let step_ms = rest
                .get(1)
                .map(|value| parse_number(value, "step in milliseconds"))
                .transpose()?;
            return demo::replay(total, step_ms).map_err(|error| anyhow!("{error}"));
        }
        _ => bail!("unknown DEMO UPDATE screen; type HELP"),
    };
    Ok(demo::Action::Screen(screen))
}

fn parse_demo_data(normalized: &[String], words: &[String]) -> Result<demo::Action> {
    let over = match normalized {
        [field] if field == "CLEAR" => demo::DataOverride::Clear,
        [field, value] if field == "BATTERY" => demo::DataOverride::Battery(match value.as_str() {
            "NONE" => None,
            value => {
                let percent: u8 = parse_number(value, "battery percentage")?;
                if percent > 100 {
                    bail!("battery percentage must be 0 to 100");
                }
                Some(percent)
            }
        }),
        [field, value] if field == "POWER" => demo::DataOverride::Power(match value.as_str() {
            "PWR" | "EXTERNAL" => PowerSource::ExternalPower,
            "BAT" | "BATTERY" => PowerSource::Battery,
            _ => bail!("power must be pwr or bat"),
        }),
        [field, value, rest @ ..] if field == "WIFI" => {
            if rest.len() > 1 {
                bail!("usage: DEMO DATA WIFI <off|0-3> [\"ssid\"]");
            }
            let bars = match value.as_str() {
                "OFF" => 0,
                value => {
                    let bars: u8 = parse_number(value, "signal bars")?;
                    if bars > 3 {
                        bail!("signal bars must be 0 to 3");
                    }
                    bars
                }
            };
            // A named network is what the badge normally shows, so an unnamed
            // request gets a placeholder rather than the "no Wi-Fi" label.
            let ssid = match (bars, words.get(2)) {
                (0, _) => None,
                (_, None) => Some("demo-net".to_owned()),
                (_, Some(ssid)) => {
                    if ssid.is_empty() || ssid.len() > 32 {
                        bail!("SSID must contain 1 to 32 bytes");
                    }
                    Some(ssid.clone())
                }
            };
            demo::DataOverride::Wifi { bars, ssid }
        }
        [field, value, rest @ ..] if field == "WEATHER" => {
            if rest.len() > 1 {
                bail!("usage: DEMO DATA WEATHER <condition|none> [temperature_c]");
            }
            if value == "NONE" {
                demo::DataOverride::Weather(None)
            } else {
                let kind = WeatherKind::parse(value)
                    .context("condition must be sunny, cloudy, fog, rain, snow, or storm")?;
                let temperature_c = rest
                    .first()
                    .map(|value| parse_decimal(value, "temperature"))
                    .transpose()?
                    .unwrap_or(18.0);
                demo::DataOverride::Weather(Some((kind, temperature_c)))
            }
        }
        [field, value] if field == "INDOOR" && value == "NONE" => demo::DataOverride::Indoor(None),
        [field, temperature, humidity] if field == "INDOOR" => {
            let humidity_percent = parse_decimal(humidity, "humidity")?;
            if !(0.0..=100.0).contains(&humidity_percent) {
                bail!("humidity must be 0 to 100");
            }
            demo::DataOverride::Indoor(Some(ClimateReading {
                temperature_c: parse_decimal(temperature, "temperature")?,
                humidity_percent,
            }))
        }
        [field, value] if field == "BADGE" => demo::DataOverride::Badge(match value.as_str() {
            "ON" => true,
            "OFF" => false,
            _ => bail!("badge must be on or off"),
        }),
        _ => bail!("unknown DEMO DATA field; type HELP"),
    };
    Ok(demo::Action::Data(over))
}

fn demo_version(value: &str) -> Result<String> {
    if value.is_empty() || value.len() > 16 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!("version must be 1 to 16 printable characters");
    }
    Ok(value.to_owned())
}

fn demo_message(value: &str) -> Result<String> {
    // Five lines of twenty-nine characters is all the failure screen wraps to.
    if value.is_empty() || value.len() > 145 {
        bail!("message must be 1 to 145 characters");
    }
    Ok(value.to_owned())
}

fn parse_decimal(value: &str, label: &str) -> Result<f32> {
    value
        .parse()
        .map_err(|error| anyhow!("{label} must be a number: {error}"))
}

fn parse_number<T>(value: &str, label: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| anyhow!("{label} must be a whole number: {error}"))
}

fn parse_optional<T>(value: Option<&String>, default: T, label: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value.map_or(Ok(default), |text| {
        text.parse()
            .map_err(|error| anyhow!("{label} must be a whole number: {error}"))
    })
}

fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut token_started = false;

    for character in line.trim().chars() {
        if escaped {
            current.push(character);
            escaped = false;
            token_started = true;
        } else if character == '\\' {
            escaped = true;
            token_started = true;
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
            token_started = true;
        } else if character.is_whitespace() {
            if token_started {
                words.push(std::mem::take(&mut current));
                token_started = false;
            }
        } else {
            current.push(character);
            token_started = true;
        }
    }

    if escaped {
        bail!("command ends with an incomplete escape");
    }
    if quote.is_some() {
        bail!("command contains an unterminated quote");
    }
    if token_started {
        words.push(current);
    }
    Ok(words)
}
