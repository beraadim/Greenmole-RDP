# Release 0.1.25: stable monitor identity and complete wake cleanup

Reuse a persisted virtual-monitor serial across sessions for the same macOS user
and RDP listening port. Previously, each helper PID produced another serial,
causing macOS to remember successive remote monitors. Investigation on the test
Mac found 165 macrdp ICC profiles and matching remembered display UUIDs, plus
WindowServer workers waiting on ColorSync color-preference restoration. That
observation supports reducing monitor identity churn; it does not prove the
saved profile count caused the physical cursor/animation slowdown.

The identity record lives in private per-user storage and is validated before
use. A nonblocking exclusive file lease rejects simultaneous ownership rather
than allocating another serial. Helpers and their one-shot configuration
selectors retain the same lease until process exit. Selectors validate a fresh
session generation separately from the stable monitor serial, including before
native configuration commits. This prevents an orphan selector from targeting a
new session after macOS reuses a display ID. Selector native deadlines exit
without running potentially blocking process shutdown handlers.

The physical backend does not acquire an identity slot. Virtual displays are
still created only for active remote sessions and removed on disconnect.
Browser resizing retains the connection's monitor. Same-user servers sharing a
listening port share its identity slot even when bound to different IP addresses;
concurrent ownership fails with a diagnostic. Existing ICC profiles and display
preferences are never deleted as automatic cleanup.

Disconnect now releases both remote wake assertions. The handler previously
released display-sleep prevention while retaining timed remote user activity
until handler destruction or the OS idle timeout. Shared idempotent cleanup also
runs on Drop, clears the activity throttle, accepts already-expired assertion
IDs, and retains unexpected release failures for retry with a warning. Correct
the native release function's IOReturn ABI. Tests use fake assertion IDs and
release functions rather than waking displays or changing host power state.

This release includes the 0.1.24 guarded private-session option, which remains
off by default. See [its setup and limits](private-sessions.md).

## Validation and device testing

Full serial native release checks passed: 799 Rust tests, with six existing
opt-in tests ignored. The display library passes 75 tests, including 14 identity
filesystem/subprocess tests, five selector tests and a strict protocol test.
Four new wake-lease tests cover consecutive sessions, idempotence, expired IDs
and partial release failures. Eight Python tests, eleven JavaScript tests,
TypeScript and the production frontend build also pass. The initial serial
subprocess fixture exposed a harness-prefix handshake hang; bounded readiness,
exit and kill/reap cleanup fixed it before the complete successful rerun.
Changed Rust modules pass formatting checks; existing formatting differences
in untouched workspace files and existing compiler/bundle-size warnings remain.
The ad-hoc signed aarch64 DMG was built, verified and delivered to the Mac mini
Desktop. See the package identity and validation record below.
No live virtual display, remote session, screen cover, input tap, host lock or
power assertion is created by the new focused tests. Identity tests use private
temporary directories and harmless subprocess fixtures.

Quit the existing application before replacing it so parent and helper protocol
versions match. After installing the new DMG, use the same listening port and connect/disconnect
several times, including one browser resize. Confirm windows gather and restore
correctly, every remote screen disappears on disconnect, and local pointer and
window animation responsiveness remains stable. Compare the monitor UUID and
macrdp ICC-profile count after the first new session with later sessions; the
first use of the new identity may create one new profile. macOS controls private
virtual-display UUID allocation, so actual profile reuse needs device testing.
The previous WindowServer/ColorSync backlog may require a fresh login or restart
before judging whether the new release prevents its return; it is not cleared
by changing the application package.

After disconnect, inspect power assertions and allow the configured display
idle timeout without local input. The wake cleanup removes macrdp's own holds;
hardware input or another application can still keep displays awake. Keep the
running application in place until the user chooses to install and test.

## Verified Desktop delivery

Test package: `macrdp_0.1.25_aarch64_3f622228016f.dmg` on the Mac mini user's Desktop.

- Source revision: `3f622228016f67fdb249cbd3b8109a19867806cb`.
- SHA-256: `33d71d26ba6caef8713a9cff1876beea82d09bf1f8153703cd35137e0d3c5748`.
- Size: 15,630,792 bytes; aarch64, ad-hoc signed.
- Image checksum, read-only mounted application deep strict signature, release
  version and embedded build metadata passed. The verification mount was removed.
- Packaged display/server/private/recovery helper extra-argument rejection and
  standalone CLI version/private-helper rejection passed. No valid helper,
  recovery, remote connection, power assertion or security action was invoked.
- DMG, checksum and build metadata were copied atomically to the Desktop and
  hashed after copying. Previous 0.1.24 installer and sidecars were removed after
  verification. The running 0.1.22 application was left in place for user testing.

[The validation record](release-0.1.25-validation.json) records test results,
package verification and remaining device acceptance checks. Test after a fresh
login or Mac restart if the old WindowServer backlog is still present; this
package is a prevention/cleanup change and does not reset system display state.
