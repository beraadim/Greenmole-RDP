# macrdp 0.1.10

Release candidate: 2026-10-13. This release improves connection startup,
shutdown, clipboard isolation, RDP compatibility and permission setup.

## Connection resilience

- Bitmap connections skip H.264 initialization. AVC starts only after the
  client confirms support, on one bounded native worker. Pending, failed or
  timed-out initialization continues with uncompressed GFX; late native results
  are destroyed on their owner. Resize can retry admission once the previous
  owner retires. The first delivered OpenH264 AVC420/AVC444 frames are independent
  keyframes, including both AVC444 views.
- Stop requests retain ownership and wait at most five seconds in the core.
  The UI shows stopping or cleanup pending, with a retry action. Restart remains
  blocked until the server thread, native capture, encoder retirement and actual
  display-worker restoration/shutdown complete. Startup cancellation is persistent
  and status polling cannot replace a new server's state with an old snapshot.
- Clipboard timeouts, protocol errors and queue limits quarantine clipboard for
  that connection while display/input continue. The dashboard reports this state;
  reconnect restores clipboard. Native pasteboard/file operations run on bounded
  workers and cannot block the watchdog or connection control callbacks.
  Already-admitted native writes cannot safely be interrupted; nothing new is
  admitted after quarantine. Ambiguous simultaneous local/remote copies preserve
  local clipboard content conservatively.
- RemoteFX preserves the negotiated entropy and operating mode, repeats headers
  for independently decodable images and advances video frame indices correctly.
  Complete updates respect the client's reassembly limit. Large images split
  into complete smaller updates; malformed/impossible limits fail boundedly.

## Disconnect investigation

The recent test logs after 19:20 Oslo time showed four TCP resets, three abrupt
TLS closes, one short-read/decode termination and one configured idle timeout.
Graphics resize reactivations were normal and are excluded from those counts.
No server write timeout or clipboard failure was logged in those sessions.

A TCP reset or abrupt TLS close establishes that the remote peer closed the
connection. It does not distinguish browser/client, guacd, proxy or network
causes. Gateway-side logs are required to establish the origin. The new protocol
and channel fixes remove concrete server-side compatibility/failure risks, but
are not presented as a proven explanation for those historical resets.

Connection diagnostics now distinguish peer reset, peer EOF, protocol error,
write timeout, idle timeout and server shutdown. They include numeric socket
error, delivered update/byte counts, last update size, socket write time and
graphics ACK state. Sanitized exports retain only selected operational data.
The default idle timeout is thirty minutes of keyboard/mouse inactivity;
passive viewing does not reset it. Settings explains this and accepts zero to
disable it. Existing preferences are retained.

## Permissions and release preparation

Startup checks required Screen Recording/Accessibility permissions without
opening system dialogs, and stays stopped if either is missing. Permissions
provides an explicit setup action only while no server/native resources remain.
Setup occurs before a virtual display can become primary. The microphone entry
was removed because current audio captures system output, not microphone input.

GUI managed TLS uses a fixed private directory, preserving an existing legacy
identity without probing the current working directory. Diagnostics export uses
a native Save dialog and atomically writes a private report to the selected path;
cancellation does not create a report. Clipboard file sharing may still require
consent for protected folders and can be disabled independently.

The release script supports Developer ID signing, hardened runtime, notarization,
ticket stapling, Gatekeeper assessment and final checksums. Public mode rejects
missing certificate/profile configuration. This test build remains ad-hoc signed
as requested; the Mac mini has no Apple signing identity installed. See
[public release preparation](public-release.md) for setup and upgrade acceptance.

## Acceptance checks

- Connect Guacamole and mstsc using each quality preset. Check consecutive screen
  changes, detailed/noisy images, resizing, double-clicks, dragging and reconnect.
- Start and immediately cancel. Stop during connection startup and resize. Check
  cleanup status, physical display restoration and prevention of overlapping starts.
- Test text/image/file clipboard sharing. Deny a folder request or leave it waiting;
  verify display/input remain usable and reconnect restores clipboard.
- Export diagnostics, cancel the Save dialog, then choose a destination. Confirm
  reset/timeout and clipboard-degraded reasons are retained without private content.
- On a fresh installation, deny required permissions and verify no listener or
  virtual display starts. Grant them on the physical display and restart as needed.

## Validation

Native tests use synthetic pixels, private pasteboards and fake stalled owners;
they never capture the user's desktop or post input. `tools/check.sh` passed on
the Apple Silicon Mac mini: 564 Rust tests (four manual experiments ignored),
eight Python tests, eight JavaScript tests, the CLI build and the TypeScript/Vite
production build. Four release preflight checks reject invalid public mode,
missing Developer ID identity/profile and notarization with ad-hoc signing.

The release workflow verifies the final app signature and DMG integrity. Delivery
also checks the mounted app's version, committed revision, architecture and
build-info sidecar, then verifies the checksum of the Desktop copy. Live
Guacamole/mstsc behavior and native permission/Save dialogs remain user acceptance
checks; automated validation does not launch or replace the running server.
