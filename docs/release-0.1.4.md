# macrdp 0.1.4

Release candidate: 2026-10-12. The user installs the Desktop DMG and checks
Guacamole and Windows Remote Desktop Connection before accepting this release.

## Changes

- Capture recovery preserves the current session's audio sender. Recovery probes
  run one at a time with bounded retry delays, while fallback video and controls
  remain available. Recovered capture establishes a complete graphics baseline.
  A process-wide capture lease also covers disconnect and service restart. A
  pending recovery call runs independently of service shutdown, retains its
  native resources through cleanup, and defers display restoration until safe.
  Connections retry after retirement instead of overlapping native capture.
- Clipboard negotiation resolves registered HTML and file-list formats by name
  and requests the client's actual ID. File copying publishes all received files,
  preserves Unicode and escaped filenames, and keeps advertised descriptors
  aligned with their source files.
- Translated keyboard input tracks remote Caps Lock, including synchronization,
  reconnect and repeated key-downs. Letter case uses Shift XOR Caps Lock;
  punctuation, AltGr and dead-key selection retain their normal Shift behavior.
- Accepted TCP connections explicitly enable TCP_NODELAY before TLS framing.
  The opt-in small-write benchmark compares the setting on loopback; live RDP
  performance still needs comparable Guacamole/mstsc workloads.
- Initial handshakes have a 30-second overall budget as well as existing
  15-second stage deadlines. Failed pre-authentication attempts use bounded,
  expiring source tracking and short cooldowns. Successful authentication clears
  the source history, and rejected retries do not prolong its cooldown. Control
  commands remain responsive during global admission pauses. This remains a
  single-session server; these limits do not imply concurrent authentication.
- The dashboard reports p50/p95/p99/worst stage timings from up to 1,024 actual
  capture, encode, completion or write events per stage. Stages have independent
  counts; bitmap write events may be individual regions. Missing measurements
  remain absent, and polling/exporting no longer changes rate sampling intervals.
- Diagnostics schema 2 includes up to 120 sanitized metric snapshots and 20
  session outcomes with structured reasons and final totals/timings. This history
  remains in memory across service stop/restart and disappears when the app exits.
  Inactive terminal polling does not overwrite the evidence preceding a failure.
  Private exports retain the existing content and identifier exclusions.

## Acceptance checks

- With audio playing, exercise display suspension and recovery, then verify video,
  audio, cursor and input without reconnecting. Check stop/restart during recovery.
- Copy rich text and several files in both directions. Include filenames with
  Norwegian letters, spaces, `#` and `%`, and verify contents and file count.
- Test Caps Lock, Shift+Caps Lock, key repeat, Norwegian letters, AltGr, dead keys
  and reconnect with Caps Lock enabled. Check punctuation and local keyboard use.
- Compare typing and small updates using the [benchmark guide](client-benchmarks.md).
  Socket timings and stage percentiles are not end-to-end input latency.
- Retry a wrong password, then the correct one. Stop the service during a stalled
  handshake and confirm the UI remains responsive. Test both client types.
- Export diagnostics after a disconnect or service stop. Confirm recent timing
  history and the final session reason remain present, without personal content.

## Automated validation

Run `tools/check.sh` on the Mac. The optional transport experiment is:

```sh
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/tools/run-native-test.sh"
cargo test -p ironrdp-server --lib --locked nodelay_small_update_microbenchmark -- --ignored --nocapture
```

The loopback transport experiment completed 200 measured fragmented 64-byte
updates per setting on the Apple Silicon Mac mini. TCP_NODELAY off measured
p50/p95 of 0.064/0.104 ms; on measured 0.067/0.111 ms. This small loopback run
showed no latency improvement and does not measure real RDP responsiveness.

Native validation passed on the Apple Silicon Mac mini:

- 357 workspace Rust tests and 29 desktop backend tests; three optional
  microbenchmarks remain excluded from the normal test run.
- CLI build, eight Python benchmark tests, three JavaScript workload tests,
  and the TypeScript/Vite production build.
- The optional transport experiment above passed separately.

Regression coverage includes bounded ownership through pending native cleanup,
shutdown without joining the recovery worker, deferred restoration, clipboard
format-ID collisions and descriptor bounds, multi-file paste using isolated
pasteboards, remote Caps Lock, handshake budgets/throttling, terminal display
errors, and diagnostic timing retention/privacy. Live client acceptance remains
the installation test described above.
