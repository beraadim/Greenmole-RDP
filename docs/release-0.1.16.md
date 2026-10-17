# Release 0.1.16: experimental browser AVC420 compatibility

This release pairs with Guacamole 1.6.1-private.13's optional browser video path.
Guacamole advertises AVC420 to the RDP server only after browser opt-in and a
successful secure-context WebCodecs capability probe. macrdp recognizes the
exact RDP client name `GuacamoleAVC420`, preserving the existing Guacamole
keyboard translations while selecting continuous full-frame AVC420 for that
connection. Native Windows and ordinary Guacamole connections keep their
existing codec negotiation and small-update paths.

## Continuous video and decoder recovery

An opted-in session uses AVC420 even if the host's saved chroma preference is
AVC444. Once the encoder is available, small changes and baseline refreshes stay
within the AVC stream rather than inserting raw GFX image patches that would
terminate passthrough. Startup and encoder failure still allow images. Lossless
desktop remains an image-only policy and is not silently converted to lossy AVC.
The marker is a transport preference and grants no authentication privileges.
It resets on disconnect so a later ordinary client cannot inherit it.

VideoToolbox IDR output now requires both SPS and PPS in-band before the encoded
slices. A new browser decoder can start after reconnect or resolution changes
without cached parameter sets. Completed native output that is empty or rejected
now forces the next IDR: the encoder may already have advanced its reference
pictures, and a following P-frame must not refer to an access unit the client
never received. An asynchronous callback still in progress is not treated as a
rejected frame.

The browser experimental path retains Guacamole's software-decoded image
baseline for recovery. macrdp's graphics indicator describes Mac-to-gateway RDP
transport; Guacamole's Display menu reports actual browser video status.

## Consistent video and fallback colors

Real browser integration exposed a color conversion mismatch: padded OpenH264
frames used limited-range BT.601, while accelerated conversions used full-range
BT.601 and H.264 metadata left the matrix or range unspecified. RDP decoders
apply full-range BT.709, so colored desktop content could change brightness or
hue between browser video and Guacamole's decoded image fallback.

Capture, scalar and vImage conversions now consistently use full-range BT.709,
as required by [MS-RDPEGFX §3.3.8.3.1](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/954d7546-6873-4466-95c8-20a7569c43e5).
Both encoders explicitly signal matching color primaries, transfer function,
matrix and full range in their H.264 output. The shared conversion also keeps
AVC444 consistent. Padded software frames average each 2×2 chroma block rather
than sampling only its top-left pixel.

Zero-copy NV12 capture explicitly selects the same matrix. Its assignment uses
the permanent native CoreGraphics BT.709 constant: the pinned ScreenCaptureKit
1.5.4 string setter creates a temporary CFString, but Apple's configuration
property does not retain that value. Bypassing that setter avoids leaving a
dangling native reference. A configuration readback test verifies the matrix
without starting screen capture.

## Testing and activation

Synthetic tests exercise AVC wire framing, visible crop versus macroblock
padding, raw startup followed by IDR and P-frames, resized surfaces, missing
parameter sets, native output rejection and per-connection preference cleanup.
The maintained `macrdp-encode` example `export_avc420_fixture` exports generated colored
quadrants and a moving marker through both real encoders; it never captures the
user's desktop. The browser tests use those real IDR/P/forced-IDR sequences to
check WebCodecs rendering and image fallback.
Real encode/decode tests also check saturated red, green and blue, black, white
and gray through scalar, accelerated and padded inputs. They verify reconstructed
RGB values against the independent RDP inverse matrix, rather than checking
dimensions or gray pixels alone.

After installing the Desktop DMG, choose Balanced or High quality in macrdp and
hardware encoding when available. In Guacamole over HTTPS, open the connection
menu's Display section, enable **Experimental AVC420 browser decoding**, then
fully disconnect and reconnect. The checkbox defaults off and is stored only in
that browser. HTTP on a LAN address or a missing decoder keeps the image path.
Lossless desktop also keeps images. Turn the checkbox off and reconnect to
compare the same workload using the original transport.

Test window switching, scrolling, typing, static text, resolution changes,
reconnect and explicit display refresh. After image fallback, the desktop should
be complete and responsive. No measured live performance improvement is claimed
by the synthetic codec or browser checks.

## Automated validation

`RUST_TEST_THREADS=1 tools/check.sh` passed: 668 Rust tests, eight Python tests and eight JavaScript
tests, with six existing opt-in experiments ignored. The CLI, TypeScript and
production frontend builds passed. Focused color validation included all 37
encoder tests and 31 capture tests. The loopback-only `avc420_fixture_server`
example also compiled; it can serve the synthetic encoded fixtures through the
actual RDP stack without capturing a screen or injecting input. Its synthetic
test listener is restricted to IPv4 loopback and is not part of the app.
The fixture requires a disposable TLS certificate and the public test-only
credentials `synthetic-fixture` / `synthetic-fixture`, satisfying the existing
acceptor policy. These example-only settings do not alter the packaged app or
its authentication behavior.

One parallel rerun hit an existing clipboard test's empty private-pasteboard
assertion. The exact test passed in isolation; no clipboard production code
changed. The final complete run serialized native tests, retaining all assertions.

## Verified Desktop delivery

Built from source commit `02350147ee662da54f6633867f4c23e95d925547` and delivered
to the Mac mini Desktop:

`/Users/beraadim/Desktop/macrdp_0.1.16_aarch64_02350147ee66.dmg`

SHA-256:
`72989ed4cc1018d0b6823b4f5f40722935b62ceeb20a2521bbf8d7cde8466aff`

The 15,367,483-byte image and checksum/build-info sidecars were copied with
verified destination hashes. Disk-image validation, read-only mount inspection
and ad-hoc deep strict signature checks passed. The mounted app reports the
expected version, architecture and source revision. Three packaged-helper smoke
checks reject unexpected arguments and invalid initialization before starting a
server or querying capture. The image mount was detached after inspection.
Only the superseded interim 0.1.16 image and its two verified sidecars were
removed; installation and live-session testing remain with the user.

Guacamole 1.6.1-private.13 was deployed on Oberon with configuration and runtime
settings preserved. Its exact packaged browser bundle and final guacd runtime
passed the synthetic end-to-end path with both real encoders: twelve presented
video frames, injected decoder failure, fresh complete native image repair and
an ordinary image-only reconnect. Sampled RGB errors were at most three levels
per channel. Temporary fixture services and SSH forwards were removed. These
functional checks do not measure a live desktop's performance.
