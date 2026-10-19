# Guarded private sessions

macrdp 0.1.24 adds an opt-in **Settings > Security > Guarded private session**
option. Select the virtual display backend first. Restart the service after
changing this option; an active connection's policy never changes underneath it.
The CLI configuration uses `private_session = true` and
`display_backend = "virtual"` in its protected TOML file.

The mode covers the local screens and filters local keyboard, pointer, scroll
and tablet input while accepting macrdp's marked remote input. Remote modifiers
must come from the remote session, not from keys held at the physical keyboard.
Black covering does not turn a monitor's backlight off.

Admission happens after RDP authentication, before waking the desktop, publishing
the connection or starting capture. The guard records the initial macOS lock
state. If it cannot establish input blocking and screen covering, it rejects
the connection instead of continuing without protection. No administrator or
macOS login password is stored, requested or automatically entered by this mode.
RDP authentication does not bypass macOS authentication.

On a deliberate client disconnect, a session that began locked is locked again
before the guard removes local protection. An unknown initial state also requires
a verified lock. Network loss, service Stop, emergency recovery and guard failure
always require locking, including when the Mac started unlocked. The mode never
automatically unlocks the Mac. Local recovery ends remote control and returns to
the macOS login screen; it does not expose the remote user's unlocked desktop.

The guard runs in an independent process group, outside the capture worker's
retirement group. Its bounded anonymous-pipe lease detects worker failure or
loss of heartbeats. Input-tap disablement and unsafe display changes also end
remote control and trigger locking. If macOS does not confirm the lock, the
guard retains its remaining barriers and its per-user ownership lease, preventing
a second protected session from replacing pending recovery. The application
must report recovery as pending until that lease is released. Guard diagnostics
contain status and fixed failure reasons, without input contents or routing tags.
The dashboard input inspector is redacted in this mode. Low-level third-party
protocol tracing is not a safe way to handle sensitive sessions.

## Permissions and limits

Grant Screen Recording, Accessibility and Input Monitoring access to the test
application before starting a private service. The Permissions page requests
Input Monitoring only for configured private mode through explicit setup while
the service is stopped. Ordinary mode keeps its previous permission requirements.
The active session
tap is a capability check; some macOS/security configurations may not permit it.
The application rejects protected connections in that case. Do not run the
entire server as root as a workaround. Ad-hoc test builds can require permission
approval again after replacement.

This mode still shares the logged-in macOS session. It is not an isolated Windows
RDP login, and does not defend against local administrator software, other
trusted input-injection tools, forced power-off or operating-system compromise.
macOS can disable an event tap; recovery requests a lock promptly but cannot
promise an atomic, uninterrupted operating-system security boundary. Lock-state
inspection and locking depend on macOS facilities that need compatibility testing
on supported releases. Do not describe this experimental mode as hardened local
access isolation. The effective event-mask check covers keyboard, pointer,
scrolling and tablet events; multi-finger Spaces/Mission Control gestures and
media/system controls still require hardware tests and must not be assumed to
behave like ordinary pointer events.

Secure Event Input prevents keyboard taps from seeing events in some password
fields and login contexts. Private mode refuses admission while this facility is
active and ends an existing session if it becomes active, requesting a macOS lock.
Consequently, some locked-screen connections or password-entry workflows may not
be usable in this experimental mode. This is a deliberate refusal of a known
input-blocking bypass, not a macOS authentication bypass. Hardware tests are
required; an event-tap guard cannot provide privileged input-device isolation.
See [Apple's Secure Event Input note](https://developer.apple.com/library/archive/technotes/tn2150/_index.html).

## Device testing

Automated checks use state-machine, event-construction and subprocess fixtures.
They never block input, cover screens, lock the build host or start live RDP.
After installing the new DMG, test in a controlled environment with a recovery
route available:

1. Enable the option with the virtual backend, restart, and connect normally.
   Verify all three local screens are covered and local keys, clicks, scrolling,
   trackpad, multi-finger gestures and modifier keys cannot affect remote work. Verify the remote
   keyboard and pointer still work, including shortcuts and Caps Lock.
2. Start from a locked Mac and authenticate normally through the remote session
   if macOS permits it. Disconnect and verify a password is needed locally.
3. Start unlocked, then test an explicit disconnect, browser/network loss and
   service Stop. Failures and Stop must leave macOS locked. A deliberate client
   disconnect may restore the initially unlocked state; browser closure may be
   reported as transport loss and therefore lock conservatively.
4. Test local desktop recovery. It must stop remote control and lock before
   releasing local input. It must not activate the dashboard over recovery.
5. Test worker failure and display hotplug during a protected session. Verify
   remote control ends, macOS locks, the virtual display is removed and later
   sessions cannot bypass pending recovery. Do not kill the guard process as an
   ordinary recovery action.
6. Test missing permissions. A rejected protected connection must never turn
   into an unprotected session. Test ordinary mode separately with the option off.
