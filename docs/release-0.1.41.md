# Greenmole RDP 0.1.41

This is a diagnostic release for the alternating 1920×1200 reconnect failure.
Version 0.1.40 successfully prepared a complete RGB graphics baseline before
AVC in both recorded fullscreen sessions, but the drawing failure persisted.
The user then tested Bitmap fallback and reported that every other connection
draws the desktop but leaves its windows frozen. The problem therefore does
not require AVC or RDPGFX pixel delivery.

The first bitmap reproduction delivered 449 captures and wrote 1,075 bitmap
regions (not full frames), totaling 31,353,741 bytes. Input processing received
1,378 protocol events and observed 1,365 native posting calls return, with no
posting errors. A temporary read-only observer then confirmed native cursor
movement inside the selected virtual display. In a longer connection it also
observed sustained host window movement during dragging. That observation
establishes host window movement in that attempt; it does not assign a visible
Windows result to every session or prove correct client presentation.

Code review found no definite alternating state in virtual-display ownership,
capture selection, bitmap encoder initialization or native input targets.
Existing counters cannot distinguish unchanged captured pixels from successful
capture callbacks, and posting-call returns do not acknowledge application
consumption. This release closes those evidence gaps without claiming a fix.

Input diagnostics now count keyboard callbacks, absolute/relative pointer
movement, button down/up callbacks and scrolling separately. Native posting
counts distinguish keyboard, mouse movement, button down/up, dragging,
scrolling and other events. Button identities, keys, text, coordinates and raw
event types are not retained. Callback counts include blocked or unavailable
input; native family counts include only posting calls that actually returned.
Protocol events and callbacks are different units: an absolute button packet
can produce a position callback followed by a button callback. These counters
saturate and follow the existing connection lifetime across resize activation.

Capture diagnostics record selected display/source/output geometry and the
first frame's content rectangle and scaling once per native stream. For the
first sixty seconds of a desktop activation, health observations occur at most
once per second, with a hard maximum of 61. Each samples at most 256 BGR points
and compares them with the previous eligible observation. Up to 256 color
triplets remain temporarily in private memory; colors, positions, images and
hashes never enter the log or export. The comparison ignores alpha and row
padding and detects color changes even at equal brightness.
A delayed first frame can still produce one baseline outside the original
window, without restarting periodic observations.

Unavailable samples and changed sources establish a new comparison baseline;
NV12 remains unsampled. A missing comparison is explicitly unavailable rather
than reported as zero changes. Source timestamp diagnostics report only
availability and forward/duplicate/regressed progress between sampled frames,
not timestamp values or end-to-end latency. Sparse unchanged samples cannot
prove that the whole desktop was frozen, and source changes cannot prove that
the client displayed the update correctly.

Diagnostic exports use schema 8, explicitly select the new aggregate fields,
and preserve the Bitmap encoder preference instead of labelling it unknown.
The recent-event limit is 400 so two one-minute traces can retain their startup
evidence; the one-MiB source-log and two-MiB export limits remain enforced.
Input selection lives in a separate module to keep the exporter maintainable.
No capture, encoder, input delivery or reconnect policy is changed.

The user deploys the DMG from this Mac mini's Desktop. The running app, RDP
service, driver and Core Audio are not installed or restarted during preparation.
No driver reinstall is needed. Keep Bitmap fallback selected and test consecutive
1920×1200 connections. On each, click and try dragging a window for fifteen
seconds even if its picture appears frozen, then disconnect. Report the order
of working and frozen connections. The new fields are retained in the normal
log and in **Logs → Export diagnostics**; there is no need to enable verbose
input logging.

All standard checks passed: 1,059 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. The 7
existing manual Rust tests remain ignored. Independent review covered bounds,
privacy, unavailable comparisons, source changes, posting categories, and
connection counter lifetimes. Dependency pins remain unchanged.

The ARM64 DMG was mounted read-only and verified for application/driver deep
strict signatures, version/revision, architecture, bundled licenses, packaged
diagnostic markers, unchanged microphone payload and checksum. The DMG and
checksum/build-info sidecars are on the Desktop. Active app hashes and app,
helper and Core Audio processes were preserved. Nothing was installed or
restarted. See the [package validation record](release-0.1.41-validation.json).
