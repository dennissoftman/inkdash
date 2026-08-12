# ESP32-S3 e-paper dashboard in Rust

Modular Rust + ESP-IDF firmware for the Waveshare **ESP32-S3-ePaper-1.54**
(200 × 200, black/white) development board.

The dashboard currently shows:

- a centered `HH:MM` clock and `Mon 10 Jun` date from the onboard PCF85063 RTC,
  framed by a scene drawn from the current hour and the current weather — rain,
  snow, fog, cloud, or a thunderbolt (see [Clock backdrops](#clock-backdrops));
- indoor temperature and relative humidity with thermometer and water-drop
  icons in the lower-left widget;
- current outdoor temperature and conditions plus today's mean, humidity, and
  rain chance from Open-Meteo in the lower-right widget, using sun, cloud, fog,
  rain, snow, and thunderstorm condition icons;
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
2. Current details: city/country code, current conditions and temperature,
   humidity, rain probability, and today's low, mean, and high temperatures.
3. Tomorrow: city/country code and local-time snapshots for 08:00, 12:00,
   18:00, and 23:00. Each row shows its condition icon and right-aligned
   temperature; a cloud-with-rain icon and probability appear only above 0%.

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

The display language is stored in NVS and defaults to English. English (`en`)
and Russian (`ru`) are currently available; changing it redraws the active page
immediately. UI copy and date abbreviations live in one translation table in
`src/language.rs`, and the display uses the ISO-8859-5 variants of its bitmap
fonts so Cyrillic and ASCII share the same layout code. The USB command protocol
continues to use stable language codes and English command names.

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
LANGUAGE GET
LANGUAGE SET ru
STATUS
REFRESH
AUDIO BEEP 880 500 45
AUDIO TONE SQUARE 440 500 35
AUDIO TONE TRIANGLE 660 250 40
HELP
```

Successful commands begin with `OK`; malformed commands and hardware/network
failures begin with `ERR`. The RTC stores local wall-clock time and has no
timezone field. After Wi-Fi comes up, SNTP synchronizes it on boot and once per
day. The location-derived UTC offset keeps the hardware clock in local time and
also applies daylight-saving changes. Wi-Fi credentials persist in the default
ESP-IDF NVS partition. They are not printed back by any command.

After Wi-Fi connects, the background location service determines approximate
coordinates, city, country code, IANA timezone, and current UTC offset using
`ipwho.is`, then saves the result in NVS. Weather and NTP both consume this
shared location value. Later boots reuse the cache, while each daily NTP sync
refreshes it so timezone/DST offsets stay current. Firmware upgrading from an
older partial cache automatically refreshes it.
Open-Meteo weather is fetched on boot and then on a dedicated worker thread
every hour, so HTTPS requests never block input or dashboard rendering. The
response includes current conditions, today's daily aggregates, and tomorrow's
hourly values sampled in the location's local timezone. A failed update is
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

python3 scripts/device_cli.py language get
python3 scripts/device_cli.py language set ru

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

## Clock backdrops

The clock card carries a scene built from two things: the hour and the current
conditions.

The hour picks the sky, the sun or moon, and the ground: sunrise over hills
(05:00–10:59), a high sun with conifers on the ridge (11:00–16:59), a sun sinking
into the ridge (17:00–20:59), and an inverted card with a crescent moon and a lit
skyline (21:00–04:59).

The weather from Open-Meteo then draws over it, so the card shows what it is
doing outside without reading a word:

| Conditions | What the scene gains |
| --- | --- |
| Clear | rays, the moon's halo, birds by day, a full star field at night |
| Cloudy | a cloud bank across the sun or moon, and a slightly heavier sky |
| Fog | drifting bands, a bare disc, and pale bands dissolving the ground |
| Rain | a cloud, falling streaks, and puddles below the horizon |
| Snow | a cloud, flakes, and settled snow along the ridge |
| Thunderstorm | the rain scene plus a bolt under the cloud |

Anything overcast also drops the clear-sky details: no rays, no birds, and no
stars, which is most of what makes the conditions readable at a glance. The sky
texture is ordered dithering that feathers out as it approaches the clock, so the
digits keep their contrast in every combination. Storms now have their own
condition icon on the weather pages too, instead of borrowing rain's.

The artwork lives in `src/dashboard/backdrops.rs` with no dependency on the rest
of the firmware, so it can be rendered on a host machine. The preview
tool includes that module and the real framebuffer, then writes a PNG contact
sheet of every hour and condition combination with the clock drawn over them:

```sh
cargo +stable run --manifest-path tools/backdrop-preview/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/^host: //p')"
# writes tools/backdrop-preview/backdrops.png

# Append a packed bitmap to preview a custom backdrop the same way.
cargo +stable run --manifest-path tools/backdrop-preview/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/^host: //p')" -- backdrop.bin
```

The explicit target and `+stable` are what keep the tool off the board's build
settings in `.cargo/config.toml`. Editing the artwork and re-running takes a
couple of seconds, so scenes can be iterated without flashing.

Custom artwork is not stored on the device yet. The drawing side is finished and
storage-agnostic — `backdrops::draw_custom` takes a packed 192 × 72 bitmap, one
bit per pixel, plus a flag pair for the clock colour and for a plate behind the
text — but four slots in NVS would cost about 7 KB of a 24 KB partition, which is
the wrong home for artwork. An SD card is the intended source. Until then the
preview tool is the only consumer, and it renders a bitmap exactly as the panel
would:

```sh
cargo +stable run --manifest-path tools/backdrop-preview/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/^host: //p')" -- backdrop.bin

# Any image converts to that format without extra packages:
magick photo.jpg -resize 192x72! -dither FloydSteinberg -monochrome backdrop.pbm
```

## Project structure

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | board wiring, event loop, refresh scheduling |
| `src/board.rs` | board peripheral power-rail control |
| `src/buttons.rs` | debounced BOOT/PWR page navigation |
| `src/commands.rs` | USB command parsing and input thread |
| `src/config.rs` | centralized operational policy and build-time overrides |
| `src/events.rs` | central application event types and blocking queue |
| `src/datetime.rs` | validated date/time representation and parsing |
| `src/rtc.rs` | PCF85063 BCD register protocol |
| `src/shtc3.rs` | SHTC3 measurement, CRC, sleep/wake handling |
| `src/battery.rs` | calibrated ADC sampling and approximate LiPo percentage |
| `src/audio.rs` | ES8311 setup and temporary I2S tone playback |
| `src/wifi.rs` | station-mode connection and NVS credential storage |
| `src/ntp.rs` | boot-time and daily SNTP synchronization events |
| `src/location.rs` | reusable background location/timezone service and NVS cache |
| `src/weather.rs` | location-agnostic Open-Meteo fetching and background refresh scheduling |
| `src/ink_stacks.rs` | 1-bit framebuffer plus fixed/fill row and column layout |
| `src/language.rs` | persistent language setting and extensible UI translation tables |
| `src/notifications.rs` | semantic battery-alert scheduling and quiet-hour policy |
| `src/ota.rs` | host-neutral manifest lookup, cancellable download, digest verification, and ESP-IDF OTA writes |
| `src/dashboard.rs` | dashboard state, page selection, and render entry point |
| `src/dashboard/backdrops.rs` | hour and weather clock artwork, and custom bitmap drawing |
| `src/dashboard/widgets.rs` | composable status, clock, climate, and weather widgets |
| `src/epaper.rs` | SPI panel driver, dirty-window writes, and refresh waveforms |
| `src/i2c_bus.rs` | ESP-IDF 5 current master-bus API used by onboard sensors |
| `src/power.rs` | CPU dynamic-frequency and always-responsive USB policy |
| `tools/backdrop-preview/` | host-side PNG preview of the clock artwork, sharing the firmware's modules |
| `espflash.toml` | 8 MB flash size and custom partition-table selection |
| `partitions.csv` | NVS, PHY, rollback metadata, and two safe OTA application slots |

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
when ESP-IDF peripheral locks require it while attached to a USB host. On
battery, the ceiling is 80 MHz. Power-source reads use ESP-IDF's cached USB SOF
state and do not block. Existing application events reconcile the CPU policy
and invalidate source-dependent UI state, so no polling timer is added.
Connected Wi-Fi uses maximum modem power saving while preserving the station
connection. If a saved network is unavailable, a non-blocking reconnect is
requested by a one-shot event after five minutes. ESP-IDF Wi-Fi/IP events update
link state immediately; signal strength and the remaining sensor state are
sampled on minute events. Pixel reconciliation suppresses the badge refresh
unless its rendered appearance changes. Automatic light sleep is intentionally
disabled because it would make the native USB Serial/JTAG command interface
intermittently unavailable.

While running on battery, the minute event also evaluates semantic low-battery
notifications. At 25% or below, a half-volume descending reminder repeats every
30 minutes. At 10% or below, a louder urgent sequence takes precedence and
repeats every five minutes. Both are suppressed outside 10:00-22:00 and whenever
external power is detected. Small recovery margins prevent ADC noise around a
threshold from repeatedly retriggering a new alert.

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

Set a manifest endpoint at compile time to enable update checks in a locally
built image. It can be hosted on GitHub, a CDN, or any HTTPS server. GitHub
Actions supplies a stable latest-release manifest URL automatically:

```sh
OTA_ENDPOINT=https://example.com/device/ota-manifest.json cargo build --release
```

Operational defaults are centralized in `src/config.rs`. The main network and
timing policies can be overridden through build environment variables:

| Variable | Default |
| --- | --- |
| `IP_LOCATION_ENDPOINT` | `https://ipwho.is/` |
| `WEATHER_ENDPOINT` | `https://api.open-meteo.com/v1/forecast` |
| `NTP_SERVER` | `0.pool.ntp.org` |
| `WEATHER_REFRESH_SECONDS` | `3600` |
| `WEATHER_RETRY_SECONDS` | `300` |
| `WIFI_RECONNECT_SECONDS` | `300` |
| `FULL_REFRESH_AFTER_PARTIALS` | `30` |
| `HTTP_TIMEOUT_SECONDS` / `HTTP_RESPONSE_LIMIT_BYTES` | `15` / `4096` |
| `BACKGROUND_TASK_STACK_SIZE` | `12288` |
| `INPUT_TASK_STACK_SIZE` | `4096` |
| `BUTTON_DEBOUNCE_MS` / `BUTTON_LONG_PRESS_SECONDS` | `30` / `2` |
| `OTA_CONFIRMATION_SECONDS` / `OTA_RESTART_SECONDS` | `60` / `3` |
| `OTA_MANIFEST_TIMEOUT_SECONDS` / `OTA_DOWNLOAD_TIMEOUT_SECONDS` | `15` / `5` |
| `BATTERY_ALERT_START_HOUR` / `BATTERY_ALERT_END_HOUR` | `10` / `22` |
| `BATTERY_WARNING_PERCENT` / `BATTERY_CRITICAL_PERCENT` | `25` / `10` |
| `BATTERY_WARNING_CLEAR_PERCENT` / `BATTERY_CRITICAL_CLEAR_PERCENT` | `28` / `12` |
| `BATTERY_WARNING_REPEAT_MINUTES` / `BATTERY_CRITICAL_REPEAT_MINUTES` | `30` / `5` |

For example:

```sh
NTP_SERVER=ntp.internal.example WEATHER_REFRESH_SECONDS=7200 cargo build --release
```

The daily SNTP polling interval remains an ESP-IDF setting in
`sdkconfig.defaults` (`CONFIG_LWIP_SNTP_UPDATE_DELAY=86400000`). Board wiring,
device addresses, display waveforms, and widget geometry stay close to their
drivers because changing them describes different hardware or layout rather
than deployment policy.

## Firmware updates and releases

Hold **BOOT + PWR** for two seconds to open the firmware update screen. The
button task uses GPIO edge interrupts and a one-shot gesture timeout; it does
not poll. Release lookup and download run on a dedicated worker so the event
loop and button handling remain responsive.

The endpoint returns a deliberately small host-neutral JSON document:

```json
{
  "schema_version": 1,
  "version": "0.2.0",
  "firmware_url": "https://cdn.example.com/esp-smart-eink-ota.bin",
  "size": 1415648,
  "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
}
```

Only semantic versions newer than the running package version are offered.
Press **BOOT** within one minute to install, or **PWR** to cancel. PWR also
cancels a download between chunks. The final validation and boot-selection
step is intentionally non-cancellable and is labelled "Do not power off". No
Wi-Fi, HTTP failures, invalid manifests, truncated or oversized downloads,
digest mismatches, invalid ESP images, flash errors, and unavailable OTA slots
all leave the current boot slot selected and show an error instead of rebooting.

The USB CLI can override the build default without reflashing. The override is
stored in NVS and takes effect on the next update check:

```text
OTA ENDPOINT GET
OTA ENDPOINT SET "https://example.com/device/ota-manifest.json"
OTA ENDPOINT CLEAR
```

The friendly host CLI exposes the same setting:

```sh
python3 scripts/device_cli.py ota endpoint get
python3 scripts/device_cli.py ota endpoint set https://example.com/device/ota-manifest.json
python3 scripts/device_cli.py ota endpoint clear
```

Only HTTPS endpoints and firmware URLs are accepted. `CLEAR` restores the
build-time `OTA_ENDPOINT`, or disables checks if the image has no default.

The download is written only to the inactive application slot. ESP-IDF validates
the completed image before `otadata` selects it for the next boot. Bootloader
rollback is enabled: the new image is marked healthy only after its first
successful display render; otherwise a subsequent reboot returns to the prior
slot.

The partition change from the original single `factory` application to
`otadata`, `ota_0`, and `ota_1` must be installed once over USB with the normal
`espflash flash` command. It cannot safely migrate itself while executing from
the old partition layout.

GitHub Actions provides two workflows:

- `Firmware CI` runs formatting, Clippy, and a release build for pull requests
  and `main`.
- `Firmware Release` is started manually with the version already present in
  `Cargo.toml`. It builds from `main`, creates `v<version>`, and publishes the
  OTA application image, `ota-manifest.json`, a merged factory image, the ELF,
  partition table, and SHA-256 checksums. Action dependencies and the ESP
  toolchain are pinned.

On GitHub, the compiled default points to
`releases/latest/download/ota-manifest.json`; the manifest then points to the
versioned firmware asset. Moving the manifest and binary to another host needs
no firmware logic change.

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
