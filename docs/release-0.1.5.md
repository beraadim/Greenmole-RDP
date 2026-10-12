# macrdp 0.1.5

Release candidate: 2026-10-12. Install the Desktop DMG and check Guacamole and
Windows Remote Desktop Connection before accepting this release.

## Changes

- Clipboard file transfers use a bounded window of eight requests instead of
  enqueueing the entire file. This removes the queue overflow that a 16 MiB
  paste could trigger. Incoming data is staged in private temporary files, with
  size/count limits, per-request deadlines and an overall transfer deadline.
  Cancelled or timed-out transfers discard their unpublished staging files.
- Clipboard locks retain file handles and descriptor indices by `clipDataId`.
  Changing the clipboard no longer silently switches the source of an existing
  locked paste. Unknown or unlocked IDs fail safely; changed source files are
  rejected. Up to 16 lock IDs share at most 128 retained source handles;
  completed incoming files release their handles while the batch finishes.
- Native capture start, stop, format replacement and frame-rate changes run on
  a dedicated owner thread. Service controls and peer closure remain observable
  while capture starts. Cancellation retains ownership until native cleanup
  finishes; a new session cannot overlap an unfinished capture owner.
- Graphics distinguish verified unchanged pixels from missing damage hints.
  Unchanged captures can skip encoding when no seed or refresh is required.
  Bitmap queues retain region descriptions, one batch source and one latest
  deferred frame, copying native pixels only when a region is dispatched.
  Raw frames use checked shared slices.
  Accumulated damage and full-frame seeds retain their correctness safeguards.
- Desktop Settings now exposes system audio, clipboard synchronization, file
  transfers and maximum file size. Defaults remain enabled with a 100 MiB file
  limit. Changes are saved and take effect after restarting the service. The
  supported per-file range is 1–256 MiB; incoming batches are limited to 256 MiB.

## Acceptance checks

- Copy a file larger than 16 MiB in each direction and compare its contents.
  Repeat with multiple files, empty files and Unicode filenames. Cancel or
  disconnect during a transfer, then reconnect and paste again.
- Start a large paste from the Mac, then copy a different file before the first
  paste finishes. The original locked transfer must retain its original file.
- Resize, change frame rate and switch graphics modes while connected. Stop the
  service during capture initialization or display recovery, then restart and
  reconnect after native cleanup. Verify video, pointer coordinates and audio.
- Leave the desktop idle, then type, scroll and move windows in both clients.
  Check that no stale region remains after a change is quickly reversed.
- Toggle audio and clipboard options independently, restart the service, and
  verify that the settings take effect. Test a file above the configured limit.

## Automated validation

`tools/check.sh` passed on the Apple Silicon Mac mini:

- 390 workspace Rust tests and 31 desktop backend tests. Four optional
  microbenchmarks are excluded from the normal test run.
- CLI build, eight Python benchmark tests, three JavaScript workload tests,
  and the TypeScript/Vite production build.
- The optional `bitmap_materialization_work_microbenchmark` also passed.
  Its fixed 100-batch, 12-region workload copied 28,262,400 bytes with the
  previous eager-and-refresh strategy and 14,745,600 bytes with the new
  dispatch-time native-pixel copy routine, a 47.8% reduction. Raw regions shared
  14,745,600 logical pixel bytes and copied none during materialization.

Tests use simulated capture stalls and isolated clipboard fixtures, leaving the
installed application and the user's clipboard untouched. Copy counters describe
bitmap materialization work; they are not an end-to-end latency measurement.
