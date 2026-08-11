# ESP32-S3 e-paper dashboard in Rust

Modular Rust + ESP-IDF firmware for the Waveshare **ESP32-S3-ePaper-1.54**
(200 × 200, black/white) development board.

The dashboard currently shows:

- a centered `HH:MM` clock from the onboard PCF85063 RTC;
- temperature in degrees Celsius and relative humidity from the onboard SHTC3;
- Wi-Fi link state and the configured network name at the top left;
- estimated battery percentage at the top right when a plausible battery voltage is detected.

The first render and every 30th subsequent update use a full e-paper refresh.
Other minute changes use the board's partial-refresh waveform to reduce flashing
and ghosting.

## USB command console

The Type-C connector exposes ESP32-S3 USB Serial/JTAG. It carries both logs and
the line-oriented command console; no separate UART adapter is required.

Commands are case-insensitive. SSIDs and passwords containing spaces can be
quoted:

```text
PING
TIME GET
TIME SET 2026-08-11 18:45:00
TIME SET 2026-08-11T18:45:00
WIFI SET "My WiFi" "my secret password"
WIFI SCAN
WIFI STATUS
WIFI CLEAR
STATUS
REFRESH
AUDIO BEEP 880 500 45
AUDIO TONE SQUARE 440 500 35
AUDIO TONE TRIANGLE 660 250 40
HELP
```

Successful commands begin with `OK`; malformed commands and hardware/network
failures begin with `ERR`. The RTC stores local wall-clock time and has no
timezone field. Wi-Fi credentials persist in the default ESP-IDF NVS partition.
They are not printed back by any command.

To set the RTC from the development computer, open the monitor and paste a
`TIME SET` command using the computer's current local time:

```sh
source /Users/kane/export-esp.sh
espflash monitor --port /dev/cu.usbmodem101
```

Exit the monitor with `Ctrl-C`.

### Friendly host CLI

The Python helper in `scripts/` autodetects a single connected serial device,
validates dates, can synchronize from the computer's local clock, and securely
prompts for Wi-Fi passwords:

```sh
python3 -m pip install -r scripts/requirements.txt

python3 scripts/device_cli.py time sync
python3 scripts/device_cli.py time get
python3 scripts/device_cli.py time set "2026-08-11 18:45:00"
python3 scripts/device_cli.py time calibration
python3 scripts/device_cli.py time calibrate --ppm 12.5
python3 scripts/device_cli.py time calibrate --drift-seconds 2 --elapsed-hours 48

python3 scripts/device_cli.py wifi set "My WiFi"
python3 scripts/device_cli.py wifi scan
python3 scripts/device_cli.py wifi status
python3 scripts/device_cli.py wifi clear

python3 scripts/device_cli.py status
python3 scripts/device_cli.py refresh
python3 scripts/device_cli.py audio beep
python3 scripts/device_cli.py audio tone sine --frequency 440
python3 scripts/device_cli.py audio tone square --frequency 440 --duration 250
python3 scripts/device_cli.py audio tone triangle --frequency 660 --volume 35
python3 scripts/device_cli.py audio pcm sound.wav --sample-rate 24000 --volume 45
python3 scripts/device_cli.py audio pcm sound.pcm --sample-rate 16000 --volume 40
python3 scripts/device_cli.py audio beep --frequency 440 --duration 1000 --volume 35
python3 scripts/device_cli.py console
```

Pass `--port /dev/cu.usbmodem101` before the subcommand to override
autodetection. Use `--dry-run` to inspect the command without opening the port;
Wi-Fi passwords are always redacted in dry-run output.

The helper performs a `PING`/`OK PONG` readiness handshake before every action.
This matters on the native ESP32-S3 USB port because opening it can reset the
board; commands are never sent into the boot window.

`time sync` waits for the device to finish booting, captures computer time only
afterward, aligns the write to the next whole second, and verifies the RTC by
reading it back. Crystal drift can be corrected in the PCF85063 hardware. A
positive calibration value means the RTC gains time; use a multi-day observation
for useful ppm accuracy. Calibration uses the lower-power normal correction mode.

WAV playback is converted by the Python CLI to signed 16-bit little-endian mono
PCM before streaming. Raw `.pcm` input must already use that format at the
selected rate. Supported output rates are 8, 16, 24, 32, 44.1, and 48 kHz;
streams are limited to 120 seconds. The CLI transports PCM as acknowledged
Base64 chunks, avoiding control-byte handling in ESP-IDF's line-oriented
console and automatically returning to ordinary commands after playback.

## Project structure

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | board wiring, event loop, refresh scheduling |
| `src/board.rs` | board peripheral power-rail control |
| `src/commands.rs` | USB command parsing and input thread |
| `src/datetime.rs` | validated date/time representation and parsing |
| `src/rtc.rs` | PCF85063 BCD register protocol |
| `src/shtc3.rs` | SHTC3 measurement, CRC, sleep/wake handling |
| `src/battery.rs` | calibrated ADC sampling and approximate LiPo percentage |
| `src/audio.rs` | ES8311 setup and temporary I2S tone playback |
| `src/wifi.rs` | station-mode connection and NVS credential storage |
| `src/dashboard.rs` | display layout, text, badges, and framebuffer |
| `src/epaper.rs` | SPI panel driver and full/partial waveforms |
| `src/i2c_bus.rs` | ESP-IDF 5 current master-bus API used by onboard sensors |
| `src/power.rs` | CPU dynamic-frequency and always-responsive USB policy |

## Power behavior

The firmware blocks on USB command input between minute boundaries instead of
polling ten times per second. The RTC is read at the next minute boundary, the
SHTC3 returns to sleep after every measurement, and I2S plus the speaker
amplifier are enabled only during playback. CPU dynamic-frequency scaling uses
40 MHz while idle and up to 160 MHz when ESP-IDF peripheral locks require it.
Connected Wi-Fi uses maximum modem power saving while preserving the station
connection. If a saved network is unavailable, a non-blocking reconnect is
requested every five minutes; link state is checked alongside the RTC minute
poll and refreshes the badge only when it changes. Automatic light sleep is intentionally disabled because it would
make the native USB Serial/JTAG command interface intermittently unavailable.

The SHTC3 temperature applies Waveshare's documented `-4 C` board/enclosure
compensation. Battery percentage is an estimate from voltage because this board
does not expose a fuel-gauge IC.

## Board connections

| Peripheral | Address / GPIO |
| --- | --- |
| E-paper power (active low) | GPIO 6 |
| E-paper busy | GPIO 8 |
| E-paper reset | GPIO 9 |
| E-paper data/command | GPIO 10 |
| E-paper chip select | GPIO 11 |
| E-paper SPI clock | GPIO 12 |
| E-paper SPI MOSI | GPIO 13 |
| RTC PCF85063 | I2C `0x51` |
| Temperature/humidity SHTC3 | I2C `0x70` |
| Shared I2C SDA / SCL | GPIO 47 / GPIO 48 |
| Battery ADC | GPIO 4, ADC1 channel 3, 2:1 divider |
| Audio/codec rail (active low; required for shared I2C) | GPIO 42 |
| Audio I2S MCLK / BCLK / LRCK / data out | GPIO 14 / 15 / 38 / 45 |
| Speaker amplifier enable | GPIO 46 |

## Build, lint, and flash

The project uses ESP-IDF 5.5.3. Waveshare requires ESP-IDF 5.5 or newer for
this board.

```sh
source /Users/kane/export-esp.sh
cargo fmt --all --check
cargo clippy -- -D warnings
cargo build
espflash flash --monitor --port /dev/cu.usbmodem101 \
  target/xtensa-esp32s3-espidf/debug/esp-smart-eink
```

## Factory firmware backup and restore

A complete raw 8 MB factory image was read before the first development flash:

```text
backups/esp32-s3-factory-14c19fd46a3c-2026-08-11.bin
SHA-256: 6e591635ed34d211fdcd2d4138fb7d7ca778d06830034c1dd06840e658b0065d
```

Binary backups are ignored by Git because they may contain device-specific
settings or credentials. Keep another copy somewhere safe.

After confirming the port, board identity, filename, and checksum, restore it
at flash offset `0x0`:

```sh
esptool --chip esp32s3 --port /dev/cu.usbmodem101 \
  write-flash 0x0 backups/esp32-s3-factory-14c19fd46a3c-2026-08-11.bin
```
