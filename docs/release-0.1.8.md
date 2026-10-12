# macrdp 0.1.8

Release candidate: 2026-10-12. This release adds image-quality controls for both
video-capable clients and the bitmap fallback shown in the reported screenshot.

## Image quality

Settings → RDP Service now offers four presets. Changes are saved immediately
and apply after restarting the service and reconnecting the client.

| Preset | Video | RemoteFX / bitmap |
| --- | --- | --- |
| Performance | QP 20–40, low encoding complexity | Smaller RemoteFX updates |
| Balanced | QP 16–34, medium complexity | Standard RemoteFX quantization |
| High quality | QP 12–28, high complexity | Lowest legal RemoteFX quantization |
| Lossless desktop | Bypasses H.264 | Full-color RGB-plane RLE; bypasses RemoteFX |

Balanced is the default for new and migrated desktop preferences. The CLI keeps
its existing high_quality default. VideoToolbox must accept the selected
quantizer bounds; unsupported settings use the existing software fallback.
Higher quality requires more bandwidth and encoding time and may reduce FPS.
Lossless refers to preservation of the captured RGB pixels by server compression.
Capture scaling, client display scaling and a browser gateway remain separate.

For the reported dark-background noise, try Lossless desktop first. With
Guacamole, also enable its [force-lossless connection option](https://guacamole.apache.org/doc/gug/configuring-guacamole.html#display-settings)
to prevent independent lossy compression between the gateway and browser.
Compare at native resolution and 100% client/browser zoom.

## Implementation and diagnosis

- Fixed a RemoteFX entropy-encoder defect in the pinned dependency: RLGR1 used
  the wrong zero-symbol adaptation constant, desynchronizing sparse coefficients
  and corrupting gradients/edges. The bounded local writer also corrects odd
  RLGR3 padding. Both modes now reconstruct the quantized coefficients exactly;
  visual error decreases as quality increases in the decoded reference fixture.
- Image quality applies to the actual negotiated bitmap encoder as well as video.
  Bitmap fallback can use lossy RemoteFX; a large H.264 bitrate has no effect on it.
  The dashboard and sanitized diagnostics now identify RemoteFX versus lossless
  RGB bitmap encoding and report the client's preferred color depth separately.
  Inactive video targets are no longer shown as applicable to bitmap traffic.
- Lossless policy removes RemoteFX from advertised codec capabilities, disables
  the GFX video channel and skips video encoder creation. Display resize and
  other connection channels retain their normal behavior. Pixels remain in the
  existing 32bpp server format, without palette reduction.
- RemoteFX uses bounded scratch storage sized to the actual RLGR bitstream, so
  higher-detail or noisy tiles cannot overflow the previous component estimate.
  Initial-frame header retry remains valid when output needs a larger buffer.
- Classic bitmap updates honor the client's alpha-skipping capability. Clients
  that cannot omit alpha receive an explicit opaque plane with a matching
  stream header. Chunk bounds cover both compressed and raw 16-bit wire lengths,
  including narrow/tall updates.
- CoreGraphics recovery images are normalized into explicit BGRA8/sRGB pixels.
  Variable source channel order, row padding or component depth can no longer be
  mistaken for the normal ScreenCaptureKit pixel layout.
- Presets are validated, persisted and included in saved/running diagnostic
  configuration. Restart notices remain visible until applied or reverted.

## Acceptance checks

- Select Lossless desktop, stop/start the service, reconnect Guacamole and mstsc,
  and check dark gradients, fine text, colored edges and static desktop content.
  The dashboard must report full-color lossless RGB bitmap encoding.
- With Guacamole, compare with force-lossless enabled and scaling at 100%.
- Compare Balanced and High quality after restarting/reconnecting. Check image
  detail and throughput while scrolling, dragging windows and displaying images.
  For video-capable clients, use Full color (AVC444) when supported if fine colored
  text needs more detail than Standard (AVC420) can preserve.
- Resize, type, play audio and copy files in Lossless mode, then reconnect.
  Confirm that image quality remains selected and session behavior remains valid.

## Automated validation

Validation uses `tools/check.sh` on the Apple Silicon Mac mini, including native
codec configuration tests, decoded RemoteFX fidelity comparisons and exact RGB
bitmap round trips. Capture tests use synthetic images, without reading the
user's desktop. End-to-end visual acceptance remains with the user.

All checks passed: 476 workspace Rust tests, 38 desktop backend Rust tests,
eight Python tests and eight JavaScript tests (530 total). Four optional
microbenchmarks remain excluded. The CLI and TypeScript/Vite production builds
passed. The deterministic RemoteFX fixture's squared RGB error fell from
773,062 at Balanced to 109,305 at High quality (86% lower) in both entropy modes;
these are fixture results, not a claim about every application or screen.
