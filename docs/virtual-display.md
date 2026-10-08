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

The virtual display belongs to the logged-in macOS desktop. Supported application
windows move onto it automatically when the remote session starts, and return
when the client disconnects. It is an additional screen in the same user session,
with shared keyboard, pointer, applications and audio.
macrdp does not automatically mirror displays, change the primary display, or
create a separate user login session.

One native display object is retained in a background helper process for the
server's lifetime. Resizing changes
its mode in place, and an RDP disconnect leaves it available for reconnection.
Stopping the server releases the virtual display. The display starts at
1920×1080 and adopts the negotiated client size when capture starts.

## Automatic window placement

Window placement requires the same **Accessibility** permission used for remote
keyboard and mouse input. Grant access and restart the server if it was missing
when the server started. The physical display backend does not move windows.
For the virtual backend, placement begins after capture has adopted the client's
negotiated size. Each supported window's original display, position and size are
remembered once per connection. Remote viewport resizing keeps windows within
the new desktop without replacing those original return positions.

New windows are collected during the session. Windows opened on a physical
display retain that return position; windows opened on the virtual display return
to the physical display that was primary when the connection started. If an
original display has been unplugged, restoration uses a remaining physical
display and keeps windows within its bounds. Closed windows are ignored.

This uses macOS Accessibility window controls and is best effort. Full-screen,
already minimized, protected, and non-movable windows are not brought onto the
remote display. A moved window that is minimized later is returned without
unminimizing it when the application permits that. Applications can
limit resizing or expose only some of their windows; moving windows between
macOS Spaces is not guaranteed. Placement does not activate applications or
change their minimized/full-screen state. Missing permission and unsupported or
unresponsive windows are reported without recording window titles or contents.

Disconnects and normal server shutdown restore the saved layout. RDP resize
reactivation is part of the same connection and does not trigger restoration.
Restoration has a bounded wait, so an unresponsive application cannot indefinitely
block shutdown. Forced process termination cannot guarantee exact restoration.

## Guacamole connection

Set **Resize method** to **Display update** (`resize-method=display-update`) on
the Guacamole RDP connection. Leave its width and height unset to use the browser
viewport. These settings are per connection, so verify them even when a Windows
connection already resizes correctly. Guacamole's `reconnect` method starts a
new RDP session instead of resizing the current session.

Supply the server's configured username and password in the RDP client.
TLS-only connections now reject empty or incorrect credentials instead of
accepting an empty-password attempt. Authentication logs omit password values.

See the [Apache Guacamole display settings](https://guacamole.apache.org/doc/gug/configuring-guacamole.html#display-settings)
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
cargo test -p macrdp-windows
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
passed 129 native unit tests and the opt-in display smoke test. The native test
verified exact modes through 3840×2160, odd-size normalization, a stable display
ID across resizes, an unchanged physical display mode, and removal after shutdown.
The release app's helper also passed independent mode and cleanup checks; the
app bundle passed signature verification and the DMG passed checksum verification.
Live Guacamole/Windows client capture and pointer behavior still require the
manual checks below.

The window-placement tests cover geometry and session boundaries without moving
your application windows. The automatic window-placement build on the same Mac
passed 103 unit tests (27 RDP server, 53 core, 11 CLI, and 12 window-placement),
the release CLI and UI builds, app signature verification, and DMG checksum
verification. Actual Accessibility movement and restoration remain part of the
manual installation checks below.

The ignored `controlled_fixture_moves_and_restores`
test additionally exercises Accessibility moves, but requires an already trusted
test executable, a disposable fixture application's PID in
`MACRDP_WINDOW_FIXTURE_PID`, and a disposable virtual display ID in
`MACRDP_WINDOW_FIXTURE_DISPLAY_ID`. It disables discovery of other applications.
Do not point this test at an application containing real work. Normal test and
build commands leave it ignored; no Accessibility permissions are granted by
the build process.

1. Start with the physical backend and confirm its existing behavior.
2. Open several ordinary windows on the physical display and note their positions
   and sizes. Enable the virtual backend, restart, and connect with Guacamole.
   Confirm the windows move onto the remote display automatically.
3. Resize between 1280×720, 1920×1080, 2560×1440 and an odd size such as
   2560×1305. Check menu items, the Dock, window controls and all four corners.
   Repeat while idle and while rapidly dragging the browser window edges.
4. Disconnect and confirm the original window positions and sizes return. Reconnect
   at a different viewport size: windows should move back onto the remote display,
   while the display ID remains the same in the log.
5. Verify the log's negotiated size matches the actual virtual mode after
   viewport dimensions are normalized.
6. Arrange the virtual screen left of or above the primary screen, then reconnect.
   Check negative origins in the log and accurate input throughout the screen.
7. Confirm fixed resolution ignores client resize requests. Confirm saving a
   backend change while running reports restart required.
8. Stop the server and confirm the display disappears; restart and confirm a new
   display can be created. Exercise screen lock/unlock and capture recovery.
9. Open a new window during the remote session, resize it, and disconnect. Confirm
   it returns to a physical display. Repeat with a dropped network connection,
   rapid reconnect, and server stop. Minimized/full-screen windows should keep
   their state; a closed window must not interrupt restoration of the others.

Capture info logs include the selected display ID, global origin, logical point
size, output pixel size and native mode changes. Useful messages include
`Persistent display helper ready`, `Virtual display mode ready for capture`,
`Activated capture and pointer desktop size`, and
`Display controller released on its owning thread`.

Changing the macOS display arrangement during an active RDP session requires a
reconnect to refresh the selected display's global origin. This backend is not a
multi-user session isolation mechanism, and its performance has to be measured
on the target hardware.
