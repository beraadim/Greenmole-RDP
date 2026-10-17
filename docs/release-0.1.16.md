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

## Testing and activation

Synthetic tests exercise AVC wire framing, visible crop versus macroblock
padding, raw startup followed by IDR and P-frames, resized surfaces, missing
parameter sets, native output rejection and per-connection preference cleanup.
The maintained `macrdp-encode` example `export_avc420_fixture` exports generated colored
quadrants and a moving marker through both real encoders; it never captures the
user's desktop. The browser tests use those real IDR/P/forced-IDR sequences to
check WebCodecs rendering and image fallback.

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

`tools/check.sh` passed: 664 Rust tests, eight Python tests and eight JavaScript
tests, with six existing opt-in experiments ignored. The CLI, TypeScript and
production frontend builds passed. The loopback-only `avc420_fixture_server`
example also compiled; it can serve the synthetic encoded fixtures through the
actual RDP stack without capturing a screen or injecting input. Its synthetic
test listener is restricted to IPv4 loopback and is not part of the app.
The integration fixture was subsequently hardened to require a disposable TLS
certificate because FreeRDP rejects the library's unencrypted test mode; this
example-only change does not alter the packaged app.

## Verified Desktop delivery

Built from source commit `c38c41c2c6e10ec39913932819f3fdd8a14fad22` and delivered
locally to the Mac mini:

`/Users/beraadim/Desktop/macrdp_0.1.16_aarch64_c38c41c2c6e1.dmg`

SHA-256:
`e8a5fad8a337fff2b49680f4bf889563f692d0a6691423eba29982b323f6f975`

The 15,367,780-byte image and checksum/build-info sidecars were copied to the
Desktop, with destination hashes checked. Ad-hoc deep strict signature
verification and disk-image verification passed. A read-only image mount
confirmed app version, source revision and signature, then was detached.
Three packaged-helper smoke checks rejected unexpected arguments and invalid
frame-rate initialization before starting a server or querying capture.
Installation remains with the user.
