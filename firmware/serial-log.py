#!/usr/bin/env python3
"""Record the robot's serial console for hours, and summarise a recording.

    python3 -I firmware/serial-log.py                 record until Ctrl-C
    python3 -I firmware/serial-log.py --port /dev/... the port, when several are plugged in
    python3 -I firmware/serial-log.py --summary FILE  memory trend, reboots, errors

Each run writes out/serial/robot-YYYYMMDD-HHMMSS.log, one line per console line
with the Mac's time in front. The port is opened with DTR/RTS low so opening it
does not reset the robot. When the port goes away (robot reset, cable pulled)
the logger notes it and opens it again when it comes back.

While it runs it holds the port: flash.sh refuses to flash until it is stopped
(out/serial/serial-log.pid).
"""

import argparse
import os
import re
import signal
import sys
import time
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "out" / "serial"
PID = OUT / "serial-log.pid"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
HEALTH = re.compile(
    r"health: up (\d+)s \| internal free (\d+) min (\d+) largest (\d+) \| dma largest (\d+) \| "
    r"psram free (\d+) min (\d+) largest (\d+) \| tasks (\d+)"
)
BOOT = re.compile(r"botstack firmware running from \S+, reset: (.+)$")
PANIC = re.compile(r"Guru Meditation|abort\(\) was called|Backtrace:|stack overflow|task_wdt", re.I)
LEVEL = re.compile(r"^([EW]) \(\d+\) (\S+): (.*)$")


def record(port: str) -> None:
    import serial  # pyserial

    OUT.mkdir(parents=True, exist_ok=True)
    if PID.exists():
        try:
            other = int(PID.read_text())
            os.kill(other, 0)
            sys.exit(f"a serial logger already runs (pid {other})")
        except (ValueError, ProcessLookupError):
            pass
    PID.write_text(str(os.getpid()))
    path = OUT / time.strftime("robot-%Y%m%d-%H%M%S.log")
    print(f"recording {port} to {path} (Ctrl-C to stop)")
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

    def stamp() -> str:
        now = time.time()
        return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(now)) + f".{int(now % 1 * 1000):03d} "

    try:
        with open(path, "a", encoding="utf-8") as log:
            lost = False
            while True:
                link = serial.Serial()
                link.port, link.baudrate, link.timeout = port, 115200, 1
                link.dtr = False
                link.rts = False
                try:
                    link.open()
                except (serial.SerialException, OSError):
                    if not lost:
                        log.write(stamp() + "--- port unavailable, waiting\n")
                        log.flush()
                        lost = True
                    time.sleep(2)
                    continue
                if lost:
                    log.write(stamp() + "--- port back\n")
                    lost = False
                try:
                    while True:
                        line = link.readline()
                        if line:
                            text = ANSI.sub("", line.decode("utf-8", "replace")).rstrip("\r\n")
                            log.write(stamp() + text + "\n")
                            log.flush()
                except (serial.SerialException, OSError):
                    log.write(stamp() + "--- port lost\n")
                    log.flush()
                    lost = True
                finally:
                    link.close()
    except KeyboardInterrupt:
        pass
    finally:
        PID.unlink(missing_ok=True)
        print(f"stopped; summary: python3 -I {Path(__file__).relative_to(ROOT)} --summary {path}")


def slope_per_hour(points: list[tuple[float, float]]) -> float:
    """Least-squares slope, bytes per hour."""
    n = len(points)
    mean_t = sum(t for t, _ in points) / n
    mean_v = sum(v for _, v in points) / n
    var = sum((t - mean_t) ** 2 for t, _ in points)
    if var == 0:
        return 0.0
    return sum((t - mean_t) * (v - mean_v) for t, v in points) / var * 3600


def summary(path: Path) -> None:
    health, boots, panics = [], [], []
    errors, warnings = Counter(), Counter()
    first = last = None
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        when, _, line = raw[:23], raw[23:24], raw[24:]
        first = first or when
        last = when
        if m := HEALTH.search(line):
            health.append((when, [int(x) for x in m.groups()]))
        elif m := BOOT.search(line):
            boots.append((when, m.group(1)))
        if PANIC.search(line):
            panics.append((when, line.strip()))
        if m := LEVEL.match(line):
            message = re.sub(r"\d+", "N", m.group(3))[:100]
            key = f"{m.group(2)}: {message}"
            (errors if m.group(1) == "E" else warnings)[key] += 1

    print(f"{path.name}: {first} -> {last}")
    print(f"boots: {len(boots)}")
    for when, reason in boots:
        print(f"  {when}  reset: {reason}")
    if panics:
        print(f"crash lines: {len(panics)}")
        for when, line in panics[:20]:
            print(f"  {when}  {line[:120]}")

    # Trends per boot: uptime resets, so a single fit across reboots means nothing.
    runs, run = [], []
    for when, values in health:
        if run and values[0] < run[-1][1][0]:
            runs.append(run)
            run = []
        run.append((when, values))
    if run:
        runs.append(run)
    names = ["internal free", "internal min", "internal largest", "dma largest", "psram free", "psram min",
             "psram largest"]
    for i, run in enumerate(runs, 1):
        hours = (run[-1][1][0] - run[0][1][0]) / 3600
        print(f"\nrun {i}: {len(run)} health lines, {hours:.1f} h of uptime ({run[0][0]} -> {run[-1][0]})")
        if len(run) < 3:
            continue
        print(f"  {'':<17} {'first':>9} {'last':>9} {'lowest':>9} {'trend/h':>9}")
        for k, name in enumerate(names, start=1):
            series = [(v[0], v[k]) for _, v in run]
            values = [v for _, v in series]
            print(f"  {name:<17} {values[0]:>9} {values[-1]:>9} {min(values):>9} {slope_per_hour(series):>+9.0f}")
        tasks = [v[8] for _, v in run]
        print(f"  tasks             {tasks[0]:>9} {tasks[-1]:>9} {min(tasks):>9}")
    if not health:
        print("\nno health lines (firmware older than the health log?)")

    for title, counter in (("errors", errors), ("warnings", warnings)):
        if counter:
            print(f"\n{title}: {sum(counter.values())}")
            for key, count in counter.most_common(15):
                print(f"  {count:>6}  {key}")


def find_port() -> str:
    """The one USB serial device plugged in, like scripts/usb-port.sh."""
    import glob

    patterns = (
        "/dev/cu.usbmodem*",
        "/dev/cu.usbserial*",
        "/dev/cu.wchusbserial*",
        "/dev/cu.SLAB_USBtoUART*",
        "/dev/ttyACM*",
        "/dev/ttyUSB*",
    )
    ports = sorted({p for pattern in patterns for p in glob.glob(pattern)})
    if len(ports) == 1:
        return ports[0]
    if not ports:
        sys.exit("no USB serial device: plug the robot's USB cable, or give --port (or PORT=...)")
    sys.exit("several USB serial devices; pick one with --port (or PORT=...):\n  " + "\n  ".join(ports))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", default=os.environ.get("PORT"), help="default: $PORT, else the one USB serial device")
    parser.add_argument("--summary", type=Path, metavar="FILE", help="summarise a recording instead")
    args = parser.parse_args()
    if args.summary:
        summary(args.summary)
    else:
        record(args.port or find_port())


if __name__ == "__main__":
    main()
