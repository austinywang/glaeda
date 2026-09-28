from __future__ import annotations

import re
import subprocess
import sys
import threading
from pathlib import Path

OBSERVATION_ENV = {"LC_ALL": "C", "LANG": "C"}


def du_bytes(path: Path) -> int:
    if not path.exists():
        return 0
    result = subprocess.run(
        ["/usr/bin/du", "-sk", str(path)],
        env=OBSERVATION_ENV,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode == 0:
        try:
            return int(result.stdout.split()[0]) * 1024
        except (ValueError, IndexError):
            pass
    total = 0
    for candidate in path.rglob("*"):
        try:
            if candidate.is_file() and not candidate.is_symlink():
                total += candidate.stat().st_size
        except OSError:
            pass
    return total


def filesystem_type(path: Path) -> str | None:
    commands = (
        ["/usr/bin/stat", "-f", "-c", "%T", str(path)],
        ["/usr/bin/stat", "-f", "%T", str(path)],
    )
    for command in commands:
        try:
            result = subprocess.run(
                command,
                env=OBSERVATION_ENV,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=5,
                check=False,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        value = result.stdout.strip()
        if result.returncode == 0 and value:
            return value[:128]
    return None


def swap_used_bytes():
    if sys.platform.startswith("linux"):
        try:
            fields = {}
            for line in Path("/proc/meminfo").read_text().splitlines():
                name, rest = line.split(":", 1)
                fields[name] = int(rest.strip().split()[0]) * 1024
            return max(0, fields["SwapTotal"] - fields["SwapFree"])
        except (OSError, ValueError, KeyError):
            return None
    if sys.platform == "darwin":
        try:
            output = subprocess.check_output(
                ["/usr/sbin/sysctl", "-n", "vm.swapusage"],
                env=OBSERVATION_ENV,
                text=True,
                stderr=subprocess.DEVNULL,
            )
            match = re.search(r"used = ([0-9.]+)([MGT])", output)
            if match is None:
                return None
            return int(
                float(match.group(1))
                * {"M": 1024**2, "G": 1024**3, "T": 1024**4}[match.group(2)]
            )
        except (OSError, subprocess.SubprocessError):
            return None
    return None


def memory_psi():
    if not sys.platform.startswith("linux"):
        return None
    try:
        match = re.search(
            r"^some\s+avg10=([0-9.]+)",
            Path("/proc/pressure/memory").read_text(),
            re.M,
        )
        return float(match.group(1)) if match else None
    except (OSError, ValueError):
        return None


def temperature():
    values = []
    if sys.platform.startswith("linux"):
        for path in Path("/sys/class/thermal").glob("thermal_zone*/temp"):
            try:
                value = float(path.read_text().strip())
                values.append(value / 1000 if value > 1000 else value)
            except (OSError, ValueError):
                pass
    return max(values) if values else None


def aggregate_rss(root_pid: int):
    result = subprocess.run(
        ["/bin/ps", "-axo", "pid=,ppid=,rss="],
        env=OBSERVATION_ENV,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode:
        return None
    rows = []
    for line in result.stdout.splitlines():
        try:
            pid, ppid, rss = map(int, line.split())
            rows.append((pid, ppid, rss))
        except ValueError:
            pass
    descendants = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, ppid, _ in rows:
            if ppid in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True
    values = [rss for pid, _, rss in rows if pid in descendants]
    return sum(values) if values else None


class Sampler:
    def __init__(self, pid: int):
        self.pid = pid
        self.stop_event = threading.Event()
        self.peak_rss_kib = None
        self.swap_max_bytes = swap_used_bytes()
        self.psi_some_avg10_max = memory_psi()
        self.max_temperature_c = temperature()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _record(self, name: str, value):
        if value is None:
            return
        previous = getattr(self, name)
        setattr(self, name, value if previous is None else max(previous, value))

    def _sample_once(self):
        self._record("peak_rss_kib", aggregate_rss(self.pid))
        self._record("swap_max_bytes", swap_used_bytes())
        self._record("psi_some_avg10_max", memory_psi())
        self._record("max_temperature_c", temperature())

    def start(self):
        # Short useful-work commands can finish inside one 200 ms period. Take
        # one synchronous sample so those runs do not silently report no RSS.
        self._sample_once()
        self.thread.start()

    def stop(self):
        self.stop_event.set()
        self.thread.join(timeout=2)
        self._sample_once()

    def _run(self):
        while not self.stop_event.wait(0.2):
            self._sample_once()
