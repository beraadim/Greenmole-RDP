# Release 0.1.20: retain AVC444 through missing and delayed native output

Fixes a server-side AVC444 recovery error observed with Guacamole: an incomplete
main/auxiliary pair previously retried as AVC420 on the same native encoder.
That advanced a shared reference chain with output the AVC444 browser could not
consume and could permanently switch the browser to image fallback. Ordinary
native dropped frames also produced a warning on almost every attempt.

AVC444 now remains a complete ordered pair. Pending native work retains its
input, auxiliary picture and callback ownership until completion; a short wait
is no longer treated as a failed frame or a reason to flush/reuse its output
slot. Callback completion wakes the display without requiring mouse movement.
The display keeps the original submission's visible size and five-second
budget through both views, coalesces newer capture data, and delivers the newest
baseline after the retained pair completes. Persistent failures recover through
owned image fallback rather than silently mixing AVC420 into the AVC444 stream.

Native drops and failures are distinguished, and repeated unavailable-frame
warnings are bounded. A fresh main IDR followed by a valid auxiliary P-picture
is an independently decodable AVC444 pair; recovery does not demand a redundant
auxiliary IDR. Native cadence now accounts for both views, with timestamps that
preserve the configured desktop rate. AVC444 uses the regular bitrate controller
instead of the low-latency controller that could repeatedly drop half a pair,
force another large IDR, and sustain rate-control debt. Hardware encoding,
real-time mode, no frame reordering and the target bitrate remain in use.
The server still preserves bitmap selection and live quality controls, including
while a native pair is pending.

The dashboard now names its value RDP graphics mode and directs Guacamole users
to the browser's Display menu for browser playback status. The Mac-to-gateway
codec can remain AVC444 while the gateway provides images to the browser.
Guacamole private.15 reports a bounded, content-free fallback cause so a browser
rejection is distinguishable from native/server recovery.

Validation: `tools/check.sh` passed with 720 Rust tests, six existing opt-in
ignored tests, eight Python checks, eight JavaScript checks, CLI validation,
TypeScript and the production frontend build. The display suite includes
regressions for delayed callbacks, cancellation, ordered pair recovery, live
settings and a shared recovery deadline across slow drops. A bounded synthetic
2560×1264 hardware probe produced 12 complete pairs with no drops or errors;
the original policy produced 40 empty results in 50 attempts. This is a
reproduction check, not a sustained live-session performance measurement.

Guacamole private.15 separately repairs desktop-size browser decoding and
includes real-browser tests at 2560×1264. The test DMG remains ad-hoc signed.
No live macrdp installation, display or user input is changed while preparing
this test DMG; the user installs and tests the new application.

Delivered test package: `/Users/beraadim/Desktop/macrdp_0.1.20_aarch64_3d7f2a46978f.dmg`

- Source revision: `3d7f2a46978f7ce9839740351a92852ecd6bda4a`.
- SHA-256: `db34feab33befdd7c23edd19e97f85a2e0ae445a74d88f9ba756e851ef9466b8`.
- Size: 15,437,578 bytes; aarch64, ad-hoc signed.
- The read-only DMG checksum, application signature, packaged version/revision
  and metadata sidecar match. Packaged helper argument and invalid-configuration
  rejection checks passed without exposing the synthetic password.
- The DMG, checksum and build metadata were copied atomically to the Desktop
  and verified again after copying. The running application remains unchanged.

The matching Guacamole private.15 implementation is frozen at
`78e80a4996d075b53b9a0791bfba1ce97ca29b8c`. Its release checks passed 285
native tests in each of three configurations, 247 Chrome tests and 40 frontend
tests; Firefox passed 244 tests with three capability-dependent skips. See the
Guacamole repository's private.15 release record for its deployed image and
packaged-browser verification.
