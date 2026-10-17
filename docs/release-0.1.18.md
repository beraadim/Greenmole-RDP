# Release 0.1.18: manual bitmap selection and live image quality

Settings now includes **Bitmap fallback** in the Encoder menu. Choosing it
explicitly uses classic bitmap updates instead of H.264 video, while retaining
the selected image-quality preset. It does not silently turn every quality
preset into Lossless desktop. The bitmap choice is saved for future starts and
can be changed during a running session.

The Image quality presets also apply during the active connection. Performance,
Balanced and High quality configure the actual video encoder or negotiated
bitmap encoder; Lossless desktop uses exact captured RGB bitmap compression.
Changing these controls does not restart the listener, authentication, capture
worker process or virtual-display owner.

## Applying changes

For video-to-video quality changes, the encoder is rebuilt asynchronously with
the new quantizer policy. The current picture is retained until a fresh,
self-contained keyframe is available. Surface identifiers, ordered updates and
acknowledgments retain their connection state. A bounded rebuild failure can
restore image updates rather than wait indefinitely.

Bitmap or Lossless selection requests a complete current baseline, including on
an idle desktop. Old encoder work remains owned until native retirement; stale
completed output and queued regions cannot overwrite the new mode. Quality
updates reach both the RDP bitmap encoder and the display policy before new
frames use that policy. Repeated identical updates do not repeatedly rebuild
codecs.

RDP capabilities cannot be expanded beyond what the current connection
negotiated. A connection started with Lossless may need a client reconnect to
use video or RemoteFX later. Guacamole's experimental browser AVC420 path also
stays with images after an explicit bitmap/Lossless switch; reconnect the client
to resume browser video. The macrdp service and display can remain running.
Normal Performance/Balanced/High video-quality changes preserve browser AVC420
when the replacement encoder starts successfully.

## Saved and accepted settings

A live Quality update is validated and transported through the supervised
worker control channel. The supervisor preserves accepted quality and encoder
choices for recovery. An image-quality or encoder update error keeps the service available,
retains the saved desired choice and reports it as not yet confirmed, with a
retry action. It does not label queue acceptance as completed native encoder
initialization. Credential-update failures retain their existing fail-closed
behavior.

## Validation and delivery

`RUST_TEST_THREADS=1 CARGO_BUILD_JOBS=4 bash tools/check.sh` passed: 699 Rust
tests, eight Python tests and eight JavaScript tests, with six existing opt-in
tests ignored. CLI, TypeScript and production frontend builds passed.

Focused checks cover 161 native transport tests and 82 display tests. Actual
bitmap wire regressions verify RemoteFX quantization changes, lossless RLE,
return to negotiated RemoteFX, unchanged-content repaint and monotonic frame
markers. Already-produced AVC and uncompressed updates complete despite a newer
image-policy request, preventing a lost-reference race during rapid reversal.
Capture-free transition tests verify idle wakeup, late encoder ownership,
bounded rapid changes and complete-baseline replacement of queued geometry.
Configuration tests cover durable graphics preferences, recovery replay,
confirmation timeouts, failed reverts and retry. A separate review checked
native ownership, capability limits, cancellation and UI feedback.

Existing codec-failure fallback remains latched for its capture/session; toggling
settings does not retry a failed or stuck codec without a fresh connection.
The dashboard identifies explicit bitmap/Lossless output correctly, and legacy
startup encoder aliases remain supported. Test preparation does not install
the app or change the user's running server or display.

## Verified Desktop delivery

Built from committed source `dfb47e26f3dce29f616eb294392717a6a64247fe`
with ad-hoc application signing. Delivered locally to the Mac mini as:

`/Users/beraadim/Desktop/macrdp_0.1.18_aarch64_dfb47e26f3dc.dmg`

The image is 15,432,919 bytes. Its SHA-256 is:

`8456be3a6746ef4e517d2132461b38cf40b627e46336be414d8d438c0daf85a3`

The adjacent `.sha256` and `.build-info.json` files identify the same image and
source revision. Verification passed for the disk-image checksum, a read-only
mount, the bundled version/build identity and the application's deep strict
code signature. Packaged helper checks rejected unexpected arguments and
invalid startup configuration before capture or credential logging. The
Desktop copy and both sidecars were verified against their build outputs.
The user installs this testing build; the existing service was left running.
