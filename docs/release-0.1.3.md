# macrdp 0.1.3

Release candidate: 2026-10-12. The user installs the Desktop DMG and checks
Guacamole and Windows Remote Desktop Connection before accepting this release.

## Changes

- Keyboard cleanup tracks successful remote key presses and releases them on
  disconnect, reconnect and input synchronization. Failed releases remain
  retryable, and cleanup continues with the remaining keys. Unrelated local
  keys receive no blanket release events. Translation and lifecycle logic now
  live in smaller modules.
- Mouse handling supports horizontal scrolling, buttons 4/5 and relative input
  with display scaling and bounds. Combined movement/button packets retain all
  transitions. Standard wheel input uses signed 120-unit detents; advanced input
  uses its signed 16.16 representation. Fractional scrolling accumulates per axis.
- Bitmap sessions adapt capture frequency to measured compression and socket
  write time. Pending rectangles use newer captured pixels while accumulated
  damage remains scheduled, helping slow clients catch up without stale patches.
  Bitmap statistics now use compressed bytes and measured encoding costs.
- Cursor capture converts directly to RGBA and caches retained immutable cursor
  images and hotspots, removing the TIFF intermediate from production capture.
- Audio queues have 200 ms duration budgets per stage and reject packets older
  than 200 ms before delivery to the socket. Capture timestamp discontinuities
  reset partial packets and timing. RDPSND selects the client's matching format
  index even when PCM is not its first advertised format. Audio buffers use
  checked decoding instead of casts with implicit alignment requirements.
- The dashboard reports the actual encoder backend, audio queue/drop/recovery
  counters, and detailed startup/runtime errors. Logs offers a diagnostics export
  to Desktop with explicitly selected operational fields. Reports exclude raw log
  messages, free-form errors, credentials, paths, addresses, input text and
  clipboard contents, and are created as private files without overwriting
  existing reports.

Audio capture currently supports 48 kHz stereo. Enabled CLI configurations with
other sample rates or channel counts now fail validation instead of producing
incorrectly formatted audio. Queue budgets do not bound operating-system network
buffers or client playback latency. The displayed audio wait is the maximum of
the most recently measured capture/transport queue waits.

## Acceptance checks

After installing the Desktop DMG, exercise both clients:

- Hold a modifier or ordinary key, disconnect, then reconnect. Confirm no remote
  key remains stuck. Test Unicode typing and local keyboard use between sessions.
- Scroll vertically and horizontally with a mouse and trackpad. Test side buttons,
  dragging and pointer bounds, including a scaled or Retina display.
- Scroll large pages or play video over a constrained connection. Pause motion
  and check that the final image converges without old rectangles reappearing.
- Change cursor shapes and hotspots; check transparency and orientation.
- Play audio during motion, pause and resume playback, then reconnect. Watch
  queue, stale/drop and resynchronization counters when the connection is slow.
- Cause a recoverable startup failure, check the displayed error, correct it and
  restart. Export diagnostics from Logs and confirm the Desktop report is useful.

Use the [client benchmark guide](client-benchmarks.md) for comparable workloads.
Automated tests and isolated cursor timings do not establish end-to-end RDP speed.

## Automated validation

Run `tools/check.sh` on macOS for pinned-dependency workspace and desktop Rust
tests, CLI compilation, benchmark harness tests and the frontend production build.
The optional native cursor microbenchmark is:

```sh
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/tools/run-native-test.sh"
cargo test -p macrdp-capture --release --locked cursor_conversion_microbenchmark -- --ignored --nocapture
```

On the Apple Silicon Mac mini (macOS 26.6.2, Rust 1.99.0), the final checks passed:
319 workspace Rust tests, 25 desktop Rust tests, 8 Python tests, 3 JavaScript
tests, CLI compilation and the TypeScript/Vite production build. The bitmap
allocation and cursor conversion microbenchmarks are opt-in and excluded from
these test counts. Live Guacamole/mstsc acceptance remains for user testing.

The isolated cursor benchmark on the Apple Silicon Mac mini took 67,352 microseconds
for 1,000 legacy TIFF conversions and 2,020 microseconds for direct RGBA conversion
of the same 2-by-2 pixel fixture. This measures conversion overhead only; unchanged
cursor caching and live client behavior need separate observation. A native test
compares complete pixel buffers, including orientation and premultiplied alpha.
