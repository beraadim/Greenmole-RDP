"""Synthetic acceptance tests. These never connect, change displays or power state."""
from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest

from session_soak import (CycleDriver, DriverError, Options, PrivateReport, Runner,
                          sanitize_snapshot)
from session_soak_evidence import CycleEvidence, LogTail, compare_snapshots
from session_soak_probes import ProbeUnavailable, bounded_command, stable_identity, wake_count


def event(component, phase, state, worker=100, **extra):
    return {"target": "macrdp_transition", "fields": {"component": component, "phase": phase,
            "state": state, "worker_pid": worker, **extra}}


def connected(worker=100):
    return [event("private_guard", "protection", "not_required", worker),
            event("session", "connection", "connected", worker),
            event("video_encoder", "vt_create", "begin", worker, retirement_id=7)]


def cleaned(worker=100):
    # Includes the real diagnostic outcomes that do not mean native failure.
    capture = lambda phase, state, operation, **extra: event("native_capture", phase, state,
        worker, owner_id=1, operation_id=operation, **extra)
    return [capture("stop", "begin", 1), capture("stream_drop", "begin", 2),
            capture("stream_drop", "end", 2, outcome="returned"),
            capture("stop", "end", 1, outcome="recovered"),
            capture("release_display_owner", "begin", 3),
            capture("restore_display", "begin", 4),
            capture("restore_display", "end", 4, outcome="ok"),
            capture("release_display_owner", "end", 3, outcome="returned"),
            event("video_encoder", "retirement", "queued", worker, retirement_id=7),
            event("video_encoder", "retirement", "begin", worker, retirement_id=7),
            event("video_encoder", "retirement", "end", worker, retirement_id=7, outcome="ok"),
            event("wake_lease", "cleanup", "complete", worker),
            event("session", "connection", "disconnected", worker)]


def resources(**overrides):
    result = {key: "a" * 64 for key in ("process_generation", "physical_topology",
             "stable_monitor_identity", "monitor_registration_fingerprint")}
    result.update(process_alive=True, process_rss_bytes=1000000, process_tree_rss_bytes=2000000,
                  process_tree_count=2, monitor_registrations=4, display_profile_entries=4,
                  physical_display_count=3, virtual_display_online_count=0,
                  macrdp_wake_assertions=0, colorsync_process_count=2, colorsync_cpu_percent=.2,
                  unavailable=[])
    result.update(overrides)
    return result


class MemoryReport:
    def __init__(self):
        self.rows = []
    def write(self, row):
        self.rows.append(json.loads(json.dumps(row)))


class FakeClock:
    def __init__(self):
        self.at = 1.
        self.on_sleep = lambda: None
    def now(self):
        return self.at
    def sleep(self, seconds):
        self.at += max(.0001, seconds)
        self.on_sleep()


class FakeTail:
    def __init__(self):
        self.queue, self.gaps = [], 0
    def poll(self):
        rows, self.queue = self.queue, []
        return rows


class FakeObserver:
    def __init__(self):
        self.get = lambda: resources()
        self.live = True
    def alive(self):
        return self.live
    def snapshot(self, _timeout):
        return self.get()


class FakeDriver:
    def __init__(self, tail):
        self.tail, self.calls = tail, []
        self.connect = lambda: tail.queue.extend(connected())
        self.disconnect = lambda: tail.queue.extend(cleaned())
    def run(self, action, cycle, _timeout):
        self.calls.append((action, cycle))
        getattr(self, action)()


class HarnessTests(unittest.TestCase):
    def setup_runner(self, cycles=2, manual=False):
        self.tail, self.observer, self.report, self.clock = FakeTail(), FakeObserver(), MemoryReport(), FakeClock()
        self.driver = None if manual else FakeDriver(self.tail)
        options = Options(cycles=cycles, connect_timeout=1, cleanup_timeout=3, active_seconds=.2,
                          max_session_seconds=1, max_run_seconds=30, action_timeout=.2,
                          poll_seconds=.1, settle_seconds=.2)
        self.runner = Runner(options, self.tail, self.observer, self.report, self.driver, self.clock,
                             notify=lambda _: None)
        return self.runner

    def result(self):
        return self.report.rows[-1]

    def test_repeated_automated_cycles_require_real_lifecycle_proof(self):
        self.assertEqual(self.setup_runner().run(), 0)
        self.assertEqual(self.driver.calls, [("connect", 1), ("disconnect", 1), ("connect", 2), ("disconnect", 2)])
        self.assertEqual(self.result()["cycles_completed"], 2)
        self.assertTrue(all(row["evidence"]["display_restoration_observed"]
                            for row in self.report.rows if row["kind"] == "cycle"))

    def test_successful_driver_without_connection_is_unknown_and_disconnect_requested(self):
        self.setup_runner()
        self.driver.connect = lambda: None
        self.assertEqual(self.runner.run(), 2)
        self.assertEqual(self.driver.calls, [("connect", 1), ("disconnect", 1)])
        self.assertEqual(self.result()["disconnect_on_exit"], "requested")

    def test_missing_capture_or_encoder_cleanup_never_passes(self):
        for omitted in ("native_capture", "video_encoder", "wake_lease"):
            with self.subTest(omitted=omitted):
                self.setup_runner()
                self.driver.disconnect = lambda omitted=omitted: self.tail.queue.extend(
                    row for row in cleaned() if row["fields"]["component"] != omitted)
                self.assertEqual(self.runner.run(), 2)
                self.assertEqual(self.result()["cycles_completed"], 0)

    def test_known_native_restore_failure_fails(self):
        self.setup_runner()
        sequence = cleaned()
        next(row["fields"] for row in sequence if row["fields"].get("phase") == "restore_display"
             and row["fields"]["state"] == "end")["outcome"] = "error"
        self.driver.disconnect = lambda: self.tail.queue.extend(sequence)
        self.assertEqual(self.runner.run(), 1)

    def test_crash_and_unexpected_disconnect_fail(self):
        self.setup_runner()
        self.driver.connect = lambda: setattr(self.observer, "live", False)
        self.assertEqual(self.runner.run(), 1)
        self.setup_runner()
        self.driver.connect = lambda: self.tail.queue.extend(connected() + cleaned())
        self.assertEqual(self.runner.run(), 1)

    def test_interruption_attempts_only_own_driver_disconnect(self):
        self.setup_runner()
        def interrupted():
            raise KeyboardInterrupt()
        self.driver.connect = interrupted
        self.assertEqual(self.runner.run(), 3)
        self.assertEqual(self.driver.calls, [("connect", 1), ("disconnect", 1)])

    def test_driver_disconnect_timeout_is_reported_unconfirmed(self):
        self.setup_runner()
        def timeout():
            raise DriverError("driver_timeout")
        self.driver.disconnect = timeout
        self.assertEqual(self.runner.run(), 1)
        self.assertEqual(self.result()["disconnect_on_exit"], "unconfirmed")

    def test_original_baseline_detects_gradual_growth(self):
        self.setup_runner()
        def growing():
            ends = sum(action == "disconnect" for action, _ in self.driver.calls)
            return resources(process_rss_bytes=1000000 + ends * 40 * 1024**2)
        self.observer.get = growing
        self.assertEqual(self.runner.run(), 1)
        self.assertEqual(self.result()["cycles_completed"], 1)
        last = [row for row in self.report.rows if row["kind"] == "cycle"][-1]
        self.assertEqual(last["checks"]["process_rss_bytes"], "fail")

    def test_missing_probe_data_is_unknown_not_zero(self):
        self.setup_runner()
        self.observer.get = lambda: {"process_alive": True, "unavailable": ["probe"]}
        self.assertEqual(self.runner.run(), 2)
        self.assertEqual(self.driver.calls, [])
        before = resources()
        before.pop("macrdp_wake_assertions")
        self.assertEqual(compare_snapshots(before, resources(), 1024)["wake_assertions"], "unknown")

    def test_report_failure_still_requests_disconnect(self):
        self.setup_runner()
        original = self.report.write
        def unavailable(row):
            if row["kind"] == "cycle":
                raise OSError()
            original(row)
        self.report.write = unavailable
        self.driver.connect = lambda: None
        self.assertEqual(self.runner.run(), 2)
        self.assertEqual(self.driver.calls, [("connect", 1), ("disconnect", 1)])

    def test_deadline_preserves_bounded_disconnect_grace(self):
        self.setup_runner()
        self.runner.run_deadline, self.runner.current = self.clock.now() - 1, 1
        received = []
        self.driver.run = lambda action, cycle, timeout: received.append((action, cycle, timeout))
        self.runner.action("disconnect")
        self.assertEqual(received, [("disconnect", 1, self.runner.options.action_timeout)])

    def test_raw_log_secrets_are_not_reported(self):
        self.setup_runner(cycles=1)
        sequence = connected()
        sequence[1]["message"] = "https://user:secret@example.test"
        sequence[1]["fields"]["password"] = "never-save"
        self.driver.connect = lambda: self.tail.queue.extend(sequence)
        self.assertEqual(self.runner.run(), 0)
        encoded = json.dumps(self.report.rows)
        self.assertNotIn("secret", encoded)
        self.assertNotIn("never-save", encoded)

    def test_log_loss_invalidates_cycle(self):
        self.setup_runner()
        def lost():
            self.tail.queue.extend(connected())
            self.tail.gaps += 1
        self.driver.connect = lost
        self.assertEqual(self.runner.run(), 2)

    def test_manual_mode_observes_but_invokes_no_driver(self):
        runner = self.setup_runner(cycles=1, manual=True)
        def user_events():
            if runner.current and not getattr(self, "sent_connect", False):
                self.sent_connect = True
                self.tail.queue.extend(connected())
            elif getattr(self, "sent_connect", False) and not getattr(self, "sent_end", False):
                self.sent_end = True
                self.tail.queue.extend(cleaned())
        self.clock.on_sleep = user_events
        self.assertEqual(runner.run(), 0)
        self.assertIsNone(runner.driver)


class EvidenceTests(unittest.TestCase):
    def test_all_observed_encoder_owners_must_retire(self):
        evidence = CycleEvidence()
        for row in connected() + [event("video_encoder", "vt_create", "begin", retirement_id=8)] + cleaned():
            evidence.consume(row)
        self.assertFalse(evidence.complete())
        self.assertEqual(evidence.snapshot()["pending_encoder_retirements"], 1)
        evidence.consume(event("video_encoder", "retirement", "end", retirement_id=8, outcome="ok"))
        self.assertTrue(evidence.complete())

    def test_preconnection_encoder_owner_is_not_lost(self):
        evidence = CycleEvidence()
        evidence.consume(event("video_encoder", "vt_create", "begin", retirement_id=8))
        for row in connected() + cleaned():
            evidence.consume(row)
        self.assertFalse(evidence.complete())

    def test_stale_unmatched_or_unrelated_cleanup_cannot_satisfy_proof(self):
        evidence = CycleEvidence()
        for row in cleaned() + connected() + cleaned(worker=999):
            evidence.consume(row)
        self.assertFalse(evidence.complete())
        evidence.consume(event("native_capture", "restore_display", "end", owner_id=1, operation_id=9, outcome="ok"))
        self.assertFalse(evidence.display_restored)
        forged = event("wake_lease", "cleanup", "complete")
        forged["target"] = "unrelated"
        evidence.consume(forged)
        self.assertFalse(evidence.wake_released)

    def test_failed_create_is_unconfirmed_not_false_cleanup_success(self):
        evidence = CycleEvidence()
        for row in connected() + [event("video_encoder", "vt_create", "end", retirement_id=7, outcome="error")]:
            evidence.consume(row)
        self.assertEqual(evidence.snapshot()["pending_encoder_retirements"], 1)
        self.assertFalse(evidence.complete())

    def test_identity_topology_registration_wake_and_process_changes_fail(self):
        for change in ({"stable_monitor_identity": "b" * 64}, {"physical_topology": "b" * 64},
                       {"monitor_registrations": 5}, {"display_profile_entries": 5},
                       {"macrdp_wake_assertions": 1}, {"process_generation": "b" * 64},
                       {"process_tree_count": 3}, {"process_alive": False}):
            with self.subTest(change=change):
                self.assertIn("fail", compare_snapshots(resources(), resources(**change), 1024).values())

    def test_log_tail_ignores_old_records_preserves_partial_and_flags_loss(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "logs"
            path.write_text(json.dumps(connected()[0]) + "\n")
            tail = LogTail(path)
            self.assertEqual(tail.poll(), [])
            payload = json.dumps(connected()[1])
            with path.open("a") as file:
                file.write(payload)
            self.assertEqual(tail.poll(), [])
            with path.open("a") as file:
                file.write("\nnot-json\n")
            self.assertEqual(len(tail.poll()), 1)
            self.assertEqual(tail.gaps, 1)
            replacement = Path(directory) / "new"
            replacement.write_text(json.dumps(connected()[1]) + "\n")
            replacement.replace(path)
            self.assertEqual(len(tail.poll()), 1)
            self.assertEqual(tail.gaps, 2)

    def test_valid_json_diagnostic_loss_is_a_gap(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "logs"
            path.touch()
            tail = LogTail(path)
            with path.open("a") as file:
                file.write(json.dumps({"fields": {"dropped_records": 4}, "message": "do-not-save"}) + "\n")
            self.assertEqual(len(tail.poll()), 1)
            self.assertEqual(tail.gaps, 1)


class PrivacyAndProcessTests(unittest.TestCase):
    def test_allowlist_drops_secret_text_and_invalid_types(self):
        raw = resources(password="never-save", peer="https://user:secret@example.test")
        raw.update(stable_monitor_identity="secret", process_rss_bytes=True,
                   colorsync_cpu_percent=float("nan"), unavailable=["password", "identity", {"secret": 1}])
        safe = sanitize_snapshot(raw)
        encoded = json.dumps(safe)
        self.assertNotIn("secret", encoded)
        self.assertNotIn("never-save", encoded)
        self.assertNotIn("process_rss_bytes", safe)
        self.assertEqual(safe["unavailable"], ["identity"])

    def test_private_exclusive_bounded_report(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.jsonl"
            report = PrivateReport(path, byte_limit=40)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            report.write({"kind": "test"})
            with self.assertRaises(ValueError):
                report.write({"overflow": "a" * 40})
            report.close()
            with self.assertRaises(FileExistsError):
                PrivateReport(path)
            target = Path(directory) / "link"
            target.symlink_to(path)
            with self.assertRaises(OSError):
                PrivateReport(target)
            record_cap = PrivateReport(Path(directory) / "record-cap")
            try:
                with self.assertRaises(ValueError):
                    record_cap.write({"oversized": "x" * 65536})
            finally:
                record_cap.close()

    def test_identity_observation_never_creates_or_changes_file(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            with self.assertRaises(FileNotFoundError):
                stable_identity(home, 3389)
            self.assertEqual(list(home.iterdir()), [])
            identity = home / ".macrdp/display-identities/rdp-port-3389.json"
            identity.parent.mkdir(parents=True)
            identity.write_text('{"schema":1,"serial":42}')
            original = identity.read_bytes()
            self.assertEqual(len(stable_identity(home, 3389)), 64)
            self.assertEqual(identity.read_bytes(), original)
            identity.write_text('{"schema":true,"serial":42}')
            with self.assertRaises(ProbeUnavailable):
                stable_identity(home, 3389)

    def test_missing_wake_observation_is_not_zero(self):
        with self.assertRaises(ProbeUnavailable):
            wake_count("")
        self.assertEqual(wake_count('Listed by owning process:\n pid 5 "macrdp remote session active"'), 1)
        self.assertEqual(wake_count('Listed by owning process:\n pid 6 "another app"'), 0)

    def make_driver(self, directory, body):
        path = Path(directory) / "driver"
        path.write_text(f"#!{sys.executable}\n" + body)
        path.chmod(0o700)
        return path

    def test_fake_executable_receives_only_action_and_cycle_and_output_is_discarded(self):
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory) / "calls"
            path = self.make_driver(directory, f"import sys\nwith open({str(calls)!r},'a') as f: f.write(repr(sys.argv[1:])+'\\n')\nprint('https://secret:password@example.test')\n")
            driver = CycleDriver(path)
            driver.run("connect", 1, 2)
            driver.run("disconnect", 1, 2)
            self.assertEqual(calls.read_text().splitlines(), ["['connect', '1']", "['disconnect', '1']"])
            path.chmod(0o722)
            with self.assertRaises(ValueError):
                CycleDriver(path)

    @unittest.skipUnless(hasattr(os, "fork"), "synthetic fork regression requires POSIX")
    def test_successful_connect_preserves_client_for_later_disconnect(self):
        with tempfile.TemporaryDirectory() as directory:
            child_file = Path(directory) / "child"
            body = f"""import os, signal, sys, time
if sys.argv[1] == 'disconnect':
    os.kill(int(open({str(child_file)!r}).read()), signal.SIGKILL)
else:
    pid = os.fork()
    if not pid:
        with open({str(child_file)!r}, 'w') as f: f.write(str(os.getpid()))
        time.sleep(60)
    else:
        while not os.path.exists({str(child_file)!r}): time.sleep(.01)
"""
            driver = CycleDriver(self.make_driver(directory, body))
            driver.run("connect", 1, 2)
            pid = int(child_file.read_text())
            try:
                os.kill(pid, 0)
                driver.run("disconnect", 1, 2)
                self.assert_not_running(pid)
            finally:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

    @unittest.skipUnless(os.name == "posix", "owned process groups require POSIX")
    def test_bounded_command_limit_and_hang(self):
        with self.assertRaises(ProbeUnavailable):
            bounded_command([sys.executable, "-c", "print('x'*1000)"], timeout=1, limit=10)
        start = time.monotonic()
        with self.assertRaises(ProbeUnavailable):
            bounded_command([sys.executable, "-c", "import time; time.sleep(60)"], timeout=.1)
        self.assertLess(time.monotonic() - start, 2)

    def assert_not_running(self, pid):
        until = time.monotonic() + 2
        while time.monotonic() < until:
            result = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "stat="],
                                    capture_output=True, timeout=1, check=False)
            status = result.stdout.decode().strip()
            if not status or status.startswith("Z"):
                return
            time.sleep(.02)
        self.fail("owned synthetic descendant survived abnormal action")

    @unittest.skipUnless(hasattr(os, "fork"), "synthetic fork regression requires POSIX")
    def test_exited_leader_descendant_holding_stdout_is_killed_and_unrelated_process_is_safe(self):
        with tempfile.TemporaryDirectory() as directory:
            child_file = Path(directory) / "child"
            script = f"""import os, signal, time
pid = os.fork()
if pid:
    os._exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with open({str(child_file)!r}, 'w') as f: f.write(str(os.getpid()))
time.sleep(60)
"""
            unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                         start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                with self.assertRaises(ProbeUnavailable):
                    bounded_command([sys.executable, "-c", script], timeout=1)
                self.assert_not_running(int(child_file.read_text()))
                self.assertIsNone(unrelated.poll())
            finally:
                unrelated.kill()
                unrelated.wait(timeout=1)

    @unittest.skipUnless(hasattr(os, "fork"), "synthetic fork regression requires POSIX")
    def test_abnormal_driver_stops_descendants_for_timeout_and_failure(self):
        for behavior in ("raise SystemExit(1)", "time.sleep(60)"):
            with self.subTest(behavior=behavior), tempfile.TemporaryDirectory() as directory:
                child_file = Path(directory) / "child"
                body = f"""import os, signal, time
pid = os.fork()
if not pid:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    with open({str(child_file)!r}, 'w') as f: f.write(str(os.getpid()))
    time.sleep(60)
else:
    while not os.path.exists({str(child_file)!r}): time.sleep(.01)
    {behavior}
"""
                driver = CycleDriver(self.make_driver(directory, body))
                with self.assertRaises(DriverError):
                    driver.run("connect", 1, 2)
                self.assert_not_running(int(child_file.read_text()))

    @unittest.skipUnless(os.name == "posix", "owned process groups require POSIX")
    def test_outer_probe_timeout_removes_inherited_inner_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            child_file = Path(directory) / "child"
            leaf = Path(directory) / "leaf.py"
            leaf.write_text(f"""import os, signal, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with open({str(child_file)!r}, 'w') as f: f.write(str(os.getpid()))
time.sleep(60)
""")
            outer = f"""import sys
sys.path.insert(0, {str(Path(__file__).parent)!r})
from session_soak_probes import bounded_command
bounded_command([sys.executable, {str(leaf)!r}], timeout=60, own_group=False)
"""
            with self.assertRaises(ProbeUnavailable):
                bounded_command([sys.executable, "-c", outer], timeout=1.5)
            self.assert_not_running(int(child_file.read_text()))


if __name__ == "__main__":
    unittest.main()
