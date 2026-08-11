# ESP32-S3 e-paper dashboard in Rust

Modular Rust + ESP-IDF firmware for the Waveshare **ESP32-S3-ePaper-1.54**
(200 × 200, black/white) development board.

The dashboard currently shows:

- a centered `HH:MM` clock and `Mon 10 Jun` date from the onboard PCF85063 RTC,
  framed by a morning/day/evening/night scene selected from the current hour;
- indoor temperature and relative humidity with thermometer and water-drop
  icons in the lower-left widget;
- current outdoor temperature and conditions plus today's mean, humidity, and
  rain chance from Open-Meteo in the lower-right widget, using sun, cloud, fog,
  rain, and snow condition icons;
- Wi-Fi signal strength and the configured network name at the top left;
- a five-level battery icon at the top right, with a lightning bolt on its left
  while attached to a USB host.

The first render and every 30th visual update use a full e-paper refresh. Between
them, ink-stacks reconciles a candidate framebuffer against the pixels already
committed to the panel. Only changed, byte-aligned rectangles are written to
panel RAM before a partial refresh, reducing SPI traffic, flashing, and ghosting.

The dashboard has three pages. Press **BOOT** to move forward and **PWR** to move
backward:

1. Home: clock, date, indoor readings, and current weather.
2. Today: city/country code, current conditions, humidity, daily mean, low,
   high, and rain probability.
3. Forecast: city/country code and side-by-side today/tomorrow conditions,
   including daily means, lows, highs, rain, and maximum wind speed.

The display code uses a small in-project layout layer called **ink-stacks**.
Every widget draws relative to the rectangle assigned by a horizontal or
vertical stack; children can have a fixed pixel length or share the remaining
space by weight. The current compositions intentionally preserve the original
200 x 200 design, while the framebuffer and stack bounds already accept runtime
dimensions for a future panel-specific layout.

Rendering follows a React-like state model: asynchronous producers emit events,
the application reduces a batch into dashboard state, widgets render a candidate
view, and framebuffer reconciliation decides whether any panel work is needed.
Widget properties may change without causing a refresh when their pixels remain
identical.

Both buttons are active-low, interrupt-driven, and software-debounced after an
edge. Holding BOOT while resetting still invokes the ESP32-S3 ROM download
behavior, so normal navigation uses short presses after startup.

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

After Wi-Fi connects, the firmware determines its approximate coordinates,
city, and country code once using `ipwho.is` and saves them in NVS. Later boots
reuse the complete location. Firmware upgrading from the old coordinate-only
cache refreshes the location once to add the display name.
Open-Meteo weather is fetched on a dedicated worker thread every 20 minutes, so
HTTPS requests never block input or dashboard rendering. A failed update is
retried after five minutes and leaves the last successful weather reading on
screen.

To set the RTC from the development computer, open the monitor and paste a
`TIME SET` command using the computer's current local time:

```sh
source "$HOME/.local/bin/export-esp.sh"
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
| `src/buttons.rs` | debounced BOOT/PWR page navigation |
| `src/commands.rs` | USB command parsing and input thread |
| `src/events.rs` | central application event types and blocking queue |
| `src/datetime.rs` | validated date/time representation and parsing |
| `src/rtc.rs` | PCF85063 BCD register protocol |
| `src/shtc3.rs` | SHTC3 measurement, CRC, sleep/wake handling |
| `src/battery.rs` | calibrated ADC sampling and approximate LiPo percentage |
| `src/audio.rs` | ES8311 setup and temporary I2S tone playback |
| `src/wifi.rs` | station-mode connection and NVS credential storage |
| `src/location.rs` | cached coordinates and pluggable IP geolocation provider |
| `src/weather.rs` | background Open-Meteo fetch and refresh scheduling |
| `src/ink_stacks.rs` | 1-bit framebuffer plus fixed/fill row and column layout |
| `src/dashboard.rs` | dashboard state, page selection, and render entry point |
| `src/dashboard/widgets.rs` | composable status, clock, climate, and weather widgets |
| `src/epaper.rs` | SPI panel driver, dirty-window writes, and refresh waveforms |
| `src/i2c_bus.rs` | ESP-IDF 5 current master-bus API used by onboard sensors |
| `src/power.rs` | CPU dynamic-frequency and always-responsive USB policy |
| `espflash.toml` | 8 MB flash size and custom partition-table selection |
| `partitions.csv` | NVS, PHY, and enlarged single-app flash layout |

## Power behavior

The main task blocks indefinitely on one event queue; it has no idle cadence.
GPIO interrupts wake the button tasks, the USB Serial/JTAG driver blocks on its
receive interrupt, and one-shot ESP timers emit clock, weather, and reconnect
events. Since the RTC interrupt output is not wired to the ESP32-S3, the next
minute boundary is scheduled once from the RTC seconds value instead of sampled
repeatedly. The e-paper BUSY pin is also awaited by interrupt, raced against a
one-shot timeout. The SHTC3 returns to sleep after every event-triggered
measurement, and I2S plus the speaker amplifier are enabled only during
playback. CPU dynamic-frequency scaling uses 40 MHz while idle and up to 160 MHz
when ESP-IDF peripheral locks require it.
Connected Wi-Fi uses maximum modem power saving while preserving the station
connection. If a saved network is unavailable, a non-blocking reconnect is
requested by a one-shot event after five minutes. ESP-IDF Wi-Fi/IP events update
link state immediately; signal strength and the remaining sensor state are
sampled on minute events. Pixel reconciliation suppresses the badge refresh
unless its rendered appearance changes. Automatic light sleep is intentionally
disabled because it would make the native USB Serial/JTAG command interface
intermittently unavailable.

The SHTC3 temperature applies Waveshare's documented `-4 C` board/enclosure
compensation. Battery percentage is an estimate from voltage because this board
does not expose a fuel-gauge IC.

The icon quantizes the voltage estimate into approximately 10%, 25%, 50%, 75%,
and nearly-full fill levels instead of displaying an exact number. Its lightning
bolt reports an active native USB host connection; the schematic does not expose
the charger's status output or USB VBUS to an ESP32 GPIO.

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
| BOOT / next-page button (active low) | GPIO 0 |
| PWR / previous-page button (active low) | GPIO 18 |
| Audio/codec rail (active low; required for shared I2C) | GPIO 42 |
| Audio I2S MCLK / BCLK / LRCK / data out | GPIO 14 / 15 / 38 / 45 |
| Speaker amplifier enable | GPIO 46 |

## Build, lint, and flash

The project uses ESP-IDF 5.5.3. Waveshare requires ESP-IDF 5.5 or newer for
this board.

```sh
source "$HOME/.local/bin/export-esp.sh"
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
