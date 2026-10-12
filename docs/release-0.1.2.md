# macrdp 0.1.2

Release candidate: 2026-10-12. The user installs the DMG and checks Guacamole
and Windows Remote Desktop Connection before accepting this release.

## Changes

- Clipboard images are validated before pixel allocation. Dimensions are limited
  to 8192 per side and 16 million pixels, with bounded encoded/decoded storage.
  Invalid DIB layouts and unsupported compression are rejected. Image conversion
  avoids an unnecessary TIFF-to-PNG intermediate.
- Startup keeps the bound TCP listener and acknowledges success only after TLS
  and server initialization. Start, stop, and settings transitions are serialized;
  failed workers can be restarted, and obsolete callbacks cannot revive their UI.
  Port and TLS changes require a service restart.
- Outgoing audio and clipboard queues have event and byte limits. Slow clients
  disconnect on overflow or a 10-second write timeout. Partial writes cannot be
  reused, and old session callbacks cannot enqueue into a new connection. Stop,
  address queries, and credential changes remain responsive during blocked writes.
  Changing credentials disconnects the authenticated client before applying them.
- Brief idle pauses preserve delta updates and adaptive bitrate history. Healthy
  AVC clients that acknowledge frames no longer receive periodic idle keyframes.
  Clients without acknowledgements retain conservative recovery behavior.
- Bitmap output allocates for the visible rectangle plus protocol overhead,
  reducing temporary allocation, especially for small updates with desktop stride.
  Regression tests compare wire output with the prior allocation strategy.
- Settings show certificate names, expiration, and SHA-256 fingerprint. Custom
  certificate/key pairs are validated and never replaced. Managed certificates
  include hostnames and configured names, renew when expired or names change,
  and atomically publish complete pairs while retaining previous generations.
- A deterministic benchmark workload, bounded metrics recording, and comparison
  tools support repeatable client testing. See [benchmark guide](client-benchmarks.md).
  Sampled server timings are not end-to-end input latency measurements.

## Upgrade and acceptance checks

The existing managed localhost-only certificate may be renewed on first startup
to include the Mac hostname. Its fingerprint will change; compare the fingerprint
shown in Settings before accepting a new client trust prompt. Custom identities
remain intact. Incomplete, expired, future-dated, or mismatched custom pairs fail
startup with a diagnostic instead of being silently replaced.

After installing the Desktop DMG, check both client types:

- Connect, disconnect, and restart repeatedly, including after a failed startup.
  Confirm service status, tray, connection history, and display restoration.
- Copy text and PNG/JPEG images in both directions; transfer files and compare
  their contents. Exercise audio while scrolling and reconnect afterward.
- Change credentials during an active connection. Verify the session disconnects,
  old credentials fail, and new credentials reconnect successfully.
- Pause typing briefly, leave the desktop idle, then resume scrolling or video.
  Check for corruption or stalls, and resize during motion and idle periods.
- Run the benchmark guide with matching client/configuration metadata. Exercise
  throttling, reconnect, and client resize manually as documented. Do not infer
  whole-session speed from the isolated allocation microbenchmark.

Run `tools/check.sh` on macOS for Rust workspace/desktop tests, CLI compilation,
Python and JavaScript harness regressions, and the frontend production build.
The optional release bitmap microbenchmark is documented in the benchmark guide.

## Automated validation

On the Apple Silicon Mac mini, the final pinned-dependency checks passed:
278 workspace Rust tests, 22 desktop Rust tests, 8 Python tests, 3 JavaScript
tests, CLI compilation, and the TypeScript/Vite production build. The isolated
bitmap allocation benchmark is opt-in and is excluded from those test counts.
Live Guacamole/mstsc acceptance and end-to-end performance comparisons remain
for user testing.

The release-mode allocation microbenchmark also passed (20 iterations per
fixture): 1080p output capacity changed from 16,588,800 to 8,297,914 bytes;
720p noise from 7,372,800 to 3,687,964 bytes; and a narrow dirty region from
11,044,096 to 92,216 bytes. Wire-byte equivalence is tested separately. These
figures describe temporary output buffers, not total process memory or RDP latency.
