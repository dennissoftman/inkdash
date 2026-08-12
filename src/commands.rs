use std::io::{self, ErrorKind, Read};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_svc::sys::{self, EspError};

use crate::audio::{
    Waveform, DEFAULT_DURATION_MS, DEFAULT_FREQUENCY_HZ, DEFAULT_VOLUME_PERCENT,
    SUPPORTED_SAMPLE_RATES,
};
use crate::config;
use crate::datetime::DateTime;
use crate::events::{AppEvent, EventSender};
use crate::language::Language;

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
    AudioPcmBegin {
        byte_count: usize,
        sample_rate_hz: u32,
        volume_percent: u8,
    },
    AudioPcmData(Vec<u8>),
    AudioPcmEnd,
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
     AUDIO PCM BEGIN <byte_count> <sample_rate_hz> <volume_percent>\n\
     AUDIO PCM DATA <base64_chunk>\n\
     AUDIO PCM END\n\
     HELP"
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
        [group, pcm, begin, byte_count, sample_rate, volume]
            if group == "AUDIO" && pcm == "PCM" && begin == "BEGIN" =>
        {
            let byte_count: usize = parse_number(byte_count, "byte count")?;
            let sample_rate_hz: u32 = parse_number(sample_rate, "sample rate")?;
            let volume_percent: u8 = parse_number(volume, "volume")?;
            if byte_count == 0 || byte_count % 2 != 0 {
                bail!("PCM byte count must be a positive even number");
            }
            if !SUPPORTED_SAMPLE_RATES.contains(&sample_rate_hz) {
                bail!("sample rate must be one of 8000, 16000, 24000, 32000, 44100, 48000 Hz");
            }
            if byte_count > sample_rate_hz as usize * 2 * 120 {
                bail!("PCM stream must not exceed 120 seconds");
            }
            if !(1..=80).contains(&volume_percent) {
                bail!("volume must be between 1 and 80 percent");
            }
            Ok(Command::AudioPcmBegin {
                byte_count,
                sample_rate_hz,
                volume_percent,
            })
        }
        [group, pcm, data, encoded] if group == "AUDIO" && pcm == "PCM" && data == "DATA" => {
            Ok(Command::AudioPcmData(decode_base64(encoded)?))
        }
        [group, pcm, end] if group == "AUDIO" && pcm == "PCM" && end == "END" => {
            Ok(Command::AudioPcmEnd)
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

fn decode_base64(encoded: &str) -> Result<Vec<u8>> {
    let bytes = encoded.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        bail!("PCM chunk must be non-empty padded Base64");
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    for (group_index, group) in bytes.chunks_exact(4).enumerate() {
        let is_last = group_index + 1 == bytes.len() / 4;
        let first = base64_value(group[0])?;
        let second = base64_value(group[1])?;
        decoded.push((first << 2) | (second >> 4));

        if group[2] == b'=' {
            if !is_last || group[3] != b'=' || second & 0x0f != 0 {
                bail!("invalid Base64 padding in PCM chunk");
            }
            continue;
        }

        let third = base64_value(group[2])?;
        decoded.push((second << 4) | (third >> 2));
        if group[3] == b'=' {
            if !is_last || third & 0x03 != 0 {
                bail!("invalid Base64 padding in PCM chunk");
            }
            continue;
        }

        let fourth = base64_value(group[3])?;
        decoded.push((third << 6) | fourth);
    }
    Ok(decoded)
}

fn base64_value(byte: u8) -> Result<u8> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => bail!("invalid Base64 character in PCM chunk"),
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
