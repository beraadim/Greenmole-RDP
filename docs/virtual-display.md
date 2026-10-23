# Virtual display and Guacamole resizing

The optional virtual display backend creates an extended macOS display named
**macRDP Remote Display**. This legacy descriptor name is retained to preserve
the existing monitor identity across the Greenmole RDP rebrand. The RDP session
captures that display, and Display Control requests change its actual macOS mode to match the client viewport.
The physical display backend remains the default.

This is experimental. Creation uses the undocumented `CGVirtualDisplay` API.
Greenmole RDP checks the available classes and method signatures at runtime and rejects
unsupported APIs or modes with a diagnostic. macOS updates can change this API;
native build and client testing are required on the target Mac. A failure does
not silently capture a different display.

## Enable it

In the Greenmole RDP app, open Settings > Display, select the virtual display backend
and automatic resolution, then stop/start the server. Settings save
automatically; backend changes require a restart. The corresponding TOML
settings are:

```toml
display_backend = "virtual"
resolution = "auto"
width = 0
height = 0
```

The standalone server also accepts `--display-backend virtual`.
A fixed resolution is supported but disables
client-driven resizing. On a running virtual server, changing the configured
resolution or listening port requires a restart; this avoids overlapping virtual
display owners during the app's usual port migration.

The virtual display belongs to the logged-in macOS desktop. It becomes the main
display while a remote client is connected, bringing the Dock and menu bar to the
remote desktop. The previous display arrangement and captured window positions
are restored on disconnect. It
is an additional screen in the same user session, with shared keyboard, pointer,
applications and audio. Greenmole RDP does not mirror displays or create a separate
user login session.

Starting the server does not create a virtual display. By default, a background helper
creates it when an authenticated remote connection starts capture and keeps it
alive for that connection. Browser resizes change its mode in place. Disconnect
restores the previous primary display and removes the virtual screen; reconnect
creates a new one. Server shutdown also cleans up any active display.

### Reuse the virtual display between sessions

Settings > Display includes **Reuse virtual display between sessions**, an
experimental option that is **off by default**. Enable it with the virtual
backend selected and restart the service. CLI users can set
`reuse_virtual_display = true` in TOML or pass `--reuse-virtual-display`;
`--reuse-virtual-display=false` overrides an enabled file setting.

The first authenticated connection creates the display normally. Disconnect
stops capture and encoding, restores the physical display arrangement and window
positions, and disables the retained virtual display. The next connection
re-enables the same owned display instead of creating another object. The
display helper and its identity lease remain owned until server shutdown;
Stop and crash recovery still retire the helper and remove the display.
Starting an idle server creates no display, and a disabled display must not
remain an extended desktop available for local windows.

This tests whether avoiding repeated virtual-display creation and removal
reduces the local cursor regression observed after consecutive remote sessions.
Enabling/disabling and switching the primary display still change desktop
topology. It is not an established fix for a macOS or display-driver bug.
The enable/disable operation also uses a private macOS API and is checked at
runtime. Turn the option off and restart to return to removal on disconnect.

From 0.1.25, new sessions reuse a persisted monitor serial for the same macOS
user and RDP listening port. Changing the bind address does not change this
identity. The standalone display-controller API uses a separate default slot.
The descriptor's vendor, product, name and color characteristics stay fixed.
Without the reuse option, no display is kept alive while disconnected. Capture
and encoding stop in both modes.
Identity records are stored in `~/.macrdp/display-identities/`, with a private
directory and same-user files. Keep these records when updating the application
to preserve monitor identity.

An exclusive identity lease prevents two helpers from creating the same monitor.
Configuration selectors inherit that lease and validate a separate session
generation, so a late selector cannot act on a subsequent session's reused
identity. A busy or invalid identity slot rejects display creation; Greenmole RDP does
not work around it by inventing another monitor serial. Concurrent same-user
servers using the same port also share this slot, even on different bind IPs.
With reuse off, normal disconnect releases ownership. With reuse on, ownership
is retained while disabled and released at server shutdown. Do not delete
identity files to bypass a pending owner.

macOS previously retained a new ICC profile and display UUID for successive
sessions. Existing profiles and display preferences are left intact. Verify UUID
and profile-count stability across reconnects on the target macOS version:
constant descriptor identity is now provided, but the private virtual-display
API still controls macOS's UUID/profile behavior. This change has not established
the cause of a previously observed ColorSync/WindowServer backlog.

## Main display during a remote session

The virtual display becomes primary before capture and pointer coordinates are
initialized. Its global origin is read back from macOS after the change. Resizing
the browser changes the same display's mode without replacing the saved physical
display arrangement. RDP resize reactivation remains part of the same connection.

Before changing the main display, Greenmole RDP snapshots normal, movable windows on
the connected physical displays. It records each window's original display,
position and size, including windows on the original main display. It then
gathers windows from the original **non-main physical displays** onto the
virtual display automatically. Windows on the original main display are left
to macOS's normal primary-display transition, rather than moved a second time.
The snapshot belongs to that connection; browser resizes do not replace it.

On disconnect, Greenmole RDP restores the physical display arrangement and returns
captured windows to their original displays, positions and sizes. If an original
display has been disconnected, the window is fitted visibly onto a surviving
physical display. Closed windows are ignored, and an application can constrain
the size or placement of its windows. Minimized, full-screen and unsupported
windows may be skipped without changing their state. Accessibility permission
is required, as it is for remote keyboard and mouse input. Snapshot, automatic
gather and restoration each have a three-second time budget, so an unresponsive
app cannot block connection cleanup indefinitely. A timeout or denied access
can therefore leave some windows for macOS to place.

Window identity is retained through Accessibility objects for the lifetime of
the session, rather than matched using titles. Window titles and contents are
not read for identity or logged. Automatic window handling runs on the isolated
display helper's main thread and queries GUI applications out of process; it
never queries its own PID. The manual Dashboard action still uses main-thread
AppKit for Greenmole RDP itself, avoiding the earlier in-process WebKit crash. Saved
window state remains in memory and is discarded when the session ends.

The Dashboard's **Move windows to remote display** button remains available
during a virtual session for windows opened later or left on another screen.
Windows already visible remotely stay in place, and larger windows are fitted
where the application allows it. The button reports moved, skipped and failed
counts. Newly opened windows are not added to the connection's original
snapshot, so their placement when the virtual display is removed or disabled remains under
macOS control. Using the button does not overwrite captured windows' original
positions or sizes.

Disconnect and normal server shutdown restore the prior primary display and
display origins before removing or disabling the virtual screen. Configuration transactions
use session scope rather than requesting permanent layout changes. macOS can
still remember monitor identities and profiles. The helper also attempts restoration when its parent closes
the control pipe. A forced termination of the helper or a changed display layout
may prevent exact restoration. Dock autohide and macOS Spaces settings still
control the Dock's normal appearance.

Disconnect also releases both the display-sleep prevention assertion and remote
user-activity assertion. Expired activity assertions are harmless during cleanup;
unexpected release failures are logged and retained for a later cleanup retry.
Local input and other applications can still delay the normal display-idle timer.

## Guacamole connection

Set **Resize method** to **Display update** (`resize-method=display-update`) on
the Guacamole RDP connection. Leave its width and height unset to use the browser
viewport. These settings are per connection, so verify them even when a Windows
connection already resizes correctly. Guacamole's `reconnect` method starts a
new RDP session instead of resizing the current session.

The initial desktop size comes from the client's connection request. Later live
resizes require Guacamole's Display update setting; the server cannot infer a
browser viewport that the client does not send. Display Control is available
even when H.264 graphics are disabled or Guacamole uses bitmap graphics.

Supply the server's configured username and password in the RDP client.
TLS-only connections now reject empty or incorrect credentials instead of
accepting an empty-password attempt. Authentication logs omit password values.

See the [Apache Guacamole display settings](https://guacamole.apache.org/doc/gug/configuring-guacamole.html#rdp-display-settings),
the [live resize troubleshooting guide](guacamole-troubleshooting.md),
and the [mouse coordinate test procedure](mouse-coordinate-testing.md).

The RDP size is measured in framebuffer pixels. Virtual sizes are bounded to
320×200 through 3840×2160. The virtual desktop uses one pixel per logical point;
virtual HiDPI is not offered in this version. Existing physical Retina display
support is unchanged. Dimensions round down to multiples of eight before RDP
negotiation, matching the native mode increments. For example, a 2560×1305
viewport requests a 2560×1304 desktop.

Resize bursts use a 120 ms quiet period, capped at 400 ms. The old capture stops
before RDP reactivation. After reactivation, the selected display mode is applied,
and capture and pointer geometry are committed to the dimensions advertised to
the client.
Another resize received during activation is processed afterward. A native mode
that does not settle within three seconds fails the session rather than starting
capture with mismatched geometry.

## Validation on the Mac mini

Build/test from the repository root on macOS:

```sh
cargo test -p macrdp-display
cargo test -p macrdp-capture geometry::tests
cargo test -p ironrdp-server --lib
cargo test -p macrdp-core
cargo test -p macrdp-server
cargo build -p macrdp-server --release
cd macrdp-ui
npm ci
MACRDP_CLI_PROFILE=release ./build.sh
```

The build script's DMG must be copied to `beraadim`'s Desktop for manual install
and testing. Portable tests and cross-target type checking do not establish
successful macOS display creation, capture, or Guacamole behavior.

The native display smoke test is opt-in because it creates a temporary extended
screen and now gathers/restores ordinary application windows. Run it only when
that live desktop change is intended. Its independent observer processes verify actual mode dimensions,
persistent display identity, physical display preservation, and cleanup:

```sh
caffeinate -d -u -t 120 cargo test -p macrdp-display --test native_smoke --locked -- --ignored
```

Keep the display awake for this standalone mode test: synchronous macOS mode
selection can stall while displays sleep. Active RDP sessions already declare
remote user activity and hold a display-sleep assertion before capture starts.

On 2026-10-08, validation on the Apple M4 Pro Mac mini with macOS 26.6.2
passed 122 unit tests and the opt-in display smoke test. Fresh observer processes
verified no virtual screen while idle, promotion to primary at origin (0,0),
exact modes through 3840×2160, odd-size normalization, and a stable display ID
across resizes. Disconnect restored the physical display's primary status and
geometry and removed the virtual screen. Reconnect created a new display, which
was also removed on final cleanup. Physical display modes stayed unchanged.
The release CLI and UI builds passed; the app bundle passed signature
verification and the DMG passed checksum verification.
Live Guacamole/Windows client capture and pointer behavior still require the
manual checks below.

The subsequent manual window recovery build passed 143 native unit tests on
the same Mac, including six UI tests. Coverage includes excluding the current
PID before Accessibility access, rejecting actions on an inactive virtual
controller, queue ordering, cancellation/expiry, negative display origins,
logical versus pixel coordinates, and fitting oversized windows. The frontend
production build also passed. These checks do not replace testing the Dashboard
button against real application windows after installing the DMG.

1. Start with the physical backend and confirm its existing behavior.
2. Enable the virtual backend and automatic resolution, restart, and confirm no
   virtual screen exists while idle. Connect through Guacamole and confirm the
   virtual display is created and becomes primary, with the Dock
   and menu bar are available. Before connecting, place ordinary windows on each
   physical display and note their positions and sizes. Windows from non-main
   physical displays should gather automatically; the original main display's
   windows follow macOS's primary-display transition.
3. Resize between 1280x720, 1920x1080, 2560x1440 and an odd viewport size such as
   2560x1305. Confirm the desktop dimensions change, not just the image scale.
   Check menu items, window controls and all four corners for pointer accuracy.
4. Disconnect and confirm the previous primary display and arrangement return
   and the virtual screen disappears. Confirm all captured windows return to
   their original displays, positions and sizes, including those originally on
   the main display. Reconnect with a different browser size:
   a new virtual screen is created. Only resizes within one connection keep its ID.
5. Repeat while rapidly resizing, with a dropped network connection, and while
   stopping the server. A resize within one session must not restore the layout.
6. Confirm fixed resolution ignores client resize requests. Confirm changing
   backend or virtual resolution settings while running reports restart required.
7. Stop the server and confirm the virtual display disappears. Exercise screen
   lock/unlock and capture recovery, and repeat using Windows Remote Desktop.
8. With a virtual session connected, open ordinary windows on the physical
   display and click **Move windows to remote display** on the Dashboard.
   Confirm they become usable remotely, existing remote windows stay put, and
   minimized/full-screen windows are handled without forcing their state.
   Repeat with Greenmole RDP's own window on the physical display and verify it moves
   without crashing. Disconnect while gathering and confirm cleanup still runs;
   the button must be disabled while disconnected or using the physical backend.
9. Repeat the connect, resize and disconnect cycle with three physical displays,
   including displays arranged to the left or above the original main display.
   Move and resize a captured window remotely, then confirm its pre-connection
   frame returns on disconnect. Verify that resizes do not replace the snapshot.
10. Disconnect an original secondary display during the remote session, then
    disconnect RDP and confirm its captured windows remain visible on a surviving
    physical screen. Repeat with a closed window, a newly opened window,
    minimized/full-screen windows, denied Accessibility access and an unresponsive
    app; cleanup must remain bounded and logs must omit window titles.
11. Enable **Reuse virtual display between sessions** and restart. Confirm no
    display is created before the first connection. Across repeated sessions,
    verify the same owned display is disabled after disconnect and re-enabled
    on reconnect, with no active virtual desktop or capture while disconnected.
    Check that captured windows and newly opened remote windows remain visible
    on physical displays after disconnect. Stop while disconnected and confirm
    the retained helper and display are removed. Disable reuse and restart to
    confirm the original removal-on-disconnect behavior returns.

Capture info logs include the selected display ID, global origin, logical point
size, output pixel size and native mode changes. Useful messages include
`Persistent display helper ready`, `Virtual display mode ready for capture`,
`Activated capture and pointer desktop size`, and
`Display controller released on its owning thread`.
Resize diagnosis also logs `Initial client desktop dimensions`, dynamic-channel
creation states, `Display Control channel opened`, and
`Received client Display Control layout`. A rejected or unopened Display Control
channel points to negotiation or Guacamole settings; received layouts followed
by `Applying resolution change` identify requests accepted by the server.

Changing the macOS display arrangement during an active RDP session requires a
reconnect to refresh the selected display's global origin. This backend is not a
multi-user session isolation mechanism, and its performance has to be measured
on the target hardware.
