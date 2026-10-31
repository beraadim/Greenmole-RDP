# Greenmole RDP 0.1.40

The 0.1.39 receive correction kept Windows sessions connected. Subsequent tests
still showed a black 1920×1200 fullscreen reconnect, with partial drawing and
stale window outlines after leaving fullscreen. During that attempt the remote
framebuffer stayed at 1920×1200; capture delivered 1,640 frames, the server wrote
424 updates and Windows acknowledged 423 logical GFX frames. Capture samples
were non-black, and input posting continued promptly. Logical frame
acknowledgements do not establish that AVC pictures were visibly presented.

The investigation found an inconsistent Windows startup path. Fresh-frame
dispatch already prepares a complete RGB surface baseline before AVC. When the
asynchronous encoder became ready, however, cached-frame recovery tried an AVC
IDR first and cleared the pending seed. The normal Windows baseline branch was
therefore bypassed. The recorded sessions all created their initial surface
with AVC and zero preceding uncompressed GFX dispatches.

This release makes cached startup honor the same complete RGB baseline policy
as fresh-frame dispatch. The first video picture remains an independent IDR;
steady video recovery and the explicit browser video path retain their existing
behavior. A missing or unreadable source image cannot be treated as a prepared
RGB seed. Existing surface and first-AVC diagnostics show whether the baseline
was prepared and how many raw dispatches preceded video.

Initial RGB preparation is tracked separately from a pending video keyframe.
Once hardware capture has an RGB baseline, waiting for a new NV12 video frame
must not repeatedly switch capture back to BGRA on cursor or pacing wakeups.

The change corrects a demonstrated server policy inconsistency. Its effect on
the reported Windows black/stale picture still requires native testing. It
does not claim that a client acknowledgement proves presentation or that the
original cause has been conclusively established.

A 1920×1200 RGB baseline contains 9,216,000 pixel bytes before framing. It is a
one-time startup cost before normal AVC video. Large-frame tests verify the
actual ZGFX, dynamic/static channel and transport fragmentation, surface bounds,
pixel data and subsequent video ordering without capturing a screen or posting
input. The existing ten-second message write deadline remains in force; slow
links can take longer to transfer this baseline than an AVC-only startup.

The running app, service, credentials, microphone driver and Core Audio are not
replaced or restarted while preparing the test package. The user deploys the
DMG from this Mac mini's Desktop. Test the first 1920×1200 connection, then
disconnect and reconnect at that same resolution. Check both the complete
initial desktop and continued repainting while dragging a window. If the
picture is still stale, leave connected briefly and disconnect before replying;
startup diagnostics and final frame counters are retained.

All standard checks passed: 1,047 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. Seven
existing manual Rust tests remain ignored. Independent review covered cached
and fresh dispatch, failed seed preparation, first video IDR, browser/bitmap
paths, repeated NV12 wakeups and large-frame transport bounds. Staged
whitespace/secret checks and dependency-pin comparison passed.
