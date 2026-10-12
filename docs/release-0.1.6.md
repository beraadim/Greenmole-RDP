# macrdp 0.1.6

Release candidate: 2026-10-12. Install the Desktop DMG and check Guacamole and
Windows Remote Desktop Connection before accepting this release.

## Changes

- Cursor hide/default updates now emit the required Fast-Path packet even though
  they have no payload. This fixes lost cursor-mode transitions.
- Clipboard publication checks both the copy generation and the native
  pasteboard change count immediately before writing. Delayed text, HTML, image,
  and file responses cannot intentionally replace a newer local or remote copy.
  macOS provides no atomic compare-and-swap across unrelated pasteboard writers.
- Clipboard callbacks use an ordered worker for file I/O and image conversion.
  Admission is limited to 32 queued jobs and 32 MiB including the active payload.
  At most two storage workers may remain alive across reconnects, bounding the
  resources retained by blocked native file operations. Teardown does not join a
  blocked worker. Discarded unpublished file batches are removed by the worker.
  Each native operation drains Cocoa temporaries. A publication panic closes the
  clipboard session safely even if it poisons shared state.
- An independent clipboard watchdog ends the client session after a format
  request or storage operation stalls for 30 seconds. Queue exhaustion also
  fails the session safely. Reconnect to resume clipboard synchronization; the
  listening service stays available. CLIPRDR format responses have no request
  IDs, so issuing another request after timeout could misinterpret a late reply.
- Native capture FPS changes retry transient failures up to four times with
  100/200/400 ms backoff. New targets supersede old requests and stop interrupts
  the backoff. The dashboard and diagnostics expose requested and applied FPS,
  failure counts and exhausted retries. The configured FPS is distinct from the
  adaptive capture target and measured delivered updates per second.
- Output preparation releases server state before waiting on the socket, keeping
  incoming input and acknowledgements responsive during outbound congestion.
  Static channels retain logical-message ordering while sharing the socket at
  complete packet boundaries, normally in batches of up to 16 KiB. An individual
  larger protocol packet remains indivisible. Audio is admitted once for its
  entire wave, and cancelling an unfinished write still invalidates the stream.
  Reactivation drains admitted logical messages before deactivating the display.
  Already received input and deferred channel events replay once after resize;
  retained input is limited to 128 PDUs and 1 MiB, and event budgets stay charged.
- Pending restart notices are derived from backend state and survive navigation.
  Reverting a saved value clears its notice. Diagnostic schema 3 separates saved
  preferences from running service preferences, including accepted live updates;
  observed capture rates are reported separately in metrics. The ineffective
  connection-limit control is replaced by the actual single-session behavior.
  Legacy preferences containing that unused field still load.

## Acceptance checks

- Toggle captured cursor mode and reconnect in both clients. Verify that hiding
  and restoring the client pointer does not leave duplicate or stale pointers.
- Copy text and images in both directions, including a newer local copy while
  remote data is pending. Only the latest copy should be published. Repeat with
  multiple files and files larger than 16 MiB; compare the received contents.
- Use a slow connection while typing, playing audio, copying files and resizing.
  Verify that input stays responsive and graphics, audio and files remain valid.
  Interrupt a transfer, reconnect, and repeat it.
- Change FPS while connected, then inspect requested/applied rates. Verify that
  a successful change eventually converges and that any exhausted retry is
  visible. Reconnect after an exhausted native configuration failure.
- Change audio or clipboard settings while running, navigate away and back, and
  export diagnostics. Pending changes must stay visible until reverted or the
  service restarts. Also test Main → Virtual → resolution edit → Main; the
  deferred resolution must remain visible as pending.

## Automated validation

`tools/check.sh` passed on the Apple Silicon Mac mini, followed by the final
clipboard regression run after adding poisoned-state recovery:

- 419 workspace Rust tests and 35 desktop backend tests passed. Four optional
  microbenchmarks remain excluded from the normal run.
- CLI build, eight Python benchmark tests, three JavaScript workload tests,
  and the TypeScript/Vite production build passed.
- Regression coverage includes empty cursor packets, throttled output ordering,
  keyboard input during stalled clipboard output, resize drain/replay across two
  activations, partial-write cancellation, stale and missing clipboard responses,
  blocked worker shutdown, poisoned publication recovery, capture retries, and
  pending-settings accuracy and diagnostic redaction.

Tests use isolated pasteboards and simulated stalls, preserving the installed
application and the user's clipboard. Transport fairness tests verify ordering
and progress under throttled output; they are not an end-to-end latency benchmark.
