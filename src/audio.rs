use std::ffi::c_void;
use std::ptr;

use anyhow::{anyhow, bail, Context, Result};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Gpio46, Output, PinDriver};
use esp_idf_svc::sys::{self, EspError};

use crate::i2c_bus::I2cBus;
use crate::notifications::Notification;

const CODEC_ADDRESS: u8 = 0x18;
const SAMPLE_RATE_HZ: u32 = 24_000;
const SAMPLE_BUFFER_SIZE: usize = 960;

pub const DEFAULT_FREQUENCY_HZ: u16 = 880;
pub const DEFAULT_DURATION_MS: u16 = 500;
pub const DEFAULT_VOLUME_PERCENT: u8 = 45;

pub const SUPPORTED_SAMPLE_RATES: [u32; 6] = [8_000, 16_000, 24_000, 32_000, 44_100, 48_000];

#[derive(Clone, Copy)]
enum ToneStep {
    Tone { frequency_hz: u16, duration_ms: u16 },
    Pause { duration_ms: u16 },
}

struct ToneSequence<'a> {
    waveform: Waveform,
    volume_percent: u8,
    steps: &'a [ToneStep],
}

const CHARGE_SOON_STEPS: [ToneStep; 5] = [
    ToneStep::Tone {
        frequency_hz: 784,
        duration_ms: 140,
    },
    ToneStep::Pause { duration_ms: 110 },
    ToneStep::Tone {
        frequency_hz: 659,
        duration_ms: 140,
    },
    ToneStep::Pause { duration_ms: 110 },
    ToneStep::Tone {
        frequency_hz: 523,
        duration_ms: 260,
    },
];

const CHARGE_CRITICAL_STEPS: [ToneStep; 9] = [
    ToneStep::Tone {
        frequency_hz: 1_176,
        duration_ms: 180,
    },
    ToneStep::Pause { duration_ms: 70 },
    ToneStep::Tone {
        frequency_hz: 1_176,
        duration_ms: 180,
    },
    ToneStep::Pause { duration_ms: 70 },
    ToneStep::Tone {
        frequency_hz: 1_176,
        duration_ms: 180,
    },
    ToneStep::Pause { duration_ms: 110 },
    ToneStep::Tone {
        frequency_hz: 784,
        duration_ms: 260,
    },
    ToneStep::Pause { duration_ms: 80 },
    ToneStep::Tone {
        frequency_hz: 587,
        duration_ms: 380,
    },
];

#[derive(Clone, Copy, Debug)]
pub enum Waveform {
    Sine,
    Square,
    Triangle,
}

impl Waveform {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_uppercase().as_str() {
            "SINE" | "SIN" => Ok(Self::Sine),
            "SQUARE" | "SQ" => Ok(Self::Square),
            "TRIANGLE" | "TRI" => Ok(Self::Triangle),
            _ => bail!("waveform must be sine, square, or triangle"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Square => "square",
            Self::Triangle => "triangle",
        }
    }
}

/// Controls the speaker amplifier while creating the I2S channel only during
/// playback. Dropping the temporary channel stops MCLK between sounds.
pub struct Audio<'d> {
    amplifier_enable: PinDriver<'d, Output>,
    pcm: Option<PcmPlayback>,
    pcm_error: Option<String>,
}

struct PcmPlayback {
    driver: I2sTxChannel,
    expected_bytes: usize,
    written_bytes: usize,
    sample_rate_hz: u32,
}

impl<'d> Audio<'d> {
    pub fn new(amplifier_enable: Gpio46<'d>) -> Result<Self> {
        let mut amplifier_enable = PinDriver::output(amplifier_enable)?;
        amplifier_enable.set_low()?;
        Ok(Self {
            amplifier_enable,
            pcm: None,
            pcm_error: None,
        })
    }

    pub fn play_tone(
        &mut self,
        i2c: &mut I2cBus<'_>,
        frequency_hz: u16,
        duration_ms: u16,
        volume_percent: u8,
        waveform: Waveform,
    ) -> Result<()> {
        let steps = [ToneStep::Tone {
            frequency_hz,
            duration_ms,
        }];
        self.play_tone_sequence(
            i2c,
            ToneSequence {
                waveform,
                volume_percent,
                steps: &steps,
            },
        )
    }

    pub fn play_notification(
        &mut self,
        i2c: &mut I2cBus<'_>,
        notification: Notification,
    ) -> Result<()> {
        let sequence = match notification {
            Notification::ChargeSoon => ToneSequence {
                waveform: Waveform::Sine,
                volume_percent: 50,
                steps: &CHARGE_SOON_STEPS,
            },
            Notification::ChargeCritical => ToneSequence {
                waveform: Waveform::Square,
                volume_percent: 90,
                steps: &CHARGE_CRITICAL_STEPS,
            },
        };
        self.play_tone_sequence(i2c, sequence)
    }

    fn play_tone_sequence(
        &mut self,
        i2c: &mut I2cBus<'_>,
        sequence: ToneSequence<'_>,
    ) -> Result<()> {
        if self.pcm.is_some() {
            bail!("PCM playback is already active");
        }
        let mut driver = I2sTxChannel::new(SAMPLE_RATE_HZ).context("configuring I2S output")?;

        let result: Result<()> = (|| {
            // Waveshare's ESP-IDF codec path starts MCLK/BCLK before codec
            // setup so every ES8311 register transition has a live clock.
            driver.enable().context("starting I2S output")?;
            initialize_codec(i2c, SAMPLE_RATE_HZ, sequence.volume_percent)?;
            self.amplifier_enable
                .set_high()
                .context("enabling speaker amplifier")?;

            for step in sequence.steps {
                match *step {
                    ToneStep::Tone {
                        frequency_hz,
                        duration_ms,
                    } => {
                        write_tone_wave(&mut driver, sequence.waveform, frequency_hz, duration_ms)?
                    }
                    ToneStep::Pause { duration_ms } => {
                        write_silence(&mut driver, SAMPLE_RATE_HZ, usize::from(duration_ms))?
                    }
                }
            }
            write_silence(&mut driver, SAMPLE_RATE_HZ, 20)?;
            Ok(())
        })();

        // Always silence the physical amplifier, including on a write error.
        let amplifier_result = self.amplifier_enable.set_low();
        let _ = mute_codec(i2c);
        result?;
        amplifier_result.context("disabling speaker amplifier")?;
        Ok(())
    }

    pub fn begin_pcm(
        &mut self,
        i2c: &mut I2cBus<'_>,
        expected_bytes: usize,
        sample_rate_hz: u32,
        volume_percent: u8,
    ) -> Result<()> {
        if self.pcm.is_some() {
            bail!("PCM playback is already active");
        }
        if expected_bytes == 0 || expected_bytes % 2 != 0 {
            bail!("PCM byte count must be a positive even number");
        }
        validate_sample_rate(sample_rate_hz)?;

        self.pcm_error = None;
        let mut driver = I2sTxChannel::new(sample_rate_hz).context("configuring PCM I2S output")?;
        driver.enable().context("starting PCM I2S output")?;
        initialize_codec(i2c, sample_rate_hz, volume_percent)?;
        self.amplifier_enable
            .set_high()
            .context("enabling speaker amplifier")?;
        self.pcm = Some(PcmPlayback {
            driver,
            expected_bytes,
            written_bytes: 0,
            sample_rate_hz,
        });
        Ok(())
    }

    pub fn write_pcm(&mut self, i2c: &mut I2cBus<'_>, data: &[u8]) -> Result<usize> {
        if let Some(error) = &self.pcm_error {
            bail!("PCM stream already failed: {error}");
        }
        let write_result = match self.pcm.as_mut() {
            Some(playback) if playback.written_bytes + data.len() > playback.expected_bytes => {
                Err(anyhow!("PCM data exceeds declared byte count"))
            }
            Some(playback) => playback.driver.write_all(data),
            None => bail!("PCM playback is not active"),
        };
        if let Err(error) = write_result {
            self.pcm.take();
            let _ = self.amplifier_enable.set_low();
            let _ = mute_codec(i2c);
            let message = format!("{error:#}");
            self.pcm_error = Some(message.clone());
            bail!("{message}");
        }
        let playback = self.pcm.as_mut().context("PCM playback is not active")?;
        playback.written_bytes += data.len();
        Ok(playback.written_bytes)
    }

    pub fn finish_pcm(&mut self, i2c: &mut I2cBus<'_>) -> Result<usize> {
        if let Some(error) = self.pcm_error.take() {
            bail!("PCM stream failed: {error}");
        }
        let mut playback = self.pcm.take().context("PCM playback is not active")?;
        let silence_result = write_silence(&mut playback.driver, playback.sample_rate_hz, 20);
        let amplifier_result = self.amplifier_enable.set_low();
        let _ = mute_codec(i2c);
        silence_result?;
        amplifier_result.context("disabling speaker amplifier")?;
        if playback.written_bytes != playback.expected_bytes {
            bail!(
                "PCM length mismatch: expected {} bytes, played {}",
                playback.expected_bytes,
                playback.written_bytes
            );
        }
        Ok(playback.written_bytes)
    }

    /// Whether the I2S channel is streaming. A remembered failure does not count:
    /// nothing is playing then, and the dashboard must keep rendering even if the
    /// host never sends `AUDIO PCM END`.
    pub fn is_pcm_active(&self) -> bool {
        self.pcm.is_some()
    }
}

fn initialize_codec(i2c: &mut I2cBus<'_>, sample_rate_hz: u32, volume_percent: u8) -> Result<()> {
    validate_sample_rate(sample_rate_hz)?;
    // Waveshare's ES8311 sequence for 16-bit mono with MCLK = sample rate * 256.
    write_register(i2c, 0x00, 0x1f)?;
    FreeRtos::delay_ms(20);
    write_register(i2c, 0x00, 0x00)?;
    write_register(i2c, 0x00, 0x80)?;
    write_register(i2c, 0x01, 0x3f)?;
    write_register(i2c, 0x02, u8::from(sample_rate_hz == 8_000) << 3)?;
    write_register(i2c, 0x03, 0x10)?;
    write_register(i2c, 0x04, 0x10)?;
    write_register(i2c, 0x05, 0x00)?;
    write_register(i2c, 0x06, 0x03)?;
    write_register(i2c, 0x07, 0x00)?;
    write_register(i2c, 0x08, 0xff)?;
    write_register(i2c, 0x09, 0x0c)?;
    write_register(i2c, 0x0a, 0x0c)?;
    write_register(i2c, 0x0d, 0x01)?;
    write_register(i2c, 0x0e, 0x02)?;
    write_register(i2c, 0x12, 0x00)?;
    write_register(i2c, 0x13, 0x10)?;
    write_register(i2c, 0x1c, 0x6a)?;
    write_register(i2c, 0x37, 0x08)?;

    let volume_register = if volume_percent == 0 {
        0
    } else {
        ((u16::from(volume_percent) * 256 / 100) - 1) as u8
    };
    write_register(i2c, 0x32, volume_register)?;
    write_register(i2c, 0x31, 0x00)?;
    Ok(())
}

fn validate_sample_rate(sample_rate_hz: u32) -> Result<()> {
    if SUPPORTED_SAMPLE_RATES.contains(&sample_rate_hz) {
        Ok(())
    } else {
        bail!("sample rate must be one of 8000, 16000, 24000, 32000, 44100, 48000 Hz")
    }
}

fn mute_codec(i2c: &mut I2cBus<'_>) -> Result<()> {
    let current = read_register(i2c, 0x31)?;
    write_register(i2c, 0x31, current | 0x60)
}

fn write_register(i2c: &mut I2cBus<'_>, register: u8, value: u8) -> Result<()> {
    i2c.write(CODEC_ADDRESS, &[register, value])
        .with_context(|| format!("writing ES8311 register 0x{register:02x}"))
}

fn read_register(i2c: &mut I2cBus<'_>, register: u8) -> Result<u8> {
    let mut value = [0_u8; 1];
    i2c.write_read(CODEC_ADDRESS, &[register], &mut value)
        .with_context(|| format!("reading ES8311 register 0x{register:02x}"))?;
    Ok(value[0])
}

fn write_tone_wave(
    driver: &mut I2sTxChannel,
    waveform: Waveform,
    frequency_hz: u16,
    duration_ms: u16,
) -> Result<()> {
    let sample_count = SAMPLE_RATE_HZ as usize * usize::from(duration_ms) / 1_000;
    let phase_step = std::f32::consts::TAU * f32::from(frequency_hz) / SAMPLE_RATE_HZ as f32;
    let amplitude = i16::MAX as f32 * 0.32;
    let mut phase = 0.0_f32;
    let mut remaining = sample_count;
    let mut buffer = [0_u8; SAMPLE_BUFFER_SIZE];

    while remaining > 0 {
        let samples = remaining.min(buffer.len() / 2);
        for bytes in buffer[..samples * 2].chunks_exact_mut(2) {
            let normalized = match waveform {
                Waveform::Sine => phase.sin(),
                Waveform::Square => {
                    if phase < std::f32::consts::PI {
                        1.0
                    } else {
                        -1.0
                    }
                }
                Waveform::Triangle => {
                    if phase < std::f32::consts::FRAC_PI_2 {
                        phase / std::f32::consts::FRAC_PI_2
                    } else if phase < 3.0 * std::f32::consts::FRAC_PI_2 {
                        1.0 - 2.0 * (phase - std::f32::consts::FRAC_PI_2) / std::f32::consts::PI
                    } else {
                        -1.0 + (phase - 3.0 * std::f32::consts::FRAC_PI_2)
                            / std::f32::consts::FRAC_PI_2
                    }
                }
            };
            let sample = (normalized * amplitude) as i16;
            bytes.copy_from_slice(&sample.to_le_bytes());
            phase += phase_step;
            if phase >= std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        driver
            .write_all(&buffer[..samples * 2])
            .context("writing tone samples")?;
        remaining -= samples;
    }
    Ok(())
}

fn write_silence(driver: &mut I2sTxChannel, sample_rate_hz: u32, duration_ms: usize) -> Result<()> {
    // Static, so this neither claims stack nor zeroes a buffer per call.
    static SILENCE: [u8; SAMPLE_BUFFER_SIZE] = [0; SAMPLE_BUFFER_SIZE];

    let bytes = sample_rate_hz as usize * duration_ms / 1_000 * 2;
    let silence = &SILENCE;
    let mut remaining = bytes;
    while remaining > 0 {
        let length = remaining.min(silence.len());
        driver
            .write_all(&silence[..length])
            .context("writing silence samples")?;
        remaining -= length;
    }
    Ok(())
}

/// Rust RAII wrapper around ESP-IDF 5's current I2S channel API.
///
/// The HAL I2S module currently pulls in ESP-IDF's legacy I2C driver, which
/// cannot coexist with the new I2C API used by the RTC and sensors.
struct I2sTxChannel {
    handle: sys::i2s_chan_handle_t,
    enabled: bool,
}

impl I2sTxChannel {
    fn new(sample_rate_hz: u32) -> Result<Self> {
        validate_sample_rate(sample_rate_hz)?;
        let channel_config = sys::i2s_chan_config_t {
            id: sys::i2s_port_t_I2S_NUM_0,
            role: sys::i2s_role_t_I2S_ROLE_MASTER,
            dma_desc_num: 6,
            dma_frame_num: 240,
            __bindgen_anon_1: Default::default(),
            auto_clear_before_cb: false,
            allow_pd: false,
            intr_priority: 0,
        };
        let mut handle = ptr::null_mut();
        EspError::convert(unsafe {
            sys::i2s_new_channel(&channel_config, &mut handle, ptr::null_mut())
        })
        .context("allocating I2S channel")?;

        let standard_config = sys::i2s_std_config_t {
            clk_cfg: sys::i2s_std_clk_config_t {
                sample_rate_hz,
                clk_src: sys::soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
                ext_clk_freq_hz: 0,
                mclk_multiple: sys::i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
                bclk_div: 8,
            },
            slot_cfg: sys::i2s_std_slot_config_t {
                data_bit_width: sys::i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
                slot_bit_width: sys::i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO,
                slot_mode: sys::i2s_slot_mode_t_I2S_SLOT_MODE_MONO,
                // These match ESP-IDF's ESP32-S3 Philips-mode defaults. In
                // mono TX mode BOTH duplicates each sample into both slots.
                slot_mask: sys::i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH,
                ws_width: 16,
                ws_pol: false,
                bit_shift: true,
                left_align: true,
                big_endian: false,
                bit_order_lsb: false,
            },
            gpio_cfg: sys::i2s_std_gpio_config_t {
                mclk: 14,
                bclk: 15,
                ws: 38,
                dout: 45,
                din: -1,
                invert_flags: Default::default(),
            },
        };
        if let Err(error) =
            EspError::convert(unsafe { sys::i2s_channel_init_std_mode(handle, &standard_config) })
        {
            unsafe {
                let _ = sys::i2s_del_channel(handle);
            }
            return Err(error).context("initializing standard I2S mode");
        }

        Ok(Self {
            handle,
            enabled: false,
        })
    }

    fn enable(&mut self) -> Result<()> {
        EspError::convert(unsafe { sys::i2s_channel_enable(self.handle) })?;
        self.enabled = true;
        Ok(())
    }

    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let mut written = 0;
            EspError::convert(unsafe {
                sys::i2s_channel_write(
                    self.handle,
                    data[offset..].as_ptr().cast::<c_void>(),
                    data.len() - offset,
                    &mut written,
                    1_000,
                )
            })?;
            if written == 0 {
                return Err(anyhow!("I2S accepted zero bytes"));
            }
            offset += written;
        }
        Ok(())
    }
}

impl Drop for I2sTxChannel {
    fn drop(&mut self) {
        unsafe {
            if self.enabled {
                let _ = sys::i2s_channel_disable(self.handle);
            }
            if !self.handle.is_null() {
                let _ = sys::i2s_del_channel(self.handle);
            }
        }
    }
}
