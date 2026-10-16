# Release 0.1.14: supervised recovery and local desktop rescue

## Capture-service failure recovery

The October 16 incident began at 15:34:56 local time when Apple's `replayd`
service exited with SIGTRAP. macrdp 0.1.13 continued using fallback capture, but
an outstanding native operation retained the capture owner. At 16:39:41 the
30-minute input idle timeout closed the session. Authenticated reconnects then
failed because capture was still retiring, and the same ownership barrier kept
the remote display primary until the Mac was restarted. The evidence does not
establish that locking the client caused the capture-service crash.

The desktop application and command-line server now supervise a separate RDP
worker process. Native capture, encoding and session ownership remain inside
that worker; pixels still travel directly from the worker to the RDP socket.
The supervisor exchanges bounded control messages and operational snapshots,
so isolation does not introduce a second frame-copy or image-encoding path.

The supervisor detects native calls pending for 30 seconds, a worker that stops
reporting health, and worker crashes. It first requests an orderly stop, then
terminates only the process it owns if cleanup cannot finish. The display
owner receives EOF independently and gets time to restore the physical layout.
Remaining members of that worker's isolated process group are terminated only
after that grace period. The worker PID remains reserved until group cleanup,
preventing PID reuse from targeting an unrelated process.

The display helper also watches its parent from an independent thread. If the
parent dies while native restoration is blocked, the helper gives normal EOF
cleanup six seconds, then exits only itself without running potentially stuck
native destructors. The server worker has a corresponding bounded parent-EOF
failsafe for its own isolated process group.

Replacement starts only after removal of the old remote displays is verified.
Recovery permits two automatic replacements within five minutes, then leaves
the service stopped with an error. A quiet desktop is healthy; missing images
or keyboard input alone do not trigger this watchdog. The existing idle setting
still determines when an unattended connection ends. Clients reconnect and
authenticate to a fresh session after a worker restart.

Credentials travel through private anonymous pipes, never command arguments or
temporary configuration files. Accepted live settings and the managed TLS
directory are retained for replacements. Settings acknowledgements, connection
events, metrics sequencing and CLI performance reports cross the process
boundary without sharing the child's stdout with human-readable output.
Only the supervisor writes and rotates the application diagnostic file.

## Local desktop recovery

The dashboard has a **Recover local desktop** action. Right-clicking the tray
icon exposes **Recover local desktop and stop server** in a native menu that
does not depend on the webview. Recovery stops the worker, verifies remote
display removal, gathers ordinary windows still unreachable on surviving
displays, and shows macrdp on a local monitor. Windows already accessible on a
secondary screen stay there. Full-screen, minimized and unsupported windows
retain their existing state. Inspection failures and partial/time-limited
window moves are reported as incomplete recovery.

The tray popover uses the monitor where it was clicked, including negative
origins and display scaling. Relaunching macrdp reveals its controls on a local
display instead of only focusing a hidden window on the remote primary.

A second invocation can request recovery without opening a window:

```sh
/Applications/macrdp.app/Contents/MacOS/macrdp-ui --recover-local-desktop
```

Use the equivalent path if the test app is on the Desktop. The command contacts
the running application through a private same-user Unix socket. The socket is
mode 0600 in a mode 0700 directory, checks peer identity, uses an exclusive owner
lock, and accepts only the bounded recovery command. It does not expose an
additional network listener. The service remains stopped after manual recovery.

## Diagnostics and validation

Native operation snapshots record the owner PID, operation, elapsed time, stop
intent and pending cleanup. Recovery logs include the failing stage and its
duration, the worker PID, replacement attempts and display-removal outcome.
Diagnostic exports use schema 4 and include selected supervision fields while
continuing to omit credentials, input, clipboard content and free-form errors.

Fault tests use gated fake native streams and isolated fixture processes,
without changing the developer's live display or sending input. They exercise
blocked startup, configuration and stop, child death, parent-pipe closure,
malformed/oversized messages, settings acknowledgements, descendant cleanup,
restart limits and cancelled startup. Local control tests use real Unix sockets
and check ownership, permissions, competing owners and malformed requests.

`tools/check.sh` passed for this release: 651 Rust tests, eight Python tests,
eight JavaScript tests, CLI compilation, and the production frontend build.
Six existing opt-in native experiments remain excluded from the default suite.
The fault fixtures do not crash Apple's services or interrupt a live session.

These changes recover the observed capture-service failure while WindowServer
is responsive. If macOS cannot remove a display or answer ownership queries,
macrdp reports cleanup pending and blocks a replacement rather than claiming
the desktop has been restored. Native streams and callback state are never
freed merely because a deadline expired.

For installation acceptance, connect through Guacamole and Windows Remote
Desktop Connection, lock or sleep the client, then reconnect. On the physical
Mac, test the tray recovery action and confirm the remote display disappears,
ordinary windows become reachable, and the server stays stopped until Start is
selected. Also test the local recovery command while the application window is
hidden. Full live macOS acceptance remains the user's DMG test.

## Command modifier restoration

Fix the stuck Command modifier reported after installing 0.1.13. That release
put Command flags on both the shortcut key-down and key-up, but never emitted a
modifier release. A private event source alone does not prevent macOS from
keeping the last event's modifier state active. Pressing and releasing Alt
worked around the problem because Alt maps to a physical Command key.

Every translated Ctrl+C, Ctrl+V and Ctrl+Enter event now posts an explicit
`FlagsChanged` restoration event immediately after the shortcut. This includes
key-down, repeats, key-up and releases during session cleanup, so a missing
client key-up cannot strand the temporary Command flag. The two native events
share one private event source and are both allocated before either is posted;
allocation failure cannot leave a half-posted shortcut without its restoration.

The restoration uses a fresh HID modifier snapshot for each event, preserving
unrelated modifiers. Both device-specific Command flags and the aggregate flag
are cleared unless the HID key table reports a genuinely held Command key
(including a remote Alt press). A held left or right Command key is preserved.
Releasing Ctrl before the letter does not reuse an earlier saved Ctrl state.

The native event construction and restoration live in a focused input module.
Trace logging records that modifiers were restored, without logging typed text
or clipboard contents. The mappings, Norwegian AltGr handling, cursor memory
fix and startup input backpressure from 0.1.13 remain in place.

## Validation and acceptance

Native tests inspect the full two-event sequence without posting keyboard
input to the user's desktop. They cover all mapped keys, keypad Enter, Shift,
repeated presses and releases, stale Command flags for both sides, unrelated
modifiers, genuinely held left/right Command keys and Ctrl-first release.

Install the Desktop DMG and test Ctrl+C, Ctrl+V and Ctrl+Enter with both Ctrl
keys. After each shortcut, release the keys and type ordinary text or click
normally; no Alt press should be needed. Also check holding Ctrl across copy
then paste, releasing Ctrl before the letter, Shift variants, AltGr symbols,
and reconnect. Test Windows Remote Desktop Connection and Guacamole. Live
macOS delivery still needs this user acceptance check; the automated event
tests deliberately do not send shortcuts into a running application.
