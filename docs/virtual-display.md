# Virtual display and Guacamole resizing

The optional virtual display backend creates an extended macOS display named
**macRDP Remote Display**. The RDP session captures that display, and Display
Control requests change its actual macOS mode to match the client viewport.
The physical display backend remains the default.

This is experimental. Creation uses the undocumented `CGVirtualDisplay` API.
macrdp checks the available classes and method signatures at runtime and rejects
unsupported APIs or modes with a diagnostic. macOS updates can change this API;
native build and client testing are required on the target Mac. A failure does
not silently capture a different display.

## Enable it

In the macrdp app, open Settings > Display, select the virtual display backend
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
remote desktop. The previous display arrangement is restored on disconnect. It
is an additional screen in the same user session, with shared keyboard, pointer,
applications and audio. macrdp does not mirror displays or create a separate
user login session.

Starting the server does not create a virtual display. A background helper
creates it when an authenticated remote connection starts capture and keeps it
alive for that connection. Browser resizes change its mode in place. Disconnect
restores the previous primary display and removes the virtual screen; reconnect
creates a new one. Server shutdown also cleans up any active display.

## Main display during a remote session

The virtual display becomes primary before capture and pointer coordinates are
initialized. Its global origin is read back from macOS after the change. Resizing
the browser changes the same display's mode without replacing the saved physical
display arrangement. RDP resize reactivation remains part of the same connection.

macOS manages application window placement. Existing windows may remain on a
physical screen, and applications can remember their preferred display or Space.
The Dashboard's **Move windows to remote display** button gathers open windows
when an app opens on the physical screen. This is a manual action, available
while a client is connected to the virtual display. Windows already visible on
the remote screen stay in place. Larger windows are fitted where the application
allows it. Minimized, full-screen and unsupported windows may be skipped; the
button reports moved, skipped and failed counts. Accessibility permission is
required, as it is for remote keyboard and mouse input.

There is no automatic window mover or per-window restoration on disconnect.
macOS handles placement when the virtual display disappears. The manual action
uses bounded Accessibility calls for other apps and main-thread AppKit for
macrdp itself. It never queries macrdp's own Accessibility windows, which caused
the earlier WebKit crash. Window titles and contents are not logged.

Disconnect and normal server shutdown restore the prior primary display and
display origins before removing the virtual screen. These are temporary display
settings, not a change to saved system preferences. The helper also attempts restoration when its parent closes
the control pipe. A forced termination of the helper or a changed display layout
may prevent exact restoration. Dock autohide and macOS Spaces settings still
control the Dock's normal appearance.

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
screen. Its independent observer processes verify actual mode dimensions,
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
   and menu bar are available. Existing application windows follow macOS behavior.
3. Resize between 1280x720, 1920x1080, 2560x1440 and an odd viewport size such as
   2560x1305. Confirm the desktop dimensions change, not just the image scale.
   Check menu items, window controls and all four corners for pointer accuracy.
4. Disconnect and confirm the previous primary display and arrangement return
   and the virtual screen disappears. Reconnect with a different browser size:
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
   Repeat with macrdp's own window on the physical display and verify it moves
   without crashing. Disconnect while gathering and confirm cleanup still runs;
   the button must be disabled while disconnected or using the physical backend.

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
