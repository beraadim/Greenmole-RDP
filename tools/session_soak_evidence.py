"""Allowlisted, in-memory lifecycle evidence; never retain raw log records."""
from __future__ import annotations

import json
import os
from pathlib import Path
import stat

MAX_LINE = 64 * 1024
MAX_POLL_BYTES = 1024 * 1024


class LogTail:
    """Only new complete records, with rotation/loss explicitly reported."""
    def __init__(self, path):
        self.path = Path(path)
        self.identity, self.offset = None, 0
        self.gaps = 0
        self._open_at_end()

    def _open(self):
        fd = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
            os.close(fd)
            raise ValueError("log_unavailable")
        return os.fdopen(fd, "rb"), info

    def _open_at_end(self):
        with self._open()[0] as file:
            info = os.fstat(file.fileno())
            self.identity, self.offset = (info.st_dev, info.st_ino), info.st_size

    def poll(self):
        rows = []
        try:
            file, info = self._open()
        except (OSError, ValueError):
            self.gaps += 1
            return rows
        with file:
            identity = (info.st_dev, info.st_ino)
            if identity != self.identity or info.st_size < self.offset:
                self.gaps += 1
                self.identity, self.offset = identity, 0
            file.seek(self.offset)
            total = 0
            for _ in range(4096):
                start = file.tell()
                raw = file.readline(MAX_LINE + 1)
                if not raw:
                    break
                total += len(raw)
                if len(raw) > MAX_LINE:
                    self.gaps += 1
                    self.offset = file.seek(0, 2)
                    break
                if not raw.endswith(b"\n"):
                    self.offset = start
                    break
                self.offset = file.tell()
                try:
                    value = json.loads(raw)
                    if isinstance(value, dict):
                        fields = value.get("fields")
                        if isinstance(fields, dict) and integer(fields.get("dropped_records")):
                            self.gaps += 1
                        rows.append(value)
                    else:
                        self.gaps += 1
                except (ValueError, UnicodeError, RecursionError):
                    self.gaps += 1
                if total >= MAX_POLL_BYTES:
                    break
        return rows


def integer(value):
    return value if type(value) is int and 0 < value < 2**64 else None


class CycleEvidence:
    """Operational flags and bounded token sets for one serial test cycle."""
    def __init__(self, encoder="hardware"):
        self.connected = False
        self.disconnected = False
        self.capture_retired = False
        self.display_restored = False
        self.wake_released = False
        self.guard_released = False
        self.encoder_expected = encoder == "hardware"
        self.encoder_retire_seen = False
        self.retiring = set()
        self.active_operations = set()
        self.errors = set()
        self.admissions = {}
        self.preconnection_encoders = {}
        self.worker = None

    def consume(self, record):
        if record.get("target") != "macrdp_transition":
            return
        fields = record.get("fields")
        if not isinstance(fields, dict):
            return
        component, phase, state = (fields.get(key) for key in ("component", "phase", "state"))
        worker = integer(fields.get("worker_pid"))
        if not worker:
            return
        # Admission is before the authenticated connection boundary. Retain only
        # a bounded per-worker boolean, never raw fields, identities or messages.
        if not self.connected and component == "private_guard" and phase == "protection":
            if state in ("not_required", "ready") and len(self.admissions) < 32:
                self.admissions[worker] = state == "not_required"
            return
        if not self.connected and component == "video_encoder":
            token = integer(fields.get("retirement_id"))
            if token and phase == "vt_create" and state == "begin":
                if len(self.preconnection_encoders) >= 32:
                    self.errors.add("lifecycle_evidence_limit")
                else:
                    owners = self.preconnection_encoders.setdefault(worker, set())
                    if len(owners) < 32:
                        owners.add(token)
                    else:
                        self.errors.add("lifecycle_evidence_limit")
            return
        if component == "session" and phase == "connection":
            if state == "connected":
                if self.connected:
                    self.errors.add("overlapping_session")
                    return
                self.connected, self.worker = True, worker
                self.guard_released = self.admissions.get(worker, False)
                self.retiring.update(self.preconnection_encoders.get(worker, set()))
                self.admissions.clear()
                self.preconnection_encoders.clear()
            elif state == "disconnected" and self.connected and worker == self.worker:
                self.disconnected = True
            return
        # A different worker, stale cleanup before this connect, an unmatched
        # completion, or a plain-text message must never satisfy cleanup proof.
        if not self.connected or worker != self.worker:
            return
        if component == "native_capture":
            operation = integer(fields.get("operation_id"))
            owner = integer(fields.get("owner_id"))
            if not operation or not owner or not isinstance(phase, str) or len(phase) > 64:
                return
            key = (owner, operation, phase)
            if state == "begin":
                if len(self.active_operations) >= 256:
                    self.errors.add("lifecycle_evidence_limit")
                else:
                    self.active_operations.add(key)
                    if phase == "restore_display":
                        self.capture_retired = True
            elif state == "end" and key in self.active_operations:
                self.active_operations.remove(key)
                # Diagnostic destructors legitimately return "returned" and a
                # retried stop can report "recovered". Neither alone proves
                # restoration, but they are not failed cleanup results.
                if fields.get("outcome") in ("error", "panicked"):
                    self.errors.add("native_cleanup_error")
                elif phase == "restore_display" and fields.get("outcome") == "ok":
                    self.display_restored = True
        elif component == "video_encoder":
            token = integer(fields.get("retirement_id"))
            if token and (phase == "vt_create" and state == "begin"
                          or phase == "retirement" and state in ("queued", "begin")):
                if len(self.retiring) >= 32:
                    self.errors.add("lifecycle_evidence_limit")
                else:
                    self.retiring.add(token)
            elif token and phase == "retirement" and state == "end" and token in self.retiring:
                self.retiring.remove(token)
                self.encoder_retire_seen = True
                if fields.get("outcome") != "ok":
                    self.errors.add("encoder_cleanup_error")
        elif component == "wake_lease" and phase == "cleanup":
            if state in ("complete", "pending"):
                self.wake_released = state == "complete"
        elif component == "private_guard" and phase == "protection":
            if state in ("not_required", "complete"):
                self.guard_released = True
            elif state in ("ready", "pending"):
                self.guard_released = False

    def snapshot(self):
        return {"connection_observed": self.connected, "disconnect_observed": self.disconnected,
                "capture_retirement_observed": self.capture_retired,
                "display_restoration_observed": self.display_restored,
                "wake_release_observed": self.wake_released,
                "guard_release_or_not_required_observed": self.guard_released,
                "encoder_retirement_observed": self.encoder_retire_seen if self.encoder_expected else None,
                "encoder_cleanup_required": self.encoder_expected,
                "pending_capture_operations": len(self.active_operations),
                "pending_encoder_retirements": len(self.retiring),
                "errors": sorted(self.errors)}

    def complete(self):
        return (self.connected and self.disconnected and self.capture_retired and self.display_restored
                and self.wake_released and self.guard_released and not self.active_operations
                and not self.retiring and (not self.encoder_expected or self.encoder_retire_seen)
                and not self.errors)


def compare_snapshots(before, after, rss_limit_bytes):
    """Evaluate against the original idle baseline, never a moving baseline."""
    checks = {}
    for key in ("process_generation", "monitor_registrations", "monitor_registration_fingerprint",
                "display_profile_entries", "physical_display_count", "physical_topology",
                "virtual_display_online_count", "stable_monitor_identity", "process_tree_count"):
        initial, final = before.get(key), after.get(key)
        checks[key] = "unknown" if initial is None or final is None else "pass" if initial == final else "fail"
    checks["process_alive"] = "fail" if before.get("process_alive") is False or after.get("process_alive") is False else "unknown" if before.get("process_alive") is None or after.get("process_alive") is None else "pass"
    checks["wake_assertions"] = "unknown" if before.get("macrdp_wake_assertions") is None or after.get("macrdp_wake_assertions") is None else "pass" if before["macrdp_wake_assertions"] == after["macrdp_wake_assertions"] == 0 else "fail"
    for key in ("process_rss_bytes", "process_tree_rss_bytes"):
        initial, final = before.get(key), after.get(key)
        checks[key] = "unknown" if initial is None or final is None else "pass" if final - initial <= rss_limit_bytes else "fail"
    return checks
