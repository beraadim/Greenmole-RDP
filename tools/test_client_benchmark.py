import http.client
import json
from pathlib import Path
import tempfile
import threading
import unittest
from http.server import HTTPServer
from unittest.mock import patch

import client_benchmark as bench


def metadata():
    return dict(client="Guacamole", client_version="1.6", server_version="0.1.1", server_build="baseline",
                host="test-mac", display_width=1920, display_height=1080, scaling="100%", encoder="hardware",
                chroma="avc420", fps=60, bitrate_kbps=20000, network="LAN", scenario="full-suite", trial=1)


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "run.jsonl"

    def write_rows(self, rows):
        self.path.write_text("\n".join(json.dumps(r) for r in rows) + "\n", encoding="utf-8")

    def test_quantiles_ignore_missing_and_nonfinite_values(self):
        self.assertIsNone(bench.percentiles([None, float("nan"), float("inf"), -1, True]))
        result = bench.percentiles(range(1, 101))
        self.assertEqual((result["p50"], result["p95"], result["p99"]), (50, 95, 99))
        self.assertEqual(bench.percentiles([7])["p99"], 7)

    def test_metadata_rejects_missing_and_invalid_configuration(self):
        bench.validate_metadata(metadata())
        for value in ({}, {**metadata(), "fps": 0}, {**metadata(), "fps": float("nan")}, {**metadata(), "display_width": "1920"}):
            with self.assertRaises(ValueError):
                bench.validate_metadata(value)

    def test_cpu_time_parses_macos_long_running_processes(self):
        self.assertEqual(bench.parse_cpu_time("02:03.45"), 123.45)
        self.assertEqual(bench.parse_cpu_time("1-02:03:04"), 93784)

    def test_tail_reads_complete_new_records_and_skips_malformed_data(self):
        old = {"type": "metrics", "timestamp_ms": 1, "metrics": {"fps": 1}}
        self.write_rows([old])
        tail = bench.MetricsTail(self.path)
        row = json.dumps({**old, "timestamp_ms": 2}).encode()
        with self.path.open("ab") as file:
            file.write(b"bad json\n" + row[:20])
        self.assertEqual(tail.poll(), [])
        with self.path.open("ab") as file:
            file.write(row[20:] + b"\n")
        self.assertEqual(tail.poll()[0]["timestamp_ms"], 2)
        self.assertEqual(tail.poll(), [])
        self.assertEqual(tail.skipped, 1)
        self.path.write_text(json.dumps(old) + "\n")
        self.assertEqual(tail.poll(), [old])

    def test_summary_excludes_stale_idle_timings_and_suspended_ack(self):
        self.write_rows([
            {"type": "metadata", "timestamp_ms": 1, "metadata": metadata(), "workload_sha256": "same", "sample_interval_s": 1},
            {"type": "workload", "timestamp_ms": 100, "event": "phase", "phase": "typing"},
            {"type": "metrics", "timestamp_ms": 110, "metrics": {"fps": 1, "encode_ms": 4, "latency_ms": 20, "bitrate_kbps": 100}},
            {"type": "metrics", "timestamp_ms": 120, "metrics": {"fps": 0, "encode_ms": 900, "bitrate_kbps": 0}},
            {"type": "metrics", "timestamp_ms": 130, "metrics": {"fps": 1, "encode_ms": float("nan"), "latency_ms": 900, "ack_suspended": True}},
            {"type": "system", "timestamp_ms": 140, "rss_bytes": 4096, "process_unavailable": True},
            {"type": "workload", "timestamp_ms": 200, "event": "end", "phase": "typing"},
            {"type": "metrics", "timestamp_ms": 210, "metrics": {"fps": 1, "encode_ms": 8}},
        ])
        with self.path.open("a") as file:
            file.write("not JSON\n[]\n")
        report = bench.summarize(self.path)
        phase = report["phases"]["typing"]
        self.assertEqual(phase["sampled_encode_ms"]["n"], 1)
        self.assertEqual(phase["sampled_encode_ms"]["p95"], 4)
        self.assertEqual(phase["sampled_decode_ack_ewma_ms"]["p95"], 20)
        self.assertNotIn("cpu_percent_one_core", phase)
        self.assertEqual(report["malformed_records"], 2)
        self.assertEqual(report["phases"]["all"]["sampled_encode_ms"]["n"], 2)

    def test_comparison_requires_matching_workload_and_client_configuration(self):
        report = {"metadata": metadata(), "workload_sha256": "same", "sample_interval_s": 1, "workload_complete": True,
                  "phases": {"idle": {"server_wire_kbps": bench.percentiles([0, 0])}}}
        candidate = {**report, "metadata": {**metadata(), "server_version": "0.1.2", "server_build": "candidate", "trial": 2}}
        result = bench.compare(report, candidate)
        self.assertTrue(result["comparable"])
        self.assertIsNone(result["changes"]["idle.server_wire_kbps"]["p95"]["percent_change"])
        candidate["metadata"]["client"] = "Windows Remote Desktop"
        self.assertFalse(bench.compare(report, candidate)["comparable"])
        self.assertFalse(bench.compare(report, {**report, "workload_complete": False})["comparable"])

    def test_recorder_is_exclusive_and_bounded(self):
        recorder = bench.Recorder(self.path)
        try:
            with self.assertRaises(FileExistsError):
                bench.Recorder(self.path)
            with patch.object(bench, "MAX_BYTES", 8):
                with self.assertRaises(ValueError):
                    recorder.write({"too": "large"})
            self.assertTrue(recorder.full)
            self.assertEqual(self.path.stat().st_size, 0)
        finally:
            recorder.close()

    def test_workload_endpoint_requires_same_origin_token_and_discards_text(self):
        recorder = bench.Recorder(self.path)
        server = HTTPServer(("127.0.0.1", 0), bench.make_handler(recorder, "secret", 0))
        port = server.server_address[1]
        server.RequestHandlerClass = bench.make_handler(recorder, "secret", port)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            row = json.dumps({"event": "input", "phase": "typing", "timestamp_ms": 123,
                              "input_count": 2, "typed_text": "never store this"})
            connection.request("POST", "/event", row, {"Content-Type": "application/json"})
            response = connection.getresponse()
            self.assertEqual(response.status, 403)
            response.read()
            connection.request("POST", "/event", row, {"Origin": f"http://127.0.0.1:{port}", "X-Benchmark-Token": "secret"})
            response = connection.getresponse()
            self.assertEqual(response.status, 204)
            response.read()
            connection.close()
            saved = json.loads(self.path.read_text())
            self.assertEqual(saved["input_count"], 2)
            self.assertNotIn("typed_text", saved)
        finally:
            server.shutdown()
            server.server_close()
            recorder.close()


if __name__ == "__main__":
    unittest.main()
