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

The virtual display belongs to the logged-in macOS desktop. Move application
windows onto it using macOS display/window controls. It is an additional screen
in the same user session, with shared keyboard, pointer, applications and audio.
macrdp does not automatically mirror displays, change the primary display, or
create a separate user login session.

One native display object is retained in a background helper process for the
server's lifetime. Resizing changes
its mode in place, and an RDP disconnect leaves it available for reconnection.
Stopping the server releases the virtual display. The display starts at
1920×1080 and adopts the negotiated client size when capture starts.

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

1. Start with the physical backend and confirm its existing behavior.
2. Enable the virtual backend and restart. Confirm an extended display appears
   in macOS Displays settings. Move a window onto it and connect with Guacamole.
3. Resize between 1280×720, 1920×1080, 2560×1440 and an odd size such as
   2560×1305. Check menu items, the Dock, window controls and all four corners.
   Repeat while idle and while rapidly dragging the browser window edges.
4. Disconnect and reconnect at a different viewport size. Confirm the display
   ID remains the same in the log and windows remain on the virtual screen.
5. Verify the log's negotiated size matches the actual virtual mode after
   viewport dimensions are normalized.
6. Arrange the virtual screen left of or above the primary screen, then reconnect.
   Check negative origins in the log and accurate input throughout the screen.
7. Confirm fixed resolution ignores client resize requests. Confirm saving a
   backend change while running reports restart required.
8. Stop the server and confirm the display disappears; restart and confirm a new
   display can be created. Exercise screen lock/unlock and capture recovery.

Capture info logs include the selected display ID, global origin, logical point
size, output pixel size and native mode changes. Useful messages include
`Persistent display helper ready`, `Virtual display mode ready for capture`,
`Activated capture and pointer desktop size`, and
`Display controller released on its owning thread`.

Changing the macOS display arrangement during an active RDP session requires a
reconnect to refresh the selected display's global origin. This backend is not a
multi-user session isolation mechanism, and its performance has to be measured
on the target hardware.
