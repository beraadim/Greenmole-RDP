# Greenmole RDP 0.1.42

The 0.1.41 diagnostics narrow the Windows drawing failure to a stage after
source capture. The three Bitmap fallback tests all negotiated the RemoteFX
bitmap codec. During the first drawn-but-frozen session, captured pixels changed
by up to 250 of 256 sampled points, and all seven button-down/up pairs reached
native posting calls, along with 463 drag posts. All 52 capture observations
across the three attempts were non-black and had advancing eligible source
timestamps. Source/output/content geometry matched 1920×1200 at origin (0,0),
with scale and contentScale both 1. The first activation resized from 1080 to
1200 rows and still failed.

| User observation | Captures delivered | Bitmap regions written | Bytes written |
| --- | ---: | ---: | ---: |
| Drawn, then frozen | 584 | 1,316 | 58,586,816 |
| Completely black | 30 | 54 | 1,904,942 |
| Brief image, then black; cursor shapes update | 213 | 353 | 6,004,212 |

Input counters reconcile without unexplained dropped posts. The third session
posted its one button pair and 17 drag events. The second received pointer
movement but no button transitions. Startup keyboard callbacks are not evidence
of typed text or failed key injection. Cursor shape capture is independent of
desktop pixel delivery, so working hover shapes do not establish image
presentation. Capture/display cleanup completed normally between attempts.
Sparse samples and successful socket writes still cannot prove that the
correct transmitted regions were decoded and presented.

The output audit found a concrete RemoteFX interoperability bug. The server
wrapped RemoteFX data in **Set Surface Bits** (command 0x0001) and selected the
codec using the SET_SURFACE_BITS capability. Microsoft's RemoteFX sequencing
specification defines RemoteFX data inside **Stream Surface Bits** (command
0x0006), and its prerequisites require STREAM_SURFACE_BITS support. The two
commands have distinct capability flags. FreeRDP's reference sample also uses
Stream Surface Bits for RemoteFX. This requirement applies to the RemoteFX
stream; no separate Set command exception was found for image mode.

This release corrects that carrier and its capability gate while preserving
RemoteFX payload geometry, mode, entropy, codec ID, frame boundaries and update
limits. A client must advertise Stream Surface Bits before RemoteFX is selected.
Other Set Surface Bits codecs retain their own capability requirement, and
classic compressed bitmap output remains the safe fallback. The same selection
policy applies when changing image quality during a connection.

This is a demonstrated protocol correction in the codec used by all three
failed tests. It is not proof that the original freeze has only one cause.
The correction also applies to temporary RemoteFX bitmap output before an AVC
encoder becomes ready; its effect on subsequent AVC presentation requires
native Windows testing. No speculative capture, display-mode, input-target or
GFX channel negotiation changes are included.

A bounded `Configured bitmap transport` record now survives the normal log
filter and selected diagnostic export. It identifies the codec, numeric surface
command (0 for classic bitmap updates, 1 for Set, 6 for Stream), optional RemoteFX
image-mode flag, frame-marker support and negotiated update limit. It contains
no image payloads, input values or free-form client data. Existing schema 8
capture/input diagnostics remain available.

The user deploys the test DMG from this Mac mini's Desktop. Nothing is installed
or restarted during preparation, and the microphone driver payload is unchanged.
Keep **Bitmap fallback** and test three consecutive 1920×1200 connections. Click
and drag a window on each for fifteen seconds, then disconnect and report the
result in order. If Bitmap works consistently, an Auto encoder test can then
check whether its startup fallback was also involved in the earlier AVC issue.

Protocol references:

- [MS-RDPRFX 3.1.8.3.1, Encode Message Sequencing](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/028ed301-385c-4b17-84ec-0a158a22c1b2)
- [MS-RDPRFX 1.5, Prerequisites/Preconditions](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdprfx/516f1a1c-20ec-4ea1-b92a-7995d8a322cf)
- [MS-RDPBCGR 2.2.9.2.2, Stream Surface Bits](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/b2218638-3cf9-4f2f-be61-b096ec3c8dc5)
- [FreeRDP official sample server](https://github.com/FreeRDP/FreeRDP/blob/master/server/Sample/sfreerdp.c)

All standard checks passed: 1,066 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. The 7
existing manual Rust tests remain ignored. Regression tests demonstrate the
old wrong command and Stream-only rejection, then verify literal wire IDs,
compressed fallback pixels, both RemoteFX modes, quality changes, complete
1920×1200 coverage and real Fast-Path fragmentation on fresh connections.
Independent review found no blocking protocol or diagnostic-export issues.
The shared Set/Stream payload layout lives in a separate module. Whitespace
and staged secret checks passed; dependency pins remain unchanged.

The ARM64 DMG was mounted read-only and verified for deep strict application
and driver signatures, version/revision, architecture, licenses, packaged
diagnostic markers, unchanged microphone payload and checksum. The DMG and
checksum/build-info sidecars are on the Desktop. Active app hashes and app,
helper and Core Audio processes were preserved. Nothing was installed or
restarted. See the [package validation record](release-0.1.42-validation.json).
