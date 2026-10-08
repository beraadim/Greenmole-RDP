# Mouse coordinates and resize verification

The UI server and standalone server share `macrdp-capture::MouseCoordMapper`.
RDP positions are mapped through the captured content rectangle to CoreGraphics
logical display coordinates, including the selected display's signed origin.
Capture selects the exact physical or virtual display ID whose geometry is used
for input. ScreenCaptureKit's frame `contentRect` is in surface
points; capture converts it to output pixels with `scaleFactor` before mapping.
The fit described by `contentScale` is already included in that rectangle.

A Display Control request queues a desktop size. Resize bursts are coalesced
with a 120 ms quiet period and a maximum 400 ms wait. The request does not immediately change
the input transform. The display stream requests RDP reactivation, then creates
capture and encoding resources for the size advertised in Demand Active. Only
that activation commits the mouse desktop size. A later queued resize gets a
separate activation, even if it arrived during the preceding handshake.

Button packets carry their own absolute position. Both fast-path and slow-path
input deliver that position before the button; wheel position fields are ignored
as required by MS-RDPBCGR.

## Automated checks

On the Mac build host, from the repository root:

```sh
cargo test -p macrdp-capture geometry::tests
cargo test -p ironrdp-server --lib
cargo test -p macrdp-core
cargo test -p macrdp-server
cargo build -p macrdp-server --release
cd macrdp-ui
npm ci
MACRDP_CLI_PROFILE=release ./build.sh
```

The geometry tests cover Retina and fractional scale factors, pillarboxing,
letterboxing, invalid metadata, changed host bounds, and reset after resize or
capture fallback. Pointer packet tests cover button-only coordinates, combined
move/button flags, simultaneous buttons, extended buttons, and wheel precedence.
See [virtual display setup and testing](virtual-display.md) for a viewport-sized
macOS desktop and additional lifecycle checks.

## Client verification

### Guacamole connection settings

Set the RDP connection's **Resize method** to **Display update**, equivalent to
`resize-method=display-update`. Guacamole then sends Display Control requests
when the browser viewport changes size. Leave the connection's width and height
unset to use the initial client viewport, and use `resolution = "auto"` in
macrdp to allow live resizing. Guacamole's `reconnect` method instead ends and
reopens the RDP session; an unset resize method does not send live resize requests.

A connection that already resizes a Windows host may already have this setting.
Verify it on the Mac connection as Guacamole settings are per connection.
See the [Apache Guacamole display settings documentation](https://guacamole.apache.org/doc/gug/configuring-guacamole.html#display-settings).

### Test sequence

Use both Guacamole and Windows Remote Desktop Connection. Choose resolutions
within the host's capture limit. For a 1920×1200 source, useful targets include
1920×1200, 1280×800, 1920×1080, and 2560×1305 if supported. Include a Retina
display when available.

1. Connect at each target size. Test the center, all four corners, menu items,
   window close buttons, and the Dock. Check the visible cursor and click target.
2. With automatic resolution enabled, resize an existing session between a
   matching aspect ratio and a different one. Repeat while the desktop is idle,
   and resize several times quickly. Check movement and clicks after each resize.
3. Disconnect and reconnect at a different size. Change the host display mode
   and reconnect without restarting the server.
4. Test left, right, and middle clicks, including clicks immediately following
   a large pointer movement. Scroll without moving the pointer and verify that
   it stays in place.
5. Test with fixed server resolution. A client layout request should leave the
   negotiated desktop and input scale unchanged.

Record client name, client desktop size, Mac display mode, whether the session
was freshly connected or resized, and the server log. Useful info-level messages:

- `Applying resolution change`: requested/clamped size and request source.
- `Requesting RDP reactivation for display resize`: next negotiated desktop.
- `Activated capture and pointer desktop size`: committed dimensions.
- `Mouse coordinate mapper: visible frame geometry changed`: output pixel
  rectangle, RDP content rectangle, and logical display dimensions.
- `Screen capture started`: selected display ID, dimensions, and pixel format.

For a remaining scaling discrepancy, enable trace logging for
`macrdp_capture` to include `ScreenCaptureKit content geometry`, which records
the original point rectangle, `scaleFactor`, `contentScale`, and converted pixels.
Do not infer successful live client testing from unit tests or a successful build.
