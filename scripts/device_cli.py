#!/usr/bin/env python3
"""Friendly USB command-line client for the ESP32-S3 e-paper dashboard."""

from __future__ import annotations

import argparse
import getpass
import glob
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Sequence


DEFAULT_BAUD = 115_200
DEFAULT_TIMEOUT_SECONDS = 35.0
RTC_SYNC_TRANSPORT_LEAD_SECONDS = 0.030
PORT_PATTERNS = (
    "/dev/cu.usbmodem*",
    "/dev/ttyACM*",
    "/dev/ttyUSB*",
    "/dev/cu.wchusbserial*",
)


class CliError(RuntimeError):
    """Expected user-facing command error."""


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Control the Waveshare ESP32-S3 e-paper dashboard over USB.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--port",
        help="serial device; omitted to autodetect a single connected ESP board",
    )
    parser.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help="seconds to wait for the device response",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the device command without opening a serial port",
    )

    commands = parser.add_subparsers(dest="command", required=True)

    time_parser = commands.add_parser("time", help="read or set the onboard RTC")
    time_commands = time_parser.add_subparsers(dest="time_command", required=True)
    time_commands.add_parser("get", help="read the RTC")
    time_commands.add_parser("sync", help="set the RTC to this computer's local time")
    time_set = time_commands.add_parser("set", help="set a specific local date and time")
    time_set.add_argument(
        "value",
        help='date/time such as "2026-08-11 18:45:00"',
    )
    time_commands.add_parser("calibration", help="read RTC crystal-drift correction")
    time_calibrate = time_commands.add_parser(
        "calibrate", help="apply PCF85063 crystal-drift correction"
    )
    calibration_source = time_calibrate.add_mutually_exclusive_group(required=True)
    calibration_source.add_argument(
        "--ppm",
        type=float,
        help="measured drift in ppm; positive means the RTC runs fast",
    )
    calibration_source.add_argument(
        "--drift-seconds",
        type=float,
        help="seconds gained (positive) or lost (negative) over the observation",
    )
    time_calibrate.add_argument(
        "--elapsed-hours",
        type=float,
        help="observation duration used with --drift-seconds",
    )

    wifi_parser = commands.add_parser("wifi", help="manage persistent Wi-Fi credentials")
    wifi_commands = wifi_parser.add_subparsers(dest="wifi_command", required=True)
    wifi_set = wifi_commands.add_parser("set", help="save credentials and connect")
    wifi_set.add_argument("ssid")
    password_group = wifi_set.add_mutually_exclusive_group()
    password_group.add_argument(
        "--password",
        help="password (prefer omitting this option to use the secure prompt)",
    )
    password_group.add_argument(
        "--open",
        action="store_true",
        help="configure an open network with an empty password",
    )
    wifi_commands.add_parser("status", help="show configured and connected state")
    wifi_commands.add_parser("scan", help="list nearby networks and signal strength")
    wifi_commands.add_parser("clear", help="disconnect and erase saved credentials")

    language_parser = commands.add_parser(
        "language", help="read or set the persistent display language"
    )
    language_commands = language_parser.add_subparsers(
        dest="language_command", required=True
    )
    language_commands.add_parser("get", help="show the active display language")
    language_set = language_commands.add_parser(
        "set", help="set the display language and refresh the screen"
    )
    language_set.add_argument("language", choices=("en", "ru"))

    ota_parser = commands.add_parser("ota", help="configure firmware update checks")
    ota_commands = ota_parser.add_subparsers(dest="ota_command", required=True)
    endpoint_parser = ota_commands.add_parser(
        "endpoint", help="read or override the OTA manifest endpoint"
    )
    endpoint_commands = endpoint_parser.add_subparsers(
        dest="endpoint_command", required=True
    )
    endpoint_commands.add_parser("get", help="show the effective manifest endpoint")
    endpoint_set = endpoint_commands.add_parser(
        "set", help="persist an HTTPS manifest endpoint override"
    )
    endpoint_set.add_argument("url")
    endpoint_commands.add_parser(
        "clear", help="restore the firmware's build-time endpoint"
    )

    commands.add_parser("status", help="show RTC and Wi-Fi status")
    commands.add_parser("refresh", help="request an immediate display refresh")
    audio_parser = commands.add_parser("audio", help="test the onboard speaker")
    audio_commands = audio_parser.add_subparsers(dest="audio_command", required=True)
    audio_beep = audio_commands.add_parser("beep", help="play a generated sine-wave tone")
    audio_beep.add_argument("--frequency", type=int, default=880, metavar="HZ")
    audio_beep.add_argument("--duration", type=int, default=500, metavar="MS")
    audio_beep.add_argument("--volume", type=int, default=45, metavar="PERCENT")
    audio_tone = audio_commands.add_parser("tone", help="play a generated waveform")
    audio_tone.add_argument("waveform", choices=("sine", "square", "triangle"))
    audio_tone.add_argument("--frequency", type=int, default=880, metavar="HZ")
    audio_tone.add_argument("--duration", type=int, default=500, metavar="MS")
    audio_tone.add_argument("--volume", type=int, default=45, metavar="PERCENT")
    commands.add_parser("help-device", help="request the firmware's command help")
    commands.add_parser("console", help="open an interactive serial console")
    return parser


def validate_datetime(value: str) -> str:
    normalized = value.strip().replace("T", " ")
    try:
        parsed = datetime.strptime(normalized, "%Y-%m-%d %H:%M:%S")
    except ValueError as error:
        raise CliError(f"invalid date/time: {error}") from error
    if not 2000 <= parsed.year <= 2099:
        raise CliError("RTC year must be between 2000 and 2099")
    return parsed.strftime("%Y-%m-%d %H:%M:%S")


def quote_device_argument(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def validate_wifi_value(value: str, label: str, minimum: int, maximum: int) -> None:
    encoded = value.encode("utf-8")
    if not minimum <= len(encoded) <= maximum:
        raise CliError(f"{label} must contain {minimum} to {maximum} UTF-8 bytes")
    if any(character in value for character in ("\0", "\r", "\n")):
        raise CliError(f"{label} cannot contain NUL or newline characters")


def validate_ota_endpoint(value: str) -> None:
    if not value.startswith("https://"):
        raise CliError("OTA endpoint must use HTTPS")
    if (
        len(value.encode("ascii", errors="ignore")) != len(value)
        or not 1 <= len(value) <= 512
    ):
        raise CliError("OTA endpoint must contain 1 to 512 ASCII bytes")
    if any(not character.isprintable() or character.isspace() for character in value):
        raise CliError("OTA endpoint cannot contain whitespace or control characters")
    if any(character in value for character in ('"', "\\")):
        raise CliError("OTA endpoint cannot contain quotes or backslashes")


def command_for(args: argparse.Namespace) -> tuple[str | None, int]:
    if args.command == "time":
        if args.time_command == "get":
            return "TIME GET", 1
        if args.time_command == "sync":
            return None, 0
        if args.time_command == "calibration":
            return "TIME CALIBRATION GET", 1
        if args.time_command == "calibrate":
            if args.ppm is not None:
                measured_ppm = args.ppm
            else:
                if args.elapsed_hours is None or args.elapsed_hours <= 0:
                    raise CliError(
                        "--elapsed-hours must be positive with --drift-seconds"
                    )
                measured_ppm = (
                    args.drift_seconds / (args.elapsed_hours * 3600.0) * 1_000_000.0
                )
            if not -277.76 <= measured_ppm <= 273.42:
                raise CliError("measured drift is outside the RTC correction range")
            return f"TIME CALIBRATION SET {measured_ppm:.6f}", 1
        return f"TIME SET {validate_datetime(args.value)}", 1

    if args.command == "wifi":
        if args.wifi_command == "status":
            return "WIFI STATUS", 1
        if args.wifi_command == "scan":
            return "WIFI SCAN", 0
        if args.wifi_command == "clear":
            return "WIFI CLEAR", 1

        if args.open:
            password = ""
        elif args.password is not None:
            password = args.password
        elif args.dry_run:
            # Dry runs must stay non-interactive and never invent a visible secret.
            password = "<prompted-password>"
        else:
            password = getpass.getpass("Wi-Fi password: ")

        validate_wifi_value(args.ssid, "SSID", 1, 32)
        validate_wifi_value(password, "password", 0, 64)
        return (
            f"WIFI SET {quote_device_argument(args.ssid)} "
            f"{quote_device_argument(password)}",
            1,
        )

    if args.command == "language":
        if args.language_command == "get":
            return "LANGUAGE GET", 1
        return f"LANGUAGE SET {args.language.upper()}", 1

    if args.command == "ota":
        if args.endpoint_command == "get":
            return "OTA ENDPOINT GET", 1
        if args.endpoint_command == "clear":
            return "OTA ENDPOINT CLEAR", 1
        validate_ota_endpoint(args.url)
        return f"OTA ENDPOINT SET {quote_device_argument(args.url)}", 1

    if args.command == "status":
        return "STATUS", 2
    if args.command == "refresh":
        return "REFRESH", 1
    if args.command == "audio":
        if not 100 <= args.frequency <= 5_000:
            raise CliError("frequency must be between 100 and 5000 Hz")
        if not 50 <= args.duration <= 5_000:
            raise CliError("duration must be between 50 and 5000 ms")
        if not 1 <= args.volume <= 80:
            raise CliError("volume must be between 1 and 80 percent")
        action = (
            "BEEP"
            if args.audio_command == "beep"
            else f"TONE {args.waveform.upper()}"
        )
        return f"AUDIO {action} {args.frequency} {args.duration} {args.volume}", 1
    if args.command == "help-device":
        return "HELP", 0
    if args.command == "console":
        return None, 0
    raise CliError(f"unsupported command: {args.command}")


def detect_port(explicit_port: str | None) -> str:
    if explicit_port:
        port = Path(explicit_port)
        if not port.exists():
            raise CliError(f"serial port does not exist: {port}")
        return str(port)

    matches = sorted(
        {
            candidate
            for pattern in PORT_PATTERNS
            for candidate in glob.glob(pattern)
        }
    )
    if not matches:
        raise CliError("no serial device found; connect the board or pass --port")
    if len(matches) > 1:
        choices = "\n  ".join(matches)
        raise CliError(f"multiple serial devices found; pass --port:\n  {choices}")
    return matches[0]


def open_serial(port: str, baud: int, timeout: float):
    try:
        import serial  # type: ignore[import-not-found]
    except ImportError as error:
        raise CliError(
            "pyserial is required; install it with "
            "`python3 -m pip install -r scripts/requirements.txt`"
        ) from error

    try:
        # Configure control lines before opening so USB-UART adapters don't
        # accidentally toggle the board into reset or download mode.
        connection = serial.Serial()
        connection.port = port
        connection.baudrate = baud
        connection.timeout = 0.2
        connection.write_timeout = 2.0
        connection.dtr = False
        connection.rts = False
        connection.open()
        return connection
    except serial.SerialException as error:
        raise CliError(f"cannot open {port}: {error}") from error


def send_command(
    connection,
    device_command: str,
    expected_responses: int,
    timeout: float,
) -> int:
    connection.reset_input_buffer()
    connection.write((device_command + "\r\n").encode("utf-8"))
    connection.flush()

    deadline = time.monotonic() + timeout
    responses = 0
    failed = False
    last_output_at: float | None = None
    while time.monotonic() < deadline:
        raw_line = connection.readline()
        if not raw_line:
            if (
                expected_responses == 0
                and last_output_at is not None
                and time.monotonic() - last_output_at >= 0.7
            ):
                return 1 if failed else 0
            continue
        line = raw_line.decode("utf-8", errors="replace").rstrip()
        if not line:
            continue
        last_output_at = time.monotonic()
        print(line)
        if line.startswith(("OK ", "ERR ")):
            responses += 1
            failed |= line.startswith("ERR ")
            if expected_responses and responses >= expected_responses:
                return 1 if failed else 0

    if expected_responses == 0:
        return 0
    raise CliError(
        f"timed out after {timeout:g}s waiting for the device response; "
        "check the port and firmware"
    )


def wait_for_device_ready(connection, timeout: float) -> None:
    """Wait through a native-USB reset until the Rust command task responds."""
    connection.reset_input_buffer()
    deadline = time.monotonic() + timeout
    next_probe_at = 0.0
    while time.monotonic() < deadline:
        now = time.monotonic()
        if now >= next_probe_at:
            connection.write(b"PING\r\n")
            connection.flush()
            next_probe_at = now + 1.0

        raw_line = connection.readline()
        if not raw_line:
            continue
        line = raw_line.decode("utf-8", errors="replace").strip()
        if line == "OK PONG":
            # More than one probe can be buffered while the firmware boots.
            # Drain their replies before sending the user's real command.
            quiet_until = time.monotonic() + 0.15
            while time.monotonic() < quiet_until:
                if connection.readline():
                    quiet_until = time.monotonic() + 0.05
            connection.reset_input_buffer()
            return
    raise CliError(
        f"device did not become ready within {timeout:g}s; check the port and firmware"
    )


def synchronize_rtc(connection, timeout: float) -> int:
    # Opening the ESP32-S3 native USB port can reset the board. Wait until all
    # startup work (including a Wi-Fi attempt) is over before sampling host time.
    if send_command(connection, "TIME GET", 1, timeout) != 0:
        return 1

    target = (datetime.now() + timedelta(seconds=1)).replace(microsecond=0)
    delay = (target - datetime.now()).total_seconds() - RTC_SYNC_TRANSPORT_LEAD_SECONDS
    if delay > 0:
        time.sleep(delay)

    print(f"Synchronizing RTC to {target:%Y-%m-%d %H:%M:%S}...")
    if send_command(connection, f"TIME SET {target:%Y-%m-%d %H:%M:%S}", 1, timeout) != 0:
        return 1
    # Read back immediately so the operator gets a second-resolution check.
    return send_command(connection, "TIME GET", 1, timeout)


def interactive_console(connection) -> int:
    print("Interactive console. Type device commands; Ctrl-C or Ctrl-D exits.")
    try:
        while True:
            try:
                line = input("eink> ")
            except EOFError:
                print()
                return 0
            if not line.strip():
                continue
            connection.write((line + "\r\n").encode("utf-8"))
            connection.flush()
            quiet_deadline = time.monotonic() + 0.6
            while time.monotonic() < quiet_deadline:
                raw_line = connection.readline()
                if raw_line:
                    print(raw_line.decode("utf-8", errors="replace").rstrip())
                    quiet_deadline = time.monotonic() + 0.2
    except KeyboardInterrupt:
        print()
        return 0


def run(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    device_command, expected_responses = command_for(args)

    if args.dry_run:
        if args.command == "time" and args.time_command == "sync":
            print("TIME SET <captured after device readiness, aligned to next host second>")
        elif device_command is None:
            print("CONSOLE")
        elif args.command == "wifi" and args.wifi_command == "set":
            # Never print a real password supplied to a dry run. An explicitly
            # open network has no secret, so show its exact command.
            password = "" if args.open else "<redacted>"
            print(
                f"WIFI SET {quote_device_argument(args.ssid)} "
                f"{quote_device_argument(password)}"
            )
        else:
            print(device_command)
        return 0

    port = detect_port(args.port)
    print(f"Connecting to {port} at {args.baud} baud...")
    with open_serial(port, args.baud, args.timeout) as connection:
        wait_for_device_ready(connection, args.timeout)
        if args.command == "time" and args.time_command == "sync":
            return synchronize_rtc(connection, args.timeout)
        if device_command is None:
            return interactive_console(connection)
        return send_command(
            connection,
            device_command,
            expected_responses,
            args.timeout,
        )


def main() -> int:
    try:
        return run()
    except CliError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("\nCancelled.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
