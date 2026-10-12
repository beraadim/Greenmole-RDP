#!/usr/bin/env python3
"""Repeatable RDP workload recorder. Python 3.10+, standard library only.

Samples are server-side observations, never end-to-end input/display latency.
Serve the workload in a browser ON the Mac desktop being streamed over RDP.
"""
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import os
from pathlib import Path
import platform
import re
import secrets
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

SCHEMA = 1
MAX_BYTES = 64 * 1024 * 1024
MAX_LINE = 64 * 1024
PHASES = {"idle", "typing", "scroll", "video", "idle-after-motion", "resize", "reconnect", "slow-network"}
REQUIRED_METADATA = ("client", "client_version", "server_version", "server_build", "host", "display_width", "display_height", "scaling", "encoder", "chroma", "fps", "bitrate_kbps", "network", "scenario", "trial")
COMPARABLE_METADATA = tuple(k for k in REQUIRED_METADATA if k not in {"server_version", "server_build", "trial"})


def finite(value):
    return isinstance(value, (float, int)) and not isinstance(value, bool) and math.isfinite(value)


def percentiles(values):
    values = sorted(float(v) for v in values if finite(v) and v >= 0)
    if not values:
        return None
    # Nearest-rank quantiles, with explicit n because small samples are noisy.
    return {"n": len(values), "mean": sum(values) / len(values), **{
        f"p{p}": values[max(0, math.ceil(len(values) * p / 100) - 1)] for p in (50, 95, 99)}}


def validate_metadata(value):
    if not isinstance(value, dict):
        raise ValueError("metadata must be a JSON object")
    for key in REQUIRED_METADATA:
        item = value.get(key)
        if item is None or item == "" or isinstance(item, (dict, list, bool)):
            raise ValueError(f"metadata requires scalar field: {key}")
    for key in ("display_width", "display_height", "fps", "bitrate_kbps", "trial"):
        if not finite(value[key]) or value[key] <= 0:
            raise ValueError(f"metadata requires positive number: {key}")
    return value


class Recorder:
    """Exclusive, private, bounded output; the HTTP thread shares one writer."""
    def __init__(self, path):
        self.file = os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")
        self.lock = threading.Lock()
        self.size = 0
        self.full = False

    def write(self, record):
        data = (json.dumps(record, allow_nan=False, separators=(",", ":")) + "\n").encode()
        with self.lock:
            if len(data) > MAX_LINE or self.size + len(data) > MAX_BYTES:
                self.full = True
                raise ValueError("benchmark output limit reached")
            self.file.write(data)
            self.file.flush()
            self.size += len(data)

    def close(self):
        self.file.close()


def command(argv):
    return subprocess.check_output(argv, text=True, timeout=3, stderr=subprocess.DEVNULL).strip()


def parse_cpu_time(value):
    days, _, rest = value.rpartition("-")
    parts = [float(p) for p in rest.split(":")]
    if len(parts) not in (2, 3):
        raise ValueError("unexpected process CPU time")
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return seconds + (int(days) * 86400 if days else 0)


def process_sample(pid):
    if sys.platform == "win32":
        value = json.loads(command(["powershell", "-NoProfile", "-NonInteractive", "-Command",
            f"$p=Get-Process -Id {int(pid)} -ErrorAction Stop; @{{cpu=$p.CPU;rss=$p.WorkingSet64}} | ConvertTo-Json -Compress"]))
        return float(value["cpu"]), int(value["rss"])
    if sys.platform.startswith("linux"):
        text = Path(f"/proc/{pid}/stat").read_text()
        fields = text[text.rindex(")") + 2:].split()
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"), int(fields[21]) * os.sysconf("SC_PAGE_SIZE")
    # macOS ps cumulative CPU time, not its lifetime-averaged %cpu field.
    fields = command(["ps", "-p", str(pid), "-o", "time=", "-o", "rss="]).split()
    return parse_cpu_time(fields[0]), int(fields[1]) * 1024


def interface_sample(interface):
    if not re.fullmatch(r"[A-Za-z0-9_. -]{1,80}", interface):
        raise ValueError("invalid interface name")
    if sys.platform.startswith("linux"):
        base = Path("/sys/class/net") / interface / "statistics"
        return int((base / "rx_bytes").read_text()), int((base / "tx_bytes").read_text())
    if sys.platform == "win32":
        value = json.loads(command(["powershell", "-NoProfile", "-NonInteractive", "-Command",
            f"Get-NetAdapterStatistics -Name '{interface}' -ErrorAction Stop | Select-Object ReceivedBytes,SentBytes | ConvertTo-Json -Compress"]))
        return int(value["ReceivedBytes"]), int(value["SentBytes"])
    lines = command(["netstat", "-ibn", "-I", interface]).splitlines()
    columns = lines[0].split()
    for line in lines[1:]:
        values = line.split()
        if values and values[0] == interface and len(values) >= len(columns) and "<Link#" in line:
            return int(values[columns.index("Ibytes")]), int(values[columns.index("Obytes")])
    raise ValueError("interface byte counters unavailable")


class MetricsTail:
    def __init__(self, path):
        self.path = Path(path)
        self.offset = self.path.stat().st_size if self.path.exists() else 0
        self.skipped = 0

    def poll(self):
        if not self.path.exists():
            return []
        if self.path.stat().st_size < self.offset:
            self.offset = 0
        rows = []
        with self.path.open("rb") as file:
            file.seek(self.offset)
            # Bound work per collection tick; retain partial final writes.
            for _ in range(4096):
                start = file.tell()
                raw = file.readline(MAX_LINE + 1)
                if not raw:
                    break
                if len(raw) > MAX_LINE:
                    self.skipped += 1
                    self.offset = file.seek(0, 2)
                    break
                if not raw.endswith(b"\n"):
                    self.offset = start
                    break
                self.offset = file.tell()
                try:
                    value = json.loads(raw)
                    if isinstance(value, dict) and value.get("type") == "metrics" and isinstance(value.get("metrics"), dict) and finite(value.get("timestamp_ms")):
                        # Reject non-finite extension fields before writing JSONL.
                        json.dumps(value, allow_nan=False)
                        rows.append(value)
                    else:
                        self.skipped += 1
                except (ValueError, UnicodeError):
                    self.skipped += 1
        return rows


def make_handler(recorder, token, port):
    directory = Path(__file__).resolve().parent

    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(3)

        def log_message(self, *_args):
            pass

        def reply(self, status, content=b"", mime="text/plain"):
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(content)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'")
            self.end_headers()
            self.wfile.write(content)

        def do_GET(self):
            if self.path == "/":
                page = (directory / "client_benchmark.html").read_text(encoding="utf-8").replace("BENCHMARK_TOKEN", token)
                self.reply(200, page.encode(), "text/html; charset=utf-8")
            elif self.path == "/workload.js":
                self.reply(200, (directory / "client_benchmark.js").read_bytes(), "text/javascript")
            else:
                self.reply(404)

        def do_POST(self):
            if self.path != "/event" or self.headers.get("Origin") != f"http://127.0.0.1:{port}" or not hmac.compare_digest(self.headers.get("X-Benchmark-Token", ""), token):
                self.reply(403)
                return
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 16384:
                    raise ValueError("event size")
                event = json.loads(self.rfile.read(size))
                if not isinstance(event, dict) or event.get("event") not in {"start", "phase", "render", "resize", "visibility", "input", "end"}:
                    raise ValueError("event kind")
                if event.get("phase") not in PHASES or not finite(event.get("timestamp_ms")):
                    raise ValueError("phase or timestamp")
                # No typed content is recorded. Keep only known numeric fields.
                allowed = ("elapsed_ms", "width", "height", "device_pixel_ratio", "frames", "p50_ms", "p95_ms", "p99_ms", "input_count", "visible", "seed")
                clean = {k: v for k, v in event.items() if k in allowed and finite(v) and v >= 0}
                recorder.write({"type": "workload", "event": event["event"], "phase": event["phase"],
                    "timestamp_ms": event["timestamp_ms"], "received_timestamp_ms": time.time() * 1000, **clean})
                self.reply(204)
            except (ValueError, TypeError, UnicodeError, OSError):
                self.reply(400)

    return Handler


def run(args):
    metadata = validate_metadata(json.loads(Path(args.metadata).read_text(encoding="utf-8")))
    if args.pid <= 0 or not 0.1 <= args.interval <= 60 or not 1 <= args.duration <= 86400 or not 1024 <= args.port <= 65535:
        raise ValueError("invalid PID, interval, duration or port")
    token = secrets.token_urlsafe(24)
    recorder = Recorder(args.output)
    http = None
    tail = None
    try:
        workload_hash = hashlib.sha256(Path(__file__).with_suffix(".js").read_bytes()).hexdigest()
        recorder.write({"type": "metadata", "schema": SCHEMA, "timestamp_ms": time.time() * 1000, "metadata": metadata,
            "collector_platform": platform.platform(), "sample_interval_s": args.interval,
            "workload_sha256": workload_hash, "interface": args.interface, "pid": args.pid})
        http = HTTPServer(("127.0.0.1", args.port), make_handler(recorder, token, args.port))
        http.timeout = 0.5
        # Per-connection timeout prevents a half-written local POST from blocking shutdown.
        http.socket.settimeout(3)
        thread = threading.Thread(target=http.serve_forever, daemon=True)
        thread.start()
        tail = MetricsTail(args.metrics) if args.metrics else None
        print(f"Open http://127.0.0.1:{args.port}/ in the browser on the streamed Mac desktop.", flush=True)
        print("Recording; Ctrl+C ends the run. Resize/reconnect/network phases require operator actions.", flush=True)
        started = time.monotonic()
        previous_cpu = previous_net = None
        previous_time = started
        while time.monotonic() - started < args.duration and not recorder.full:
            tick = time.monotonic()
            row = {"type": "system", "timestamp_ms": time.time() * 1000}
            try:
                cpu, rss = process_sample(args.pid)
                row["rss_bytes"] = rss
                if previous_cpu is not None and cpu >= previous_cpu and tick > previous_time:
                    row["cpu_percent_one_core"] = (cpu - previous_cpu) * 100 / (tick - previous_time)
                previous_cpu = cpu
            except (OSError, ValueError, TypeError, IndexError, KeyError, subprocess.SubprocessError):
                row["process_unavailable"] = True
                previous_cpu = None
            if args.interface:
                try:
                    counters = interface_sample(args.interface)
                    if previous_net is not None and tick > previous_time and all(a >= b for a, b in zip(counters, previous_net)):
                        row["interface_rx_bytes_per_s"], row["interface_tx_bytes_per_s"] = [(a - b) / (tick - previous_time) for a, b in zip(counters, previous_net)]
                    previous_net = counters
                except (OSError, ValueError, IndexError, KeyError, subprocess.SubprocessError):
                    row["interface_unavailable"] = True
                    previous_net = None
            recorder.write(row)
            if tail:
                for record in tail.poll():
                    recorder.write(record)
            previous_time = tick
            time.sleep(max(0, args.interval - (time.monotonic() - tick)))
    except KeyboardInterrupt:
        pass
    finally:
        if http:
            http.shutdown()
            http.server_close()
        try:
            if tail and not recorder.full:
                recorder.write({"type": "collector_end", "timestamp_ms": time.time() * 1000, "malformed_metrics_records": tail.skipped})
        finally:
            recorder.close()
    return 0


def load_records(path):
    records, skipped = [], 0
    if Path(path).stat().st_size > MAX_BYTES:
        raise ValueError("input exceeds 64 MiB limit")
    with Path(path).open("rb") as file:
        for raw in file:
            if len(raw) > MAX_LINE:
                skipped += 1
                continue
            try:
                row = json.loads(raw)
                if not isinstance(row, dict) or not finite(row.get("timestamp_ms")):
                    raise ValueError("invalid record")
                records.append(row)
            except (ValueError, UnicodeError):
                skipped += 1
    return records, skipped


def summarize(path):
    records, skipped = load_records(path)
    meta = next((r for r in records if r.get("type") == "metadata"), {})
    events = sorted((r for r in records if r.get("type") == "workload" and r.get("event") in {"phase", "end"}), key=lambda r: r["timestamp_ms"])
    observed_phases = {r.get("phase") for r in events if r.get("event") == "phase"}
    complete = PHASES.issubset(observed_phases) and any(r.get("event") == "end" for r in events)
    warnings = []
    if not complete:
        warnings.append("workload incomplete; don't treat this as a full-suite regression comparison")
    if any(r.get("type") == "workload" and r.get("event") == "visibility" and r.get("visible") == 0 for r in records):
        warnings.append("host workload browser was hidden; background throttling can affect results")
    if not any(r.get("type") == "metrics" for r in records):
        warnings.append("no server metrics samples; server timing/bandwidth unavailable")
    buckets = {"all": []}
    for row in records:
        buckets["all"].append(row)
        phase = None
        for event in events:
            if event["timestamp_ms"] > row["timestamp_ms"]:
                break
            phase = event.get("phase") if event["event"] == "phase" else None
        if phase:
            buckets.setdefault(phase, []).append(row)
    def summary(rows):
        values = {}
        def add(key, value):
            if finite(value) and value >= 0:
                values.setdefault(key, []).append(value)
        for row in rows:
            if row.get("type") == "system":
                for key in ("cpu_percent_one_core", "rss_bytes", "interface_rx_bytes_per_s", "interface_tx_bytes_per_s"):
                    add(key, row.get(key))
            elif row.get("type") == "metrics" and isinstance(row.get("metrics"), dict):
                m = row["metrics"]
                add("server_wire_kbps", m.get("bitrate_kbps"))
                add("delivered_updates_per_s", m.get("fps"))
                # Timing gauges describe the last update. Exclude idle samples
                # which would otherwise count the same old frame repeatedly.
                if finite(m.get("fps")) and m["fps"] > 0:
                    for key in ("encode_ms", "capture_queue_ms", "completion_queue_ms", "write_ms", "pending_acks"):
                        add("sampled_" + key, m.get(key))
                    if not m.get("ack_suspended") and finite(m.get("latency_ms")) and m["latency_ms"] > 0:
                        add("sampled_decode_ack_ewma_ms", m["latency_ms"])
            elif row.get("type") == "workload" and row.get("event") == "render":
                add("source_browser_cadence_p95_ms", row.get("p95_ms"))
        return {key: percentiles(value) for key, value in values.items()}
    return {"schema": SCHEMA, "metadata": meta.get("metadata", {}), "workload_sha256": meta.get("workload_sha256"), "sample_interval_s": meta.get("sample_interval_s"),
        "records": len(records), "malformed_records": skipped, "workload_complete": complete, "warnings": warnings,
        "observed_build_versions": sorted({str(r["build_version"]) for r in records if r.get("type") == "metrics" and "build_version" in r}),
        "workload_events": {name: sum(r.get("type") == "workload" and r.get("event") == name for r in records) for name in ("resize", "input", "visibility")},
        "limitations": "1Hz last-update gauge percentiles, not per-frame or end-to-end latency. Interface bytes include other traffic. CPU 100% = one core.",
        "phases": {phase: summary(rows) for phase, rows in buckets.items()}}


def compare(baseline, candidate):
    mismatches = [key for key in COMPARABLE_METADATA if baseline["metadata"].get(key) != candidate["metadata"].get(key)]
    for key in ("workload_sha256", "sample_interval_s"):
        if not baseline.get(key) or baseline.get(key) != candidate.get(key):
            mismatches.append(key)
    missing = [key for key in REQUIRED_METADATA if key not in baseline["metadata"] or key not in candidate["metadata"]]
    changes = {}
    for phase, metrics in baseline["phases"].items():
        for name, before in metrics.items():
            after = candidate["phases"].get(phase, {}).get(name)
            if after:
                changes[f"{phase}.{name}"] = {"baseline_n": before["n"], "candidate_n": after["n"],
                    **{q: {"before": before[q], "after": after[q], "percent_change": (after[q] / before[q] - 1) * 100 if before[q] else None} for q in ("mean", "p50", "p95", "p99")}}
    incomplete = not baseline.get("workload_complete") or not candidate.get("workload_complete")
    return {"comparable": not mismatches and not missing and not incomplete, "incomplete_workload": incomplete,
        "metadata_mismatches": mismatches, "missing_metadata": missing,
        "warnings": {"baseline": baseline.get("warnings", []), "candidate": candidate.get("warnings", [])}, "changes": changes}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    record = commands.add_parser("run", help="serve workload and record local process/server metrics")
    record.add_argument("--pid", type=int, required=True)
    record.add_argument("--metadata", required=True)
    record.add_argument("--output", required=True)
    record.add_argument("--metrics", help="CLI --metrics-jsonl path; new samples only")
    record.add_argument("--interface", help="optional interface counters (all host traffic)")
    record.add_argument("--port", type=int, default=8765)
    record.add_argument("--interval", type=float, default=1)
    record.add_argument("--duration", type=float, default=600)
    report = commands.add_parser("summarize")
    report.add_argument("input")
    diff = commands.add_parser("compare")
    diff.add_argument("baseline")
    diff.add_argument("candidate")
    diff.add_argument("--allow-mismatch", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "run":
            return run(args)
        if args.command == "summarize":
            print(json.dumps(summarize(args.input), indent=2, allow_nan=False))
            return 0
        result = compare(summarize(args.baseline), summarize(args.candidate))
        print(json.dumps(result, indent=2, allow_nan=False))
        return 0 if result["comparable"] or args.allow_mismatch else 2
    except (OSError, ValueError) as error:
        print(f"benchmark: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
