# Greenmole RDP 0.1.45

The 0.1.44 Windows attempts remained alive but displayed only some regions,
including long thin rectangles. Activating a window covering the desktop
repaired the first picture once, inconsistently. Both connections negotiated
RDP6-RLE, alpha omission and compression-header omission, so the optional-header
trial did not resolve the failure. Their final capture geometry matched
1920×1200 at origin (0,0), with scale/contentScale 1. The first activation
briefly started at 1080 rows before resizing.

| Windows attempt | Captures delivered | Display updates written | Bytes written |
| --- | ---: | ---: | ---: |
| First | 565 | 1,344 | 71,085,381 |
| Second | 287 | 722 | 44,865,421 |

The 90 source-health observations were available and source timestamps advanced
in all 88 comparisons. Pixels changed in 52 comparisons, with up to 179/256
sample points changing. Sparse sampling does not certify the whole image, but
there is no evidence of wholesale black/frozen capture. Input completed native
posting without errors. Capture stopped and display ownership retired normally
before the next connection. Earlier authentication probes do not explain a
successfully activated session's drawing failure.

The repaint audit found a definite contract bug: core requested a full baseline
or recovery image by supplying full geometry with empty damage hints. The wire
encoder interprets empty hints as permission to compare against its cached
source. An unchanged repair could therefore produce no bitmap bytes even after
core cleared its recovery request. This does not establish the cause of the
initial missing image, because a fresh encoder has no framebuffer cache.

`BitmapUpdate.force_refresh` now explicitly requests every pixel in the supplied
region. It takes precedence over hints and source-cache equality while retaining
all validation, compression and size limits. Core sets it for required seeds,
requested repaint counts, stale recovery and cached recovery. Ordinary full
source reads, including unknown/chaotic damage, retain normal diff suppression.
The existing idle-resume repair request becomes effective; no repaint timer or
periodic resend mechanism is added. Crops preserve this local intent; it adds
no field to the RDP wire protocol.

This release also tests smaller complete classic bitmap messages. A classic
RDP6 payload must fit within one Fast-Path packet: at most 16,374 encoded bytes,
or a smaller negotiated/working limit. Codec-aware preflight partitioning bounds
work before encoding, and an exact encoded-size check spatially splits oversized
images. It never cuts compressed bytes into separately presented images. The
single-rectangle layout and negotiated header/alpha policies remain in place.
Generic Fast-Path fragmentation, SurfaceCommands, RemoteFX, GFX and pointer
limits are preserved.

This is a compatibility isolation, not a demonstrated Windows protocol bug.
Microsoft permits fragmented Bitmap updates. Reference classic server paths
commonly use small rectangles; large strips versus small independently received
updates remain a useful difference to test. If this build works, acceptance will
establish the smaller-message policy's value, not which Windows internal stage
previously failed.

Bitmap crop validation now rejects wrapped relative/absolute coordinates,
invalid strides, truncated backing data and arithmetic overflow, returning no
crop instead of panicking. An invalid crop in the wire iterator produces an
explicit error without committing its cache, rather than an empty successful
batch. Valid padded crops, short final rows and zero-copy sharing remain.

Normal-level activation diagnostics record the effective classic payload
ceiling. A separate `Bitmap repaint prepared` event records explicit repaint
intent, dimensions, origin and fixed captured/cached source. At most 16 such
observations are emitted per display activation; ordinary frames consume no
quota. The label means preparation, not confirmed delivery or presentation.
Typed diagnostic export retains these bounded fields without pixels or
free-form client strings. Schema 8 remains compatible with older records.

Regression tests corrupt a synthetic client image after caching a baseline,
verify an ordinary duplicate emits nothing, then require a forced repair with
misleading tiny hints to restore the entire supplied region. Nonzero-origin
partial repair, unchanged suppression afterward, exact coverage and pixel bytes
are also checked. The header/alpha/budget matrix now verifies every classic
update is a single packet. Existing RemoteFX fullscreen tests still require
real fragmentation. Separate tests retain the narrow-column alpha u16 limits,
safe crop boundaries, core intent selection and bounded/private diagnostics.

The user installs the DMG from this Mac mini's Desktop. Keep **Bitmap fallback**
and **Lossless desktop**. Test three consecutive 1920×1200 connections. On each,
observe whether the whole desktop draws before activating a full-screen window,
then drag a window and disconnect. Compare Guacamole afterward. Native Windows
acceptance remains pending; the user deploys the package.

References:

- [MS-RDPBCGR Fast-Path Update](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/a1c4caa8-00ed-45bb-a06e-5177473766d3)
- [FreeRDP classic shadow updates](https://github.com/FreeRDP/FreeRDP/blob/master/server/shadow/shadow_client.c)
- [xrdp classic bitmap budgeting](https://github.com/neutrinolabs/xrdp/blob/devel/libxrdp/libxrdp.c)

All standard checks passed: 1,084 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. The seven
existing manual Rust tests remain ignored. The server example compiles and the
updated bitmap-crop documentation test passes. Independent review found no
blocking issues in forced repair, cache suppression, crops, codec-specific
limits, preserved fragmentation or diagnostic privacy. Whitespace and added-lines
secret-pattern checks passed; dependency pins remain unchanged.

Smaller complete messages can increase encoding-task and message counts; this
release claims no measured client performance improvement. The two 0.1.44
session timing snapshots are retained for later comparison with a comparable
workload, alongside the correctness observations.

The ARM64 DMG was mounted read-only and verified for strict deep application
and driver signatures, version/revision, architecture, licenses, packaged
repaint/payload-limit diagnostics, unchanged microphone payload and checksum.
The verified DMG and checksum/build-info sidecars are on the Desktop. Active
application hashes, GUI/helper processes and Core Audio were preserved. Nothing
was installed or restarted. See the [package validation record](release-0.1.45-validation.json).
