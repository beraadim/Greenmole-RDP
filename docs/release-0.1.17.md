# Release 0.1.17: distinguish hardware frame skips from encoder failures

The 0.1.16 browser AVC420 experiment exposed noisy VideoToolbox callback logging.
The server labeled a successful callback with no output sample as an encoding
error. VideoToolbox explicitly permits dropped-frame callbacks with no sample;
see Apple's [compression callback contract](https://developer.apple.com/documentation/videotoolbox/vtcompressionoutputhandler?language=objc).

## Observed behavior

At the 2026-10-17 09:37 CEST snapshot, the examined 0.1.16 session had logged
6,375 repeated callback warnings since 09:34, roughly 35 per second. All had
status 0 and a null sample. Its INFO messages were disabled,
so the logs did not contain the native frame-drop flag or current codec state.
There were no ERROR records, watchdog recovery or crash markers in that run,
and no disconnect after the latest reconnect. Existing logs cannot prove the
reason for each empty callback. The warning volume alone is not a measure of
connection failure.

The running app was an earlier 0.1.16 test build. This release also includes the
final 0.1.16 full-range BT.709 conversion and color-metadata correction, along
with the safe NV12 capture-matrix lifetime fix. Guacamole private.13 remains
compatible and does not require redeployment for this encoder-only correction.

## Completion and recovery behavior

A native encode completion now distinguishes encoded output, an explicitly
reported frame drop and a failed or ambiguous completion. Normal flagged drops complete
and release their submitted input without logging a warning or forcing an
unnecessary new IDR. A dropped input that carried an explicit keyframe request
preserves that request. A newer request made while a previous input is pending
must also survive collection.

Real native failures, malformed encoded data and unexpected missing output
retain conservative decoder repair. Normal drop INFO and ambiguous empty WARN
reports are limited to one of each per session per five seconds. They include
native status, flags, cumulative counts and the number of suppressed reports.
Native errors and malformed output remain immediate warnings; parser panics
remain immediate errors. Consumers
still distinguish a finished empty result from an outstanding callback: a
normal drop must neither strand an input nor leave the display loop waiting.
Existing stale-image and idle repair remain responsible for a dropped final
visible change.

An AVC444 pair is published only when both views are usable. A discarded
encoded counterpart may already be a native reference, so incomplete pairs
request repair even if the missing view was normally dropped. Auxiliary
allocation and submission failures after the first encoded view also preserve
that repair request.

## Testing and delivery

All 47 encoder tests passed, including direct callback status/flag cases,
completion-watch notification, pending versus completed-empty collection,
cumulative diagnostic counts, keyframe retention and AVC444 pair edges. Real
VideoToolbox and OpenH264 encode/decode color checks retain the full-range
BT.709 contract. Independent review checked native input ownership, all
post-main error paths and immediate visibility of genuine failures.

`RUST_TEST_THREADS=1 CARGO_BUILD_JOBS=4 bash tools/check.sh` passed: 678 Rust
tests, eight Python tests and eight JavaScript tests, with six existing opt-in
tests ignored. CLI, TypeScript and production frontend builds passed. Package
verification and the Desktop delivery identity are recorded after build.

The running application and its remote display are not replaced or restarted
by preparation of this release. Installation and live-session testing remain
with the user. Browser AVC420 activation still requires a supported HTTPS
browser, the Display experiment enabled and a full disconnect/reconnect.

## Verified Desktop delivery

Built from source commit `03487866056d181eea3b53e6b95b95384ee59823` and delivered
to the Mac mini Desktop:

`/Users/beraadim/Desktop/macrdp_0.1.17_aarch64_03487866056d.dmg`

SHA-256:
`17df3948f88b30225454a20b39f4589b2cdb3a9fb26c394278c4b2233aa102b7`

The 15,368,133-byte image and checksum/build-info sidecars were copied with
verified destination hashes. Disk-image verification, a read-only mount,
embedded version/source/architecture inspection and the app's ad-hoc deep
strict signature checks passed. Three packaged helper argument/initialization
smoke checks rejected their invalid inputs before server or capture startup.
The owned mount was detached after inspection. Preparation did not install the
package or restart the user's application, server or remote display.
