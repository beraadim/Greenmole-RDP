# Release 0.1.22: gather secondary-display windows and restore their placement

Virtual-display sessions now keep a connection-scoped snapshot of ordinary,
movable windows before promoting the remote display to primary. Automatically
gather windows from the originally non-main physical displays. Leave the original
main display's windows to macOS's primary-display transition, while retaining
their original display, position and size for restoration too.

On disconnect, restore the physical display arrangement and each captured window's
original monitor and frame. If an original monitor is no longer connected, fit
its windows visibly onto a surviving physical display. Retain Accessibility
window identities in memory instead of matching titles, and do not log titles or
window contents. Snapshot, automatic gather and restoration each have a bounded
three-second budget. Closed, minimized, full-screen, unsupported or unresponsive
windows can be skipped, and applications can constrain window placement or size.

Keep the same snapshot through RDP resize reactivation; a browser resize must not
replace the pre-connection frames with remote positions. A later connection takes
a fresh snapshot. Windows opened after connection remain under macOS placement
control on display removal. The manual **Move windows to remote display** button
remains available and does not overwrite the captured original frames.

This test release updates the CLI, core and desktop application metadata to
0.1.22 without updating dependencies. It does not change display modes on the
physical screens or save permanent display preferences. Accessibility permission
is still required for moving other applications' windows.

## Validation and test package

Validation passed with `RUST_TEST_THREADS=1 tools/check.sh`: 733 Rust tests,
six existing opt-in ignored tests, eight Python tests and eight JavaScript
checks, CLI validation, TypeScript and the production frontend build. The focused
display library passed all 55 tests, including 12 new memory-only window-session
and frame-setter regressions. These cover three monitors, secondary-only gathering,
exact position/size restoration, a smaller remote viewport, resize/idempotent
teardown, retained closed-window identities, partial setter failures, deadlines,
unsupported windows and missing-monitor UUID fallback.

The actual Accessibility setter also retries size after moving to the destination
monitor: applications can clamp a large size while the window still belongs to
the smaller virtual screen. A fake clamping application demonstrates the old
failure and verifies the full original size is restored by the shared setter.

Normal replies forward safe phase counts, timing and warnings from the isolated
helper to the server log. Helper, orphan and supervisor deadlines allow the new
bounded window work to complete without weakening eventual process cleanup.
Stop still returns a pending result promptly while cleanup remains supervised.

The initial parallel check exceeded the existing two-encoder VideoToolbox test
capacity. All 53 encoder tests, followed by the complete suite, passed in the
serial mode already used for native release checks. No production encoder
behavior or dependencies changed. Existing compiler and frontend bundle-size
warnings remain.

The tests did not create a remote display or move live application windows. The
opt-in live display smoke test now explicitly warns that it gathers/restores
windows and was not run against the active desktop. Three-monitor acceptance
below remains for the user after installing the package. DMG build, checksum,
source revision and Desktop delivery will be recorded after packaging.

Manual acceptance testing after the user installs the DMG must cover three
physical displays with ordinary windows at distinct positions and sizes:

- Connect and confirm windows from the original non-main displays gather on the
  virtual screen, while the original main display follows macOS's transition.
- Move and resize captured windows remotely, resize the RDP session, and
  disconnect. Verify all captured windows return to the original monitors and
  pre-connection frames, including windows originally on the main display.
- Repeat reconnect, normal server stop and dropped-connection cleanup. Confirm a
  resize within the same connection never restores or replaces the snapshot.
- Disconnect an original physical monitor and confirm its windows return visibly
  to a surviving physical screen. Check windows opened or closed during the
  session, minimized/full-screen windows, denied Accessibility access and an
  unresponsive application without blocking cleanup indefinitely.

See [virtual display setup and the full test procedure](virtual-display.md).
