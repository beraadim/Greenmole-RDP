# Greenmole RDP 0.1.44

The 0.1.43 logs show the initial Windows connection and three reconnects using
RDP6-RLE, 32 bpp, and the single-rectangle layout. All four sessions captured
matching 1920×1200 source/output/content geometry at origin (0,0) and scale 1.
Their 77 source-health observations were non-black, and pixels changed in 45
of 73 comparisons. Capture stopped and display ownership retired before every
new session. There is no evidence that capture cleanup caused the persistent
missing tiles or newly reported lines. Sparse sampling and socket writes do
not prove correct wire coverage or Windows presentation.

| Client attempt | Captures delivered | Display updates written | Bytes written |
| --- | ---: | ---: | ---: |
| Windows initial | 140 | 232 | 9,151,577 |
| Windows reconnect 1 | 442 | 910 | 36,514,208 |
| Windows reconnect 2 | 106 | 305 | 12,027,919 |
| Windows reconnect 3 | 133 | 340 | 14,000,436 |
| Guacamole | 72 | 136 | 12,966,630 |

The final working Guacamole attempt also used RDP6-RLE and the single-rectangle
layout, with Lossless desktop still selected. It resized to 1920×1064, so it
does not provide an identical-resolution comparison. Unlike the earlier AVC
browser trial, this result directly compares two clients receiving the classic
bitmap codec. The existing logs do not identify their alpha/header layout.

The protocol audit found no specific RLE-plane, bottom-up row, Fast-Path
fragmentation or bitmap-length error. A relevant interoperability difference
remains: our encoder always included the optional eight-byte TS_CD_HEADER,
whereas FreeRDP and xrdp normally omit it when the client permits. Microsoft
defines omission as optional support, not a requirement. The included header's
scan-width convention also differs between Microsoft's prose and reference
implementations. This release tests negotiated omission without changing that
legacy field's interpretation for clients that retain the header.

The server now advertises implemented NO_BITMAP_COMPRESSION_HDR support and
reads the client's returned General capability. When supported, classic RDP6
updates use bitmap flags 0x0401 and omit TS_CD_HEADER; otherwise they retain
flags 0x0001 and the existing header. The bitmap-length field follows the actual
layout. RLE encoding, opaque-alpha handling, single-rectangle messages, maximum
strip heights, allocation policy and negotiated receive/working budgets remain
in place. Live image-quality changes preserve the negotiated policy. Other
graphics codecs retain their transport.

This is a standards-valid compatibility trial following established server
behavior, not a demonstrated explanation for the Windows failure. No capture,
input, AVC or display-lifecycle policy change is included.

The bounded activation record and selected diagnostic export now include
client General flags, Bitmap drawing flags, activated width/height, and actual
RDP6 alpha omission and compression-header inclusion. The new values are typed
integers and booleans; pixel data and free-form client strings are excluded.
Schema 8 remains backward compatible with missing fields in older records.

Regression coverage includes literal little-endian flags, lengths and optional
header offsets; identical compressed pixels for the same strip with either
header layout; quality changes that rebuild the codec; and independent complete
1920×1200 reconstruction across header/alpha combinations and three working
budgets. Fast-Path fragmentation, later padded damage and cache checks remain.
Codec tests live in a separate module to keep the production encoder focused.

The user installs the DMG from this Mac mini's Desktop. Keep **Bitmap fallback**
and **Lossless desktop** and test the initial 1920×1200 connection plus three
reconnects. Then compare Guacamole. Native Windows acceptance remains pending;
the new activation records will identify the negotiated layout on each client.
No automatic installation or microphone driver reinstall is needed.

References:

- [MS-RDPBCGR General Capability Set](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/41dc6845-07dc-4af6-bc14-d8281acd4877)
- [MS-RDPBCGR Bitmap Data](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/84a3d4d2-5523-4e49-9a48-33952c559485)
- [FreeRDP bitmap writer](https://github.com/FreeRDP/FreeRDP/blob/master/libfreerdp/core/update.c)
- [xrdp bitmap writer](https://github.com/neutrinolabs/xrdp/blob/devel/libxrdp/libxrdp.c)

All standard checks passed: 1,072 Rust tests, 39 Python tests, 11 JavaScript
tests, native microphone checks and the production frontend build. The seven
existing manual Rust tests remain ignored. Independent review found no blocking
issues in negotiation, fallback, literal wire framing, policy preservation or
diagnostic privacy. Whitespace and added-lines secret-pattern checks passed;
dependency pins remain unchanged.
