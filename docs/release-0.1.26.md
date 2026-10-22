# Release 0.1.26: optional retained virtual display

Add **Settings > Display > Reuse virtual display between sessions**. It is
experimental, off by default, requires the virtual display backend, and takes
effect after stopping and starting the service. CLI configuration supports
`reuse_virtual_display = true` and `--reuse-virtual-display`.

The first connection creates the virtual display. Disconnect still stops input,
capture and encoding, restores the physical display arrangement and saved
window positions, and releases the session's wake assertions. With reuse enabled,
the display is then disabled while its owning helper and identity lease remain
alive. Reconnecting enables the same owned display and prepares its requested
mode before starting capture. Starting an idle server creates no display.
Stop, final controller destruction and crash recovery still retire the owner;
the option does not leave an active fourth desktop while disconnected.

The extra enable/disable operation uses a private WindowServer API checked at
runtime. Parking failures fall back to full display retirement. Windows created
during a remote session receive a bounded recovery pass before disabling, rather
than relying only on the snapshots of windows present at connection time.
The physical display backend and default remove-on-disconnect behavior remain
unchanged. Turn reuse off and restart the service to compare the two behaviors.

This is an experiment to reduce virtual-display creation and destruction after
consecutive remote sessions. The latest investigation again found increased
WindowServer color-restoration waits and ColorSync requests, despite unchanged
monitor-profile data and no remaining connected client or wake assertion.
The user reported smooth local-only use after reboot and degradation after RDP
sessions. These observations support the experiment; they do not prove an OS
bug or establish the cause of cursor lag. Enabling/disabling and changing the
primary monitor still exercise macOS display topology changes. This release
does not reset existing WindowServer state or delete saved monitor profiles.

## Testing the package

Quit the existing application before replacing it so all helper protocols match.
Install the new DMG, select the virtual backend, enable the reuse option, and
restart the service. If the old cursor slowdown remains from the preceding run,
use a fresh login or reboot before comparing consecutive sessions.

Connect and disconnect several times, including a client resize. Check that
windows from all physical displays restore to their original locations, and that
a window opened remotely returns to a physical screen. While disconnected, the
remote desktop must be unavailable, capture must stop and physical screens must
be able to sleep normally. Confirm cursor responsiveness remains stable. Test a
physical-display hotplug and sleep/wake cycle before relying on reuse daily.

Finally stop the service and check that no owned virtual display/helper remains.
Repeat with reuse disabled as a control. Guarded private-session users should
also verify local-input blocking and the existing lock-restoration policy in
both modes. The package is delivered for user installation; the running
application is not replaced automatically.

Native acceptance can also be run explicitly with
`cargo test -p macrdp-display --test native_smoke -- --ignored --retained` in a
logged-in test session. This changes display topology and gathers/restores
windows. Ordinary release tests do not opt into those host actions.

## Validation and delivery

Full serial checks passed: 818 Rust tests, with six existing opt-in tests
ignored, eight Python tests and eleven JavaScript tests. TypeScript and the
production frontend build passed. The display library passes 89 tests, including
new retention ordering, strict protocol, missing-symbol, saved-layout and
remaining-window recovery cases. The opt-in native smoke executable compiles;
normal invocation confirms it skips live display changes.

Independent reviews covered native identity and ownership validation, parked
capture rejection, fallback error propagation, window recovery and supervision.
Changed Rust files pass scoped formatting and the staged source passes a secret
scan. Existing unrelated compiler and frontend bundle warnings remain.

No live virtual display, capture stream, remote connection, input tap, screen
cover, host lock or wake assertion was created by these checks. A read-only
native probe confirmed private all-slot enumeration works on the build Mac;
actual disable/re-enable behavior and prevention of cursor regression remain
device tests. The ad-hoc signed package identity and Desktop delivery are
recorded separately against the committed source after the build.
