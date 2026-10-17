# Release 0.1.21: keep fresh captures after a slow video encode

Performance review found a redundant frame skip following slow BGRA video
encoding. The capture mailbox already keeps only the newest image while the
single pending native operation completes. Discarding the next fresh capture
again could halve update frequency at a steady service time near the frame
interval, and could delay the final update until an idle recovery.

Remove that second discard while retaining one pending native input/pair,
accumulated damage, callback ownership, watchdogs, bounded queues and adaptive
capture-rate control. Completion cost remains diagnostic; it includes callback
wait time and does not measure CPU utilization. Native AVC420 capture remains
zero-copy and the AVC444 software-decoding compatibility policy is unchanged.

Validation: full `tools/check.sh` passed with 721 Rust tests, six existing
opt-in ignored tests, eight Python and eight JavaScript checks, CLI validation,
TypeScript and the production frontend build. The new actual-dispatch
regression fails against the original code and passes for both AVC420/AVC444,
with and without a mandatory stale-client/seed refresh. It simulates a 35 ms
completed encode at 30 FPS followed by exactly one new capture and verifies
that the newest pixel marker is delivered without another capture or idle event.
The focused display suite passed 96 tests with one existing ignored test.

The review retained the existing encoder conversions after measurement:
skipping redundant full-plane clearing saved only about 0.03–0.05 ms at
2560×1264, and a proposed fused conversion was slower (about 5.2 ms versus
1.8 ms in an isolated optimized synthetic test). These candidates were not
implemented. Existing native AVC420 steady-state capture remains zero-copy.

Guacamole private.16 separately reduces browser packet parsing and protocol
blob dispatch overhead. Its source is frozen at
`db83cdf0680d4aa8b4a7304cf03f27e7d3cb5dc6`; it is prepared independently from
the running private.15 gateway. Verified Desktop delivery is recorded after
the ad-hoc signed package finishes building.
No live Mac application, capture, display or input is changed during this work.
