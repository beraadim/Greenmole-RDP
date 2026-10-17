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
`db83cdf0680d4aa8b4a7304cf03f27e7d3cb5dc6`; it was built and tested in isolation before the user authorized deployment.
The verified Desktop delivery and matching gateway release are recorded below.
No live Mac application, capture, display or input is changed during this work.

Delivered test package: `/Users/beraadim/Desktop/macrdp_0.1.21_aarch64_ef58df08a45c.dmg`

- Source revision: `ef58df08a45c3539d2dc9aec5e0e7f4286ae5f6e`.
- SHA-256: `30072ff163d3f0cd1bd92b7e4dd5d8004590852a8c0dc6effee63791dce3d5db`.
- Size: 15,431,567 bytes; aarch64, ad-hoc signed.
- Read-only DMG checksum, deep strict application signature, packaged version,
  source revision and metadata sidecar match. Packaged helper argument and
  invalid-configuration rejection checks passed without leaking test secrets.
- The DMG, checksum and build metadata were copied atomically to the Desktop and
  hashed again after copying. The running app and its display remain unchanged.

The matching Guacamole private.16 frozen implementation passed 286 native tests
in each of three configurations, 259 Chrome tests, 40 frontend tests, pinned
matched-image builds, read-only preflight and all four exact packaged-bundle
AVC420/AVC444 × VideoToolbox/OpenH264 playback/fallback/image-reconnect cases.
These isolated checks do not constitute acceptance testing on the user's older
client. Guacamole deployment is recorded independently in its repository.

After explicit user approval, Guacamole private.16 was deployed on Oberon with
configuration and runtime settings preserved. The served browser bundle matches
the exact bundle used by all four integration cases; both runtime containers
have zero restarts and no critical startup entries, and the private.15 rollback
images are retained. The user still installs the Mac test DMG independently.
