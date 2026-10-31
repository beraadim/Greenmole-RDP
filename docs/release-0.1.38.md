# Greenmole RDP 0.1.38

Windows acceptance exposed a rapid-disconnect regression in this package's
graphics envelope validation. Use [0.1.39](release-0.1.39.md), which corrects that
regression while retaining the cache reply and startup diagnostics below.

Windows testing of 0.1.37 exposed a black reconnect at 1920×1200 even though
capture, input posting, socket delivery and client frame acknowledgements kept
advancing. The preceding successful session resized from 1920×1080 to 1920×1200.
Changing to 640×480 worked, and reconnecting again at 640×480 also worked. This
does not establish a general failure on same-size reconnects or a capture cleanup
failure. The black session received 458 captures and sent 139 display updates;
Windows reported 138 decoded frames. An acknowledgement does not prove that
the expected desktop was visibly presented.

This release corrects a definite graphics protocol omission found during the
investigation: valid persistent bitmap cache offers now receive an empty import
reply. Importing the offered entries is optional; replying is required by
[MS-RDPEGFX 3.2.5.16](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/a635dd5a-1334-43b7-9869-8590c33578b2).
The server does not use client cache entries, and does not store or log their
keys or metadata. The bounded parser rejects invalid envelopes and oversized
counts before processing them. The logical DVC receive limit increases by six
bytes to admit the specification's largest valid offer; fixed channel slots and
the separate microphone limits remain bounded. This omission is not yet proven
to cause the reported black screen.

The receive boundary also rejects truncated graphics headers and malformed
legacy uncompressed ZGFX envelopes before the pinned decoder can panic. Tests
exercise hostile lengths and segment boundaries without a native session.

Bounded startup diagnostics supply the missing evidence for the next test:

- Capture health samples at most 256 BGRA pixels, once per second during the
  first five seconds of each desktop activation. Logs contain only aggregate
  luma range/mean and near-black sample counts. No pixels, images, coordinates
  or image hashes are saved. Opaque NV12 frames are explicitly unsampled.
- Graphics startup records prepared surface reset/create/map dimensions and
  generations, first AVC keyframe/SPS/PPS/IDR presence, and preceding bitmap/raw
  dispatch counts. These records describe prepared commands; existing delivery
  and acknowledgement counters show subsequent progress.
- Sanitized diagnostic exports use schema 7 and explicitly select only numeric,
  boolean and fixed-enumeration startup fields. Existing input diagnostics remain
  available without changing the configured log level.

Native display configuration and authentication behavior are unchanged. The
microphone driver is unchanged and does not need reinstalling. Tests use
synthetic images and protocol data without posting live input or creating a
remote desktop session. The DMG is built for the user to deploy; the running
Desktop app, service, credentials and Core Audio are not replaced or restarted.

For acceptance, connect at the original 1920×1200 resolution, disconnect and
reconnect at that same resolution. If the picture is black, leave it connected
for at least five seconds before disconnecting. The persisted startup records
will distinguish sampled black source content from a non-black source and show
the graphics/cache initialization sequence. The Windows result remains pending
until the user deploys this build.

The standard checks passed: workspace, patched acceptor and UI Rust tests,
native microphone tests, 39 Python tests, 11 JavaScript tests and the frontend
production build. The final graphics receive-boundary regressions were followed
by a complete server-library run (215 passed, three existing manual tests
ignored). Across the final suites, 1,025 distinct Rust tests passed; seven
existing manual Rust tests remain ignored. Staged whitespace/secret checks and
an independent review of the protocol, sampling and export boundaries passed.

The optimized ARM64 DMG was mounted read-only and verified for deep strict
application/driver signatures, version/revision, actual architecture, bundled
licenses, startup/input diagnostic markers, the unchanged microphone payload,
and checksum. The DMG and checksum/build-info sidecars are on the Desktop.
Active application/configuration hashes, RDP worker and Core Audio processes
were preserved. See the [package validation record](release-0.1.38-validation.json).
