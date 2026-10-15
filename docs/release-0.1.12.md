# Release 0.1.12: preserve complete surface updates through Guacamole

Surface-codec updates now carry negotiated RDP frame markers around every
region and encoded partition belonging to one capture. Guacamole private.3
honors both boundaries, so delivery gaps no longer force it to infer where a
marked update ends. Classic BitmapUpdate clients retain their existing format;
the Guacamole update adds bounded adaptive grouping for that path.

Markers are sent only when the client advertises FRAME_MARKER support and the
selected codec actually uses Surface Commands. The capture provider reports
whether lazy regions remain, allowing one frame ID to span the complete batch.
Pointer updates may still interleave at complete Fast-Path update boundaries.
Unchanged captures send no empty marker pair. Abandoned batches and resize
close any open marker before later updates; frame IDs wrap safely.

The connection log's `Configured bitmap transport` event now includes
`surface_frame_markers`, making the negotiated behavior visible without
recording pixels or credentials. No runtime configuration migration is needed.

## Validation

`tools/check.sh` passed on the Mac mini: 595 Rust tests, eight Python tests,
eight JavaScript tests, CLI compilation and TypeScript/Vite production build.
Five optional experiments remain ignored by the normal Rust suites.

Seven new wire tests cover every encoded partition and pointer update within
the same frame ID, grouping across lazy capture regions, unchanged updates,
missing capability, classic fallback, abandoned batches and resize/deactivation.
The test build remains ad-hoc signed, as requested. Packaging verifies app
signatures, the disk image, embedded version/revision and Desktop checksum.

The paired Guacamole release is `1.6.1-private.3`. Its synthetic jitter tests
measure frame coherence and bounded scheduling, not end-to-end session speed.
Its dirty-copy optimization and new diagnostics can be used with existing
macrdp versions; the explicit frame-boundary benefit requires this DMG.

## User acceptance

Install the verified Desktop DMG, reconnect Guacamole, and check large screen
changes, scrolling and window movement over the same Wi-Fi connection. Compare
images per update and update-gap percentiles in the optional diagnostics panel.
Those gaps include idle periods and are not a network latency measurement.
Check Windows Remote Desktop Connection as well, including resize, reconnect,
cursor movement during large updates and the lossless bitmap fallback.

The user installs and tests this package; the release workflow does not replace
the running Mac server or change its current settings.
