# Release 0.1.28: recover slow AVC startup without raw graphics traffic

The first AVC420 session after installing 0.1.27 requested the iPhone's portrait
capture size before encoder initialization. VideoToolbox successfully created
the 1184 × 1808 encoder after 25.9 seconds, but macrdp had discarded the attempt
at its five-second deadline. Pending and failed startup then used uncompressed
GFX updates, explaining the large network load.

The delay was inside `VTCompressionSessionCreate`, before codec properties or
frame preparation. macOS cold-launched its encoder service promptly, followed
by a long gap before native session setup completed. The exact OS blocking cause
is unknown. The logs show successful encoder creation, rather than unsupported
portrait dimensions or a Safari decoder rejection.

## Startup and fallback

Initial encoder startup remains asynchronous, with one global initialization
worker. Five seconds now produces a single slow-start notice; the hard adoption
deadline is thirty seconds. A result completed within that deadline remains
eligible even when the next server poll occurs later. Cancelled or late results
are dropped on their owning startup worker; asynchronous native cleanup
continues blocking admission until retirement finishes. No blocking join or
retry loop is added to the RDP thread.

While accepted AVC is unavailable, Greenmole sessions use the negotiated,
compressed bitmap path with existing damage detection, recovery and bitmap-cost
pacing. Startup failure remains on this image path instead of raw full-desktop
GFX. This temporary transport does not change negotiated capabilities or latch
a permanent video failure. The dashboard reports `bitmap-fallback` while it is
actually carrying images. Explicit raw validation and non-AVC behavior retain
their existing policies.

Greenmole's browser-AVC configuration also advertises RemoteFX, so compressed
fallback for the reported session is a code-supported inference; its exact
bitmap codec was not recorded at the warning log level. A different RDP client
advertising neither RemoteFX nor another supported image codec retains its
existing negotiated surface transport, which can be raw.

When an eligible encoder becomes ready, macrdp preserves the newest pixels,
discards unsent image tiles, resets transport pacing, restores the video frame
rate and forces a complete AVC keyframe/baseline. A pending initializer prepared
for a different negotiated chroma mode is cancelled instead of being adopted.
Changes in image quality or encoder preference continue to cancel their old
initializers through the existing live-control path.

Greenmole accepts image updates before its first video frame. Its existing
five-second live-rebuild hold/cancellation budget is preserved: once browser
video has started, classic image updates select image fallback for that
connection. This release does not change Greenmole or promise that browser video
will resume after an existing active-session resize.

## Diagnostics and preserved behavior

Bounded `macrdp_transition` records distinguish startup begin, slow, ready and
cancelled states and temporary bitmap transport. VideoToolbox create records
include requested configuration; separate property and preparation phases record
duration and outcome. Codec settings and property order are unchanged. No
per-frame logging or image/input contents are added.

Virtual-display creation, promotion, physical layout/window restoration and
removal are unchanged. The current stable monitor identity, physical displays,
wake assertions, capture stop watchdogs and resource retirement ordering are
preserved. Retained virtual display remains off by default. No monitor cleanup
or native service restart is part of this release.

## Validation and device testing

Focused tests cover slow-success adoption, hard deadlines, worker ownership,
negotiated-chroma cancellation, fallback dispatch/recovery/pacing, fresh bitmap
baselines and real initializer-to-keyframe handoff. Full serial release checks
pass: 837 Rust tests, seven opt-in tests ignored, eight Python tests, eleven
JavaScript tests, TypeScript and the production frontend build. The core suite
also passes with three test threads (243 passed, one ignored). Full release and
package results are recorded in
[the validation record](release-0.1.28-validation.json).

An explicit hardware test encoded and decoded generated portrait pixels at
1184 × 1808, verified a self-contained IDR and exact decoded dimensions, and
retired two sequential encoder instances. Startup took 1190 ms then 2 ms. This
test neither captured the desktop nor changed displays or permissions; it does
not reproduce or eliminate the original macOS cold-service delay.

Quit the old application before installing the Desktop DMG. Re-enable AVC420
and start a fresh iPhone Safari connection with browser AVC420 decoding enabled.
During a slow initial startup, the desktop should remain usable with compressed
images and then change to AVC automatically. Check network use, portrait resize,
disconnect/window restoration and physical cursor responsiveness. A true native
failure or startup longer than thirty seconds should remain on images. The
installer is supplied for user testing; the running application is not replaced
automatically.

## Verified package

Test package: `macrdp_0.1.28_aarch64_ac0350c852bd.dmg`, delivered on the Mac mini
Desktop with SHA-256 and build metadata sidecars.

- Source: `ac0350c852bd5c8d25d2ac2b77a8c3ec14333dd1`.
- SHA-256: `5e7d6511128322956c10feab27de4fb26f516cc1681a1cdb98f9f675a6c7ced8`.
- Size: 15,674,923 bytes; aarch64, ad-hoc signed.
- Image checksum, read-only mounted application deep strict signature, actual
  binary architecture, version and embedded source metadata passed. The
  verification mount was removed.
- Packaged helper extra-argument rejection and standalone CLI version checks
  passed; no valid helper/recovery command or remote session was invoked.
- Desktop copies were written atomically and verified before removing only the
  previous 0.1.27 DMG and its two sidecars. The installed app and configuration
  were preserved for the user to replace and test.

The package verifies the implementation and generated-pixel codec path. Actual
AVC420 browser handoff and network use on the iPhone remain device acceptance
checks; this release does not claim to eliminate the OS service's native delay.
