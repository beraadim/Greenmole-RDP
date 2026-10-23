#!/usr/bin/env python3
"""Explicit, opt-in session acceptance observer; never changes server policy."""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import sys
import time

from session_soak_evidence import CycleEvidence, LogTail, compare_snapshots
from session_soak_probes import ProbeUnavailable, bounded_command, terminate_owned

MAX_REPORT_BYTES = 8 * 1024 * 1024
MAX_RECORD_BYTES = 64 * 1024
HASH_FIELDS = {"process_generation", "physical_topology", "stable_monitor_identity",
               "monitor_registration_fingerprint"}
COUNT_FIELDS = {"process_rss_bytes", "process_tree_rss_bytes", "process_tree_count",
                "physical_display_count", "virtual_display_online_count", "monitor_registrations",
                "display_profile_entries", "macrdp_wake_assertions", "colorsync_process_count"}
PROBE_NAMES = {"process", "displays", "registrations", "identity", "wake", "macos_probes", "probe"}


def sanitize_snapshot(value):
    """Allowlist types and fields before anything can enter a saved report."""
    result = {}
    if not isinstance(value, dict):
        return {"unavailable": ["probe"]}
    for key in HASH_FIELDS:
        item = value.get(key)
        if isinstance(item, str) and re.fullmatch(r"[0-9a-f]{64}", item):
            result[key] = item
    for key in COUNT_FIELDS:
        item = value.get(key)
        if type(item) is int and 0 <= item < 2**60:
            result[key] = item
    if type(value.get("process_alive")) is bool:
        result["process_alive"] = value["process_alive"]
    cpu = value.get("colorsync_cpu_percent")
    if type(cpu) in (float, int) and math.isfinite(cpu) and 0 <= cpu <= 100000:
        result["colorsync_cpu_percent"] = cpu
    result["unavailable"] = sorted({name for name in value.get("unavailable", [])
                                    if isinstance(name, str) and name in PROBE_NAMES}) if isinstance(value.get("unavailable", []), list) else ["probe"]
    return result


class PrivateReport:
    """Exclusive private JSONL file; no app logs, commands or arbitrary metadata."""
    def __init__(self, path, byte_limit=MAX_REPORT_BYTES):
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        self.file = os.fdopen(fd, "wb")
        self.used, self.limit = 0, byte_limit

    def write(self, record):
        encoded = (json.dumps(record, sort_keys=True, allow_nan=False, separators=(",", ":")) + "\n").encode()
        if len(encoded) > MAX_RECORD_BYTES or self.used + len(encoded) > self.limit:
            raise ValueError("report_limit")
        self.file.write(encoded)
        self.file.flush()
        self.used += len(encoded)

    def close(self):
        self.file.close()


class Clock:
    now = staticmethod(time.monotonic)
    sleep = staticmethod(time.sleep)


class Observer:
    def __init__(self, pid, port):
        self.pid, self.port = pid, port

    def alive(self):
        try:
            os.kill(self.pid, 0)
            return True
        except ProcessLookupError:
            return False
        except PermissionError:
            return None

    def snapshot(self, timeout):
        try:
            data = bounded_command([sys.executable, str(Path(__file__).with_name("session_soak_probes.py")),
                                    str(self.pid), str(self.port)], timeout=max(.01, min(8, timeout)), limit=16384)
            return sanitize_snapshot(json.loads(data))
        except (ProbeUnavailable, OSError, ValueError, UnicodeError):
            return {"unavailable": ["probe"]}


class DriverError(Exception):
    pass


# The supervisor stays unreaped until acknowledged. This reserves its PGID
# while an abnormal action's descendants are terminated. The actual driver's
# stdout/stderr cannot reach this protocol or the report. Successful actions may
# intentionally leave a configured client alive for the later disconnect action.
DRIVER_SUPERVISOR = """
import os, subprocess, sys
try:
    status = subprocess.call(sys.argv[1:], stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
except OSError:
    status = 127
os.write(1, b'0\\n' if status == 0 else b'1\\n')
sys.stdin.buffer.read(1)
"""


class CycleDriver:
    """Only invokes an explicitly supplied, same-user local executable."""
    def __init__(self, executable):
        path = Path(executable)
        if not path.is_absolute():
            raise ValueError("driver_unavailable")
        info = path.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o022 or not os.access(path, os.X_OK)):
            raise ValueError("driver_unavailable")
        self.path = str(path)

    def run(self, action, cycle, timeout):
        if action not in ("connect", "disconnect") or not 1 <= cycle <= 1000:
            raise ValueError("driver_action")
        process = None
        selector = selectors.DefaultSelector()
        try:
            process = subprocess.Popen([sys.executable, "-c", DRIVER_SUPERVISOR,
                                        self.path, action, str(cycle)], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                       start_new_session=True)
            selector.register(process.stdout, selectors.EVENT_READ)
            deadline, status = time.monotonic() + max(.01, timeout), bytearray()
            while status != b"0\n":
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise DriverError("driver_timeout")
                if selector.select(min(remaining, .1)):
                    part = os.read(process.stdout.fileno(), 3)
                    status.extend(part)
                    if not part or len(status) > 2 or status == b"1\n":
                        raise DriverError("driver_failed")
            process.stdin.write(b"1")
            process.stdin.flush()
            process.stdin.close()
            if process.wait(timeout=max(.01, deadline - time.monotonic())) != 0:
                raise DriverError("driver_failed")
        except subprocess.TimeoutExpired:
            raise DriverError("driver_timeout") from None
        except OSError:
            raise DriverError("driver_unavailable") from None
        finally:
            selector.close()
            stopped = True
            if process is not None:
                try:
                    if process.returncode is None:
                        stopped = terminate_owned(process)
                finally:
                    if not process.stdin.closed:
                        process.stdin.close()
                    process.stdout.close()
            if not stopped:
                # Fixed status only; no failed OS call or driver output.
                raise DriverError("driver_cleanup_unconfirmed")


@dataclass
class Options:
    cycles: int = 10
    connect_timeout: float = 60
    cleanup_timeout: float = 45
    active_seconds: float = 5
    max_session_seconds: float = 300
    max_run_seconds: float = 3600
    action_timeout: float = 30
    poll_seconds: float = .5
    settle_seconds: float = 2
    rss_limit_bytes: int = 64 * 1024 * 1024
    encoder_mode: str = "hardware"
    backend: str = "virtual"


def outcome(checks):
    return "fail" if "fail" in checks.values() else "unknown" if "unknown" in checks.values() else "pass"


class Runner:
    def __init__(self, options, tail, observer, report, driver=None, clock=None, notify=print):
        self.options, self.tail, self.observer, self.report = options, tail, observer, report
        self.driver, self.clock, self.notify = driver, clock or Clock(), notify
        self.baseline = None
        self.current, self.disconnect_attempted = 0, False
        self.disconnect_result = "not_requested"
        self.connection_requested = False
        self.completed = 0
        self.run_deadline = 0
        self.gaps = tail.gaps

    def remaining(self, deadline):
        return max(0, min(deadline, self.run_deadline) - self.clock.now())

    def wait(self, deadline):
        self.clock.sleep(min(self.options.poll_seconds, self.remaining(deadline)))

    def read(self, evidence):
        for record in self.tail.poll():
            evidence.consume(record)
        if self.tail.gaps != self.gaps:
            evidence.errors.add("log_evidence_gap")

    def checks(self, snapshot):
        checks = compare_snapshots(self.baseline, snapshot, self.options.rss_limit_bytes)
        if self.options.backend == "main":
            checks.pop("stable_monitor_identity", None)
        return checks

    def action(self, action):
        if not self.driver:
            return
        if action == "connect":
            self.connection_requested = True
        else:
            self.disconnect_attempted = True
            self.disconnect_result = "unconfirmed"
        # A run deadline stops new work. The driver's final disconnect still
        # receives its bounded grace period, rather than an unusable 10 ms.
        timeout = self.options.action_timeout if action == "disconnect" else min(
            self.options.action_timeout, max(.01, self.remaining(self.run_deadline)))
        self.driver.run(action, self.current, timeout)
        if action == "disconnect":
            self.disconnect_result = "requested"

    def disconnect_on_exit(self):
        # Only the explicit user-owned driver's own connection can be closed.
        # No process-name kills, input injection, server actions or GUI scripts.
        if self.disconnect_attempted:
            return self.disconnect_result
        if self.driver and self.connection_requested:
            self.disconnect_attempted = True
            try:
                self.driver.run("disconnect", self.current, self.options.action_timeout)
                return "requested"
            except (DriverError, KeyboardInterrupt):
                return "unconfirmed"
        return "not_requested"

    def write_cycle(self, status, reason, evidence, snapshot=None, checks=None):
        self.report.write({"kind": "cycle", "cycle": self.current, "status": status,
                           "reason": reason, "evidence": evidence.snapshot(),
                           "snapshot": snapshot or {}, "checks": checks or {}})
        self.notify(f"Cycle {self.current}: {status} ({reason}).")
        return status

    def baseline_check(self):
        evidence = CycleEvidence(self.options.encoder_mode)
        first = self.observer.snapshot(self.remaining(self.run_deadline))
        self.baseline = first
        until = min(self.run_deadline, self.clock.now() + self.options.settle_seconds)
        while self.remaining(until):
            self.read(evidence)
            if evidence.connected or evidence.errors:
                return ("fail", "baseline_session_activity") if evidence.connected else ("unknown", "baseline_log_gap")
            self.wait(until)
        self.read(evidence)
        second = self.observer.snapshot(self.remaining(self.run_deadline))
        checks = self.checks(second)
        status = outcome(checks)
        if evidence.connected or evidence.errors:
            status = "fail" if evidence.connected else "unknown"
        self.report.write({"kind": "baseline", "status": status,
                           "snapshot": first, "settled_snapshot": second, "checks": checks})
        return status, "baseline_verified" if status == "pass" else "baseline_unconfirmed"

    def cycle(self):
        options = self.options
        evidence = CycleEvidence(options.encoder_mode)
        if not self.remaining(self.run_deadline):
            return self.write_cycle("unknown", "run_deadline", evidence)
        # Read old between-cycle output before the next connection: it cannot
        # satisfy this cycle. Fresh events are required after this boundary.
        self.tail.poll()
        self.gaps = self.tail.gaps
        self.action("connect")
        self.notify(f"Cycle {self.current}: waiting for a new authenticated connection.")
        until = min(self.run_deadline, self.clock.now() + options.connect_timeout)
        while self.remaining(until):
            self.read(evidence)
            if self.observer.alive() is False:
                return self.write_cycle("fail", "server_process_exited", evidence)
            if evidence.errors:
                return self.write_cycle("fail" if evidence.errors != {"log_evidence_gap"} else "unknown", "lifecycle_error", evidence)
            if evidence.connected:
                break
            self.wait(until)
        if not evidence.connected:
            return self.write_cycle("unknown", "connection_not_observed", evidence)
        active_limit = min(self.run_deadline, self.clock.now() + (
            options.active_seconds if self.driver else options.max_session_seconds))
        while self.remaining(active_limit):
            self.read(evidence)
            if self.observer.alive() is False:
                return self.write_cycle("fail", "server_process_exited", evidence)
            if evidence.disconnected:
                if self.driver:
                    return self.write_cycle("fail", "unexpected_disconnect", evidence)
                break
            if evidence.errors:
                return self.write_cycle("unknown" if evidence.errors == {"log_evidence_gap"} else "fail", "lifecycle_error", evidence)
            self.wait(active_limit)
        if self.driver:
            self.action("disconnect")
        elif not evidence.disconnected:
            return self.write_cycle("unknown", "disconnect_not_observed", evidence)
        self.notify(f"Cycle {self.current}: waiting for cleanup and stable idle resources.")
        until = min(self.run_deadline, self.clock.now() + options.cleanup_timeout)
        snapshot, checks, stable_since = {}, {}, None
        next_snapshot = self.clock.now()
        while self.remaining(until):
            self.read(evidence)
            if self.observer.alive() is False:
                return self.write_cycle("fail", "server_process_exited", evidence)
            if evidence.errors:
                return self.write_cycle("unknown" if evidence.errors == {"log_evidence_gap"} else "fail", "lifecycle_error", evidence)
            if self.clock.now() >= next_snapshot:
                snapshot = self.observer.snapshot(self.remaining(until))
                checks = self.checks(snapshot)
                next_snapshot = self.clock.now() + max(1, options.settle_seconds)
                if evidence.complete() and outcome(checks) == "pass":
                    if stable_since is not None and self.clock.now() - stable_since >= options.settle_seconds:
                        return self.write_cycle("pass", "cleanup_and_resources_verified", evidence, snapshot, checks)
                    stable_since = self.clock.now()
                else:
                    stable_since = None
            self.wait(until)
        status = "fail" if outcome(checks) == "fail" else "unknown"
        return self.write_cycle(status, "cleanup_did_not_settle", evidence, snapshot, checks)

    def run(self):
        self.run_deadline = self.clock.now() + self.options.max_run_seconds
        status, reason, recovery = "unknown", "run_incomplete", "not_requested"
        self.report.write({"kind": "run", "schema": 1,
                           "started_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                           "mode": "automated" if self.driver else "manual", "cycles_requested": self.options.cycles,
                           "encoder_mode": self.options.encoder_mode, "backend": self.options.backend,
                           "rss_growth_limit_bytes": self.options.rss_limit_bytes})
        try:
            status, reason = self.baseline_check()
            if status == "pass":
                for cycle in range(1, self.options.cycles + 1):
                    self.current, self.disconnect_attempted, self.connection_requested = cycle, False, False
                    self.disconnect_result = "not_requested"
                    status = self.cycle()
                    reason = "cycles_verified" if status == "pass" else "cycle_unconfirmed"
                    if status != "pass":
                        break
                    self.completed += 1
                if self.completed != self.options.cycles and status == "pass":
                    status, reason = "unknown", "run_deadline"
        except KeyboardInterrupt:
            status, reason = "interrupted", "user_interruption"
        except DriverError as error:
            status, reason = "fail", str(error)
        except (OSError, ValueError):
            # Even an unwritable/full report must not bypass the driver's
            # bounded disconnect request for a connection we requested.
            status, reason = "unknown", "observation_or_report_unavailable"
        finally:
            if status != "pass":
                recovery = self.disconnect_on_exit()
        self.report.write({"kind": "result", "status": status, "reason": reason,
                           "cycles_completed": self.completed, "disconnect_on_exit": recovery})
        self.notify(f"Result: {status}; {self.completed}/{self.options.cycles} verified cycles.")
        return {"pass": 0, "fail": 1, "unknown": 2, "interrupted": 3}[status]


def bounded_number(minimum, maximum, integral=False):
    def parse(value):
        try:
            result = int(value) if integral else float(value)
            if not math.isfinite(result) or not minimum <= result <= maximum:
                raise ValueError()
            return result
        except ValueError:
            raise argparse.ArgumentTypeError(f"must be between {minimum} and {maximum}") from None
    return parse


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog="No mode starts by default. Read docs/session-soak.md before using real clients.")
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--manual", action="store_true", help="observe user-performed connect/disconnect cycles")
    modes.add_argument("--driver", metavar="EXECUTABLE", help="explicit same-user local connect/disconnect driver")
    parser.add_argument("--confirm-idle", action="store_true", required=True, help="confirm no existing remote client and only one macrdp server")
    parser.add_argument("--pid", type=bounded_number(1, 2**31 - 1, True), required=True, help="running macrdp UI/server root PID")
    parser.add_argument("--port", type=bounded_number(1, 65535, True), default=3389)
    parser.add_argument("--log", type=Path, default=Path.home() / "Library/Application Support/macrdp/macrdp-logs.jsonl")
    parser.add_argument("--output", type=Path, required=True, help="new private report; existing files are never replaced")
    parser.add_argument("--cycles", type=bounded_number(1, 1000, True), default=10)
    parser.add_argument("--encoder-mode", choices=("hardware", "software", "bitmap"), default="hardware")
    parser.add_argument("--backend", choices=("virtual", "main"), default="virtual")
    for name, default, lower, upper in (("connect-timeout", 60, 1, 300), ("cleanup-timeout", 45, 2, 300),
            ("active-seconds", 5, .1, 300), ("max-session-seconds", 300, 1, 3600),
            ("max-run-seconds", 3600, 10, 86400), ("action-timeout", 30, .1, 120),
            ("poll-seconds", .5, .1, 5), ("settle-seconds", 2, 1, 30)):
        parser.add_argument("--" + name, type=bounded_number(lower, upper), default=default)
    parser.add_argument("--max-rss-growth-mib", type=bounded_number(0, 4096), default=64)
    args = parser.parse_args(argv)
    if sys.platform != "darwin":
        parser.exit(2, "Read-only native observations require macOS; synthetic tests run on other platforms.\n")
    report = None
    try:
        driver = CycleDriver(args.driver) if args.driver else None
        tail = LogTail(args.log)
        options = Options(**{name: getattr(args, name) for name in Options.__dataclass_fields__ if name != "rss_limit_bytes"},
                          rss_limit_bytes=int(args.max_rss_growth_mib * 1024 * 1024))
        report = PrivateReport(args.output)
        return Runner(options, tail, Observer(args.pid, args.port), report, driver).run()
    except (OSError, ValueError):
        # Never interpolate exception text, paths or subprocess output.
        print("Unable to complete observations or create a private bounded report. No verified result.", file=sys.stderr)
        return 2
    finally:
        if report:
            report.close()


if __name__ == "__main__":
    raise SystemExit(main())
