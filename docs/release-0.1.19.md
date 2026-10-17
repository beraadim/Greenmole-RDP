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

The complete serial `tools/check.sh` passed: 704 Rust tests, eight Python tests
and eight JavaScript tests, with six existing opt-in tests ignored. CLI,
TypeScript and production frontend builds passed. The focused codec suite
passed 50 tests and the browser wire contract passed four tests. The verified
Desktop delivery is recorded after packaging.
The test package does not install itself or change the running server/display.

## Verified Desktop delivery

Built from source `a07a4cec48f1e5ea31ec7a552166ec5096485d2c`, with ad-hoc
application signing, and delivered to:

`/Users/beraadim/Desktop/macrdp_0.1.19_aarch64_a07a4cec48f1.dmg`

The image is 15,432,569 bytes; SHA-256:

`2cee5744bdb3fda7ac52567eda59f3a8b3e2888eaaef7219ba9bd31f70335b51`

The disk-image checksum, read-only mounted bundle version/build identity and
deep strict application signature passed verification. Packaged helper checks
rejected unexpected arguments and invalid configuration before capture or
credential logging. The Desktop copy and adjacent checksum/build-info sidecars
were verified against their build outputs. Installation remains with the user.
