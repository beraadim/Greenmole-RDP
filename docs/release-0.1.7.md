# macrdp 0.1.7

Release candidate: 2026-10-12. Install the Desktop DMG and check Guacamole and
Windows Remote Desktop Connection before accepting this release.

## Changes

- Relative mouse input on the Fast-Path preserves movement and every simultaneous
  button transition. Additional-input channel mouse events run through the same
  ordered connection dispatcher as keyboard input. A bounded staging queue drains
  before the next input packet or resize barrier, and clears on disconnect.
- Hardware encoder inputs and Core Foundation objects have scoped owners, so
  native allocation, configuration and encode failures release their references.
  Native session invalidation runs on a reserved cleanup worker while retaining
  callback state and any pending input buffer. Two reservations bound the number
  of active or blocked cleanup owners; unavailable capacity permits software
  fallback. Idle hardware refreshes use asynchronous submission and the existing
  completion watchdog instead of synchronously flushing the codec.
- Native encoded output validates contiguous buffer lengths or copies segmented
  buffers through Core Media. Checked AVCC parsing supports one-, two- and
  four-byte NAL lengths, rejects truncated data and bounds allocations. Malformed
  output and Rust unwinding become a completed encode failure rather than an
  out-of-bounds read or a panic crossing the native callback boundary.
- Encoder bitrate changes report native failures and update confirmed bitrate
  only after the API accepts the target. OpenH264 receives the required layer and
  bitrate structure; changes before its first frame configure the unused encoder
  instead of tuning an uninitialized native session. Hardware initialization checks required properties and
  preparation; unsupported optional tuning properties produce diagnostic logs.
  Requested and confirmed encoder targets and failure counts appear in the
  dashboard and sanitized diagnostics.
- Rich clipboard copies request HTML and advertised Unicode text sequentially,
  then publish both representations together in one pasteboard item. Invalid or
  rejected HTML falls back to text. A newer local or remote copy cancels retained
  rich content; the existing timeout policy protects untagged response ordering.
- Text and HTML payloads have an 8 MiB budget, including Unicode terminators or
  HTML envelope overhead. Native lengths are checked before Rust string and wire
  allocations. Oversized local copies receive an ordinary format error and leave
  the clipboard session usable. CF_HTML parsing reads only the bounded header,
  supports CR, LF and CRLF, and validates context and fragment byte boundaries.
- Adaptive streaming considers fresh client graphics decode-backlog trends.
  Sustained backlog reduces bitrate and frame rate within existing bounds;
  isolated bursts, repeated snapshots, unavailable/suspended ACKs and stale
  reports do not manufacture congestion. Recovery waits for elevated queues to
  drain. Structured logs distinguish client backlog from socket/encoder pressure.
- Settings display Auto, the 50 Mbps default and saved custom bitrate, FPS and
  resolution values. Numeric network settings validate and save on blur or Enter,
  avoiding partial edits being persisted with every keystroke.
- Metrics include process-wide sample identifiers and millisecond timestamps.
  Events and polling share one snapshot identity; late or duplicate snapshots
  cannot rewind charts or keep stale telemetry marked healthy. Charts record
  unchanged and zero values, retain a 60-second window and use actual sample times.
  Displayed averages include every recorded sample.

## Acceptance checks

- In both clients, drag while changing keyboard modifiers and click while moving.
  Verify no lost movement, button releases or modifier-click behavior. Resize and
  reconnect, then repeat the sequence.
- Copy rich text from a browser or document into both a rich editor and a
  plain-text field. Copy a newer value while synchronization is pending and
  verify only that value survives. Check normal copying after an oversized copy.
- Test hardware AVC420 and AVC444, including a desktop that becomes idle just
  after a change. Inspect requested and applied encoder rates when changing
  bitrate; failures must remain visible rather than report false success.
- Use a client or connection under graphics load. Check that sustained decode
  backlog reduces streaming rates and that rates recover after the queue drains.
- Confirm Auto and 50 Mbps are visible on a fresh configuration. With steady
  throughput and an idle desktop, verify charts retain repeated values and zeros.
  Stop/restart the service and verify old charts and stale data clear correctly.

## Automated validation

`tools/check.sh` passed on the Apple Silicon Mac mini, followed by 28 encoder
tests after the final Core Foundation dictionary ownership repair. In total,
458 workspace Rust tests, 36 desktop backend tests, eight Python benchmark tests
and eight JavaScript workload/history tests passed (510 unique tests). Four
optional microbenchmarks remain excluded from the normal suite. The CLI build
and TypeScript/Vite production build passed.
Native tests use isolated pasteboards and simulated stalls. User installation and
end-to-end Guacamole/mstsc acceptance remain manual.
