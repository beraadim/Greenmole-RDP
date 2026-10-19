# Release 0.1.24: guarded private-session test mode

Add the opt-in **Settings > Security > Guarded private session** option for the
virtual display backend. The service must restart to apply a policy change.
Ordinary mode remains the default.

Authenticated connections must establish an independent guard before wake,
input, capture or connection publication. The guard covers local screens and
blocks local input while permitting marked remote events. Remote modifiers are
isolated from physically held keys. If protection cannot be established, the
connection is rejected rather than downgraded to ordinary control.

The guard snapshots macOS lock state before waking the session. Initial locked
or unknown state requires a verified lock before protection is released. A
deliberate RDP disconnect may restore an initially unlocked desktop. Network
loss, internal channel closure, Stop, emergency recovery and guard failure force
locking. The mode never stores or enters a macOS password and never unlocks
macOS automatically.

Guard ownership is independent of the capture worker's process group. A bounded
heartbeat lease detects worker death or loss of control; input-tap loss and unsafe
display changes also trigger recovery. Pending lock verification retains local
barriers and a per-user generation lease. New protected connections and local
recovery completion cannot bypass that pending owner. Local recovery does not
activate the application over the verified macOS login screen.

This is an experimental guard for a shared macOS session, not an isolated login
or a guarantee against local administrator software, trusted input-injection
tools, operating-system interruption or physical power controls. macOS may deny
the active session tap, especially with incomplete permissions. Such a connection
must fail rather than claim protection. Compatibility of lock detection and
locking needs device testing; an unknown initial state locks conservatively.

Secure Event Input can bypass keyboard taps without disabling them. Admission is
refused while it is active; activation during a guarded session ends remote control
and requests locking. Some password fields and locked/login-screen states may
therefore be unavailable in this mode. The permission page now exposes Input
Monitoring only when private mode is configured and requests it only through
explicit setup while stopped. Ordinary mode's permissions remain unchanged.

See [setup, limits and device tests](private-sessions.md). All automated native
checks for this release use synthetic backends, anonymous-pipe subprocesses or
event construction only. They must not post input, create active taps or screen
covers, lock the host or start a live remote connection.

## Validation and test package

Full serial release checks passed: 775 Rust tests, six existing opt-in ignored
tests, eight Python tests, eleven JavaScript tests, TypeScript and the production
frontend build. This includes 19 new privacy-policy, lease, protocol and real
subprocess/pipe monitor tests, plus private input event construction, modifier
isolation, admission rejection, restart policy, permission and recovery cases.
The privacy suite passed again after hardening its synthetic child reaper.
Existing compiler and frontend bundle-size warnings remain.

An independent source review covered guard lifetimes, FFI, input admission and
gating, lock-state policy, permission checks, supervisor recovery, screen covers
and the documented limitations. No live input filtering, covering or lock action
was run on the build host. Exact Desktop delivery evidence follows the committed
source build. The installed/running application is left for the user to replace
and test, with the new option initially disabled.
