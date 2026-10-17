# Release 0.1.19: AVC444 browser transport

Adds the matching server side of Guacamole private.14's experimental AVC444
browser path. The exact `GuacamoleAVC444` client marker keeps supported video
updates continuous, including small desktop changes and idle keyframe refresh.
It permits AVC444 only when Full color is configured and negotiated. The
existing `GuacamoleAVC420` marker remains explicitly AVC420-only; ordinary RDP
clients retain their capability-based selection.

## Encoding and recovery

AVC444's main and auxiliary pictures use one ordered H.264 reference stream.
VideoToolbox already used this arrangement; OpenH264 now does too, instead of
independent contexts. Software AVC444 uses an encoder mode that respects disabled
scene-change detection, preventing alternating luma/chroma views from causing
an unnecessary IDR on every access unit. Quantizer controls and no-skip behavior
remain active. Empty or incomplete pairs arm reference repair rather than expose
a dependent frame whose reference was never sent.

BGRA-to-YUV444 conversion now uses the encoded row stride and explicitly pads
the source to the full macroblock dimensions before splitting. Visible widths
that require horizontal padding no longer pack rows into the wrong offsets.
Both backends retain full-range BT.709 and AVC444 v1's chroma mapping. Idle
refresh emits the same paired format as active frames; codec drops preserve
the browser picture while awaiting repair rather than inserting raw updates.

A new connection now retains accepted quality, encoder/manual bitmap, frame
rate, bitrate, cursor, resolution and validation controls rather than discarding
them with the previous protocol state. Pending chroma is latched only at that
connection boundary. A resize or graphics reactivation preserves active chroma;
a reconnect constructs the capture encoder using the newly latched choice.
Regression tests send actual 420/444 graphics PDUs across both transitions and
verify repeated reconnect and control-watch continuity.

## Try it

Install this DMG, then choose **Full color (AVC444)** under Settings → Video
color detail, with a video encoder and a non-Lossless quality preset. Select
AVC444 in Guacamole private.14's Display menu, then fully disconnect/reconnect.
The browser must pass its secure-context WebCodecs/YUV/WebGL support check.
Image fallback and Lossless remain available. Live quality selection from
0.1.18 remains supported; chroma changes still require a client reconnect.

The browser reconstructs both decoded YUV420 views into the complete color
image. It does not rely on a browser-native H.264 4:4:4 decoder. This adds work
and bandwidth relative to AVC420; interactive performance remains for user QA.
Partial regions, single-view refinement and AVC444 v2 use Guacamole's existing
software-decoded image fallback in this first browser implementation.

## Validation

Synthetic fixtures exercise real OpenH264 and VideoToolbox encoding, a shared
H.264 decoder, initial/P/forced-IDR sequences and 320×180, 352×208 and 318×178
visible desktops. One-pixel color patterns verify the reconstructed detail
against source pixels and against ordinary AVC420 subsampling. Wire and display
tests cover strict marker selection, capability limits, continuous small
updates, raw suppression and paired idle repair. Existing shortcut regression
cases also cover the new exact Guacamole client name. Independent review checked
reference repair, padding, client negotiation and ownership.

The complete serial `tools/check.sh` passed: 706 Rust tests, eight Python tests
and eight JavaScript tests, with six existing opt-in tests ignored. CLI,
TypeScript and production frontend builds passed. The focused codec suite
passed 50 tests and the browser wire contract passed six tests. The verified
Desktop delivery is recorded after packaging.
The test package does not install itself or change the running server/display.

## Verified Desktop delivery

Built from corrected source `f28df9c168f3f7d62deb183d73350c4eb51a0982`, with
ad-hoc application signing, and delivered to:

`/Users/beraadim/Desktop/macrdp_0.1.19_aarch64_f28df9c168f3.dmg`

The final image is 15,434,158 bytes; SHA-256:

`f3e35fd52ca73717afd8bf2441232611d5b2247a36dad8805419f079f8ef6709`

The disk-image checksum, read-only mounted bundle version/build identity and
deep strict application signature passed verification. Packaged helper checks
rejected unexpected arguments and invalid configuration before capture or
credential logging. The Desktop image and adjacent checksum/build-info sidecars
were verified against their build outputs. The earlier 0.1.19 testing image is
superseded and archived outside the Desktop; use this reconnect-corrected build.
Installation remains with the user.
