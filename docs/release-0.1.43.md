# Greenmole RDP 0.1.43

The 0.1.42 test at 1920×1200 advanced some tiles while leaving most of the
desktop black. Its activation records identify classic RDP6-RLE bitmap output,
not RemoteFX. The server delivered 2,591 captures and wrote 38,260,360 bytes in
1,962 display regions. The 56 capture-health observations sampled non-black
source pixels, and all six button pairs reached native posting calls. These
observations establish continuing source and transport activity, not successful
Windows decoding or presentation.

The follow-up with **Bitmap fallback → Lossless desktop** still had missing
areas. That setting disables GFX channel negotiation on a new connection while
retaining RDP6-RLE. In that attempt the server delivered 1,100 captures and wrote
100,713,192 bytes in 3,104 regions. Its eight button pairs reached native posting
calls without errors. This result points away from GFX negotiation alone.

The bitmap audit found a close interoperability match in FreeRDP issue #10413:
Windows Remote Desktop drew only a few squares when one BitmapUpdate contained
multiple rectangles. FreeRDP PR #10455 addressed it by sending one BitmapUpdate
per rectangle. The issue reports that later Windows updates repaired that client
bug; the matching symptom does not prove that this client's failure has the
same cause. The user reports Windows 11 25H2, build 26200.9711, so
acceptance must be checked on that client.

This release applies that conservative message layout to RDP6-RLE. The lazy
encoder partitions a region at the existing compression-header row limit before
encoding, and each complete logical BitmapUpdate contains exactly one rectangle.
Ordinary Fast-Path fragmentation still applies to a large individual rectangle.
The bounded working budget, negotiated receive limit, bottom-up row order,
compression headers, RLE planes, opaque-alpha handling and framebuffer commit
ordering remain in place. Each strip now has its own encoding task and write
boundary, adding scheduler and protocol-header overhead on large baselines. No arbitrary encoded fragments are presented as
complete images. Other codecs retain their existing transport.

The normal activation log and selected diagnostic export include a typed boolean
`bitmap_single_rectangle_updates`. This identifies the compatibility policy
without logging pixels or free-form client data. Diagnostic schema 8 remains
compatible with earlier records.

The independent RDP6 test receiver reconstructs a complete 1920×1200 desktop,
then a nonzero-origin damage patch, at 64, 128 and 512 KiB working budgets with
both alpha policies. It checks exact coverage, opaque output from unused source
alpha, long RLE runs, signed and wrapping row deltas, real Fast-Path reassembly,
and unchanged-frame suppression. A server dispatch test also covers pointer
updates between complete bitmap messages while a bitmap batch remains pending
and GFX capabilities have been confirmed. The one-rectangle assertions fail on
the previous production implementation. These tests do not capture the screen,
inject input or connect to the live service.

The user installs the test package from this Mac mini's Desktop. Keep **Encoder:
Bitmap fallback** and **Image quality: Lossless desktop** for the next comparison.
Test three consecutive 1920×1200 connections, dragging a window on each before
disconnecting. Report whether the whole screen draws and continues updating on
each connection. Native Windows acceptance remains pending.

References:

- [FreeRDP issue #10413](https://github.com/FreeRDP/FreeRDP/issues/10413)
- [FreeRDP PR #10455: one rectangle per BitmapUpdate](https://github.com/FreeRDP/FreeRDP/pull/10455)
- [MS-RDPBCGR Bitmap Data](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/84a3d4d2-5523-4e49-9a48-33952c559485)

All standard checks passed: 1,069 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. The seven
existing manual Rust tests remain ignored. The lossless-quality and narrow-alpha
regressions also verify the new layout through the production iterator, including
exact coverage and the alpha-dependent compressed row limit. Independent review
found no blocking correctness or test-coverage issues. Whitespace and added-lines
secret-pattern checks passed, and dependency pins remain unchanged.

The same-machine Guacamole comparison on 0.1.42 drew correctly using AVC video
after startup. Its 83-second session delivered 4,432 captures and wrote
14,031,048 bytes. This is a useful reference for further performance comparison;
it does not establish a regression or end-to-end network/browser latency. No
capture, AVC or GFX policy change is included in this release.

The browser comparison overlapped local compilation/tests and used a gateway
and Wi-Fi route. Its server encode p95 was 9.26 ms, write p95 0.174 ms and
capture-queue p95 0.138 ms. The bounded ACK-history count is not an exact client
backlog; its latency estimate stopped updating after timestamp eviction, so it
cannot identify current network RTT. An idle-host comparison is needed before
attributing the user's possible slowdown to either a regression or the network.

The ARM64 DMG was mounted read-only and verified for strict deep application
and driver signatures, version/revision, architecture, licenses, packaged
compatibility and diagnostic markers, unchanged microphone payload and checksum.
The verified DMG and checksum/build-info sidecars are on the Desktop. Active
application hashes, app/helper processes and Core Audio were preserved. Nothing
was installed or restarted. See the [package validation record](release-0.1.43-validation.json).
