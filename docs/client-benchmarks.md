# Repeatable Guacamole and Windows RDP benchmarks

Use `tools/client_benchmark.py` on the Mac running the server. It serves a fixed,
170-second browser workload on loopback, records timestamped process CPU/RSS,
optional interface byte counters, and the CLI's metrics JSONL. Run the same
workload through Guacamole and Windows Remote Desktop separately.

The harness measures **server observations and source-browser rendering cadence**.
It does not measure input-to-display latency, client presentation FPS, or browser
video decoding. Those require client instrumentation or an external camera. Its
typing animation exercises damage and short pauses; use the manual text box to
check actual RDP keyboard input. Text entered there is never recorded.

## Record a run

Requires Python 3.10+; no Python packages or admin privileges are required. The
workload browser must support `AbortSignal.timeout` (current Safari/Chrome/Edge).
Start an instrumented CLI server using your normal secure configuration and a
new metrics filename; don't run the GUI server on the same port simultaneously:

```sh
./target/release/macrdp-server --config config.toml --metrics-jsonl /tmp/macrdp-metrics-trial1.jsonl
```

Use the actual server process PID (`pgrep -fl macrdp-server` to identify it). Do
not put passwords, usernames, tokens or connection URLs in benchmark metadata.
Create `trial1-metadata.json` with the exact settings used:

```json
{
  "client": "Guacamole",
  "client_version": "record Guacamole + guacd + browser versions",
  "server_version": "record installed version",
  "server_build": "record git commit",
  "host": "record Mac model, chip, RAM, macOS version",
  "display_width": 1920,
  "display_height": 1080,
  "scaling": "100%, host browser zoom 100%, devicePixelRatio 1",
  "encoder": "hardware",
  "chroma": "avc420",
  "fps": 60,
  "bitrate_kbps": 20000,
  "network": "wired LAN, no shaper; record measured link conditions",
  "scenario": "full-suite, initial viewport 1920x1080, resize to 1280x720 then restore",
  "trial": 1
}
```

In another Mac terminal, with the PID substituted:

```sh
python3 tools/client_benchmark.py run --pid 12345 \
  --metadata trial1-metadata.json --metrics /tmp/macrdp-metrics-trial1.jsonl \
  --interface en0 --output trial1.jsonl
```

Connect the RDP client, then open `http://127.0.0.1:8765/` **in a browser on the
streamed Mac desktop**. Keep that browser foreground and click Start. For
Guacamole this is the Mac's browser inside the remote session, separate from the
browser displaying Guacamole. The collector is intentionally unavailable over
the network. Choose the correct network interface or omit `--interface`.

The collector stops after ten minutes by default (`--duration` overrides this),
or press Ctrl+C after the page says Complete. Each output file is create-new and
capped at 64 MiB. Metrics collection starts at the current end of the metrics
file; start the collector before the workload. Use a new metrics file per run.
CLI metrics are optional, but their absence is reported by omitted metrics,
never fabricated zero values. Leave credentials in your normal server config.

## Workload and manual checks

| Phase | Duration | Rendering/action |
| --- | ---: | --- |
| idle | 8 s | Fixed scene; don't move the pointer or type. |
| typing | 24 s | Four-second 20-character/s bursts, two-second pauses. Optional actual RDP typing in the text box. |
| scroll | 20 s | Fixed document at 180 CSS pixels/s. |
| video | 30 s | Deterministic synthetic 30 fps moving scene. |
| idle-after-motion | 8 s | Final scene stays unchanged. |
| resize | 20 s | Manually resize the **RDP client** at phase +5 s, restore at +12 s. Record requested/actual sizes. |
| reconnect | 30 s | Manually disconnect at +5 s and reconnect at +15 s, leaving the Mac browser open. |
| slow-network | 30 s | Same motion over a separately configured constrained link. |

Run a baseline suite on LAN, and another suite with the same explicit bandwidth,
latency and loss limits applied to the **RDP connection path**. A browser's HTTP
throttle does not necessarily affect the RDP/guacd transport. Configure and
remove network shaping using the test network's existing tooling; the harness
does not change host firewall or network settings. Put exact shaping conditions
in `network` and repeat the identical setup for both server builds.

Record observations alongside each run: full image after idle/reconnect, no
black/stale rectangles, readable text, keyboard layout/characters, pointer
alignment, correct resize, and any disconnect/error. The phase timestamps and
browser viewport events support analysis; they do **not** prove successful RDP
resize/reconnection. Verify those in the client/server logs. Do not switch away
from the host workload browser during the suite: visibility changes are recorded
and background throttling invalidates a rendering comparison. Desktop browser
chrome affects the viewport, so use identical window dimensions and zoom.

## Compare

```sh
python3 tools/client_benchmark.py summarize trial1.jsonl > trial1-summary.json
python3 tools/client_benchmark.py compare baseline.jsonl candidate.jsonl > comparison.json
```

Run at least three trials per client/build/network combination, after one warmup,
and alternate baseline/candidate order. Compare like-for-like metadata. A compare
command exits with status 2 for an incomplete suite or mismatched client/config/workload/sample interval;
`--allow-mismatch` permits exploratory comparisons and preserves the warnings.
Server build/version and trial number may differ. Missing fields stay absent.

Reports include mean/p50/p95/p99 **of samples**, with the sample count, per phase:

- CPU: cumulative process CPU delta, where 100% equals one busy CPU core; RSS is
  resident bytes. This excludes guacd, the client and host-browser CPU. Sample
  those separately if attributing whole-system costs.
- Wire kbps and delivered updates/s come from server metrics. Bitmap regions can
  be separate updates; this is not client display FPS.
- Encoding, capture/completion queue and transport-write values are the last
  update sampled at approximately 1 Hz, excluding inactive samples. Their sampled
  percentiles are **not per-frame latency percentiles**. ACK latency is an EWMA
  including client decoding and transport, not network ping or input latency;
  suspended/unobserved ACK values are excluded.
- Interface throughput includes all traffic on the interface, including unrelated
  apps; use server wire throughput to attribute RDP bytes. Browser cadence is
  the source browser's rAF scheduling, not the remote client's frame presentation.

For GUI builds, run the same workload and manual checklist; the collector can
sample the GUI PID without `--metrics`. Use the UI metrics and exported logs for
diagnosis. CLI results do not automatically establish GUI lifecycle correctness.

## Automated regressions and allocation microbenchmark

```sh
python3 -m unittest discover -s tools -p 'test_client_benchmark.py' -v
node --test tools/client_benchmark_workload_test.js
cargo test -p macrdp-core idle_policy
cargo test -p ironrdp-server allocation_tests
cargo test -p ironrdp-server --release bitmap_allocation_benchmark -- --ignored --nocapture
```

The microbenchmark compares the prior output-sizing strategy with packed-visible-
pixel sizing on solid, noisy and padded dirty regions. It prints elapsed times
and output capacities; it makes no speed threshold assertion. Wire-byte equality
and small/odd-width/padded/multi-chunk cases are normal regression tests. Execute
timing comparisons on the same otherwise-idle machine, using release builds.
