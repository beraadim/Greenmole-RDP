# macrdp 0.1.1 release notes and acceptance checks

Release candidate: 2026-10-09. Native automated checks and a built DMG do not
establish live Guacamole or Windows client behavior. The user installs the DMG
and performs the acceptance checks below.

## Changes

- The CLI and desktop app now use the same server implementation, including
  display, input, capture, diagnostics, and session cleanup.
- Desktop passwords migrate from legacy TOML into macOS Keychain. New passwords
  use the OS random source. Missing/empty credentials and unavailable credential
  storage fail closed. Explicit CLI credential files remain supported with
  owner-only permissions; unattended startup requires configured credentials.
  Passwords are excluded from saved UI preferences and diagnostic formatting.
- TLS keys and private preferences are written atomically with restrictive
  permissions. Captured configuration and diagnostic files are no longer tracked.
  **If a password from the historical captured configuration is still used,
  rotate it. Removing the file from the current tree does not erase Git history.**
- Negotiation, TLS, CredSSP, and activation have fixed 15-second stage deadlines.
  Stop and credential updates remain responsive during negotiation. The configured
  idle timeout tracks keyboard/mouse inactivity; zero disables it.
- Clipboard file policy is enforced. Outgoing reads are bounded; incoming files
  have per-transfer and per-session quotas, private storage, and protected names.
  Transfer assembly handles reordered, repeated, and short responses.
- Capture favors the newest raw frame when it falls behind. Encoded H.264
  completion handling preserves reference-frame order and requests recovery when
  frames are lost. Bitmap fallback correctly encodes odd widths and partial
  final row groups.
- Graphics acknowledgement handling respects suspended acknowledgements and
  bounds tracking state. Metrics report delivered graphics updates per second
  (bitmap regions can count separately) and wire throughput, capture
  and completion queue delays, encoding time, and socket write time. Graphics
  acknowledgement latency includes transport and client decoding; network-only
  ping latency is not measured.
- Logs append across restarts, rotate within a bounded retention budget, and use
  asynchronous JSONL writes. Connection history preserves measured duration,
  timestamps, and traffic in an atomic database transaction. Invalid numeric
  settings are rejected; listener address and idle timeout changes require restart.
- Live bitrate changes now reach the encoder and adaptive controller. Encoder
  rate limits are checked before native conversions and arithmetic.
- About shows the package version and commit revision and links to Releases.
  Packaging verifies signatures and DMG integrity and writes checksum/build metadata.
  Native test and release executables use the system Swift concurrency runtime.

These changes target responsiveness and correctness. No claim of matching Windows
performance is made without comparable client, resolution, workload, and network measurements.

## Package and automated verification

Build on macOS using the repository's release workflow or `make release`. Record
the commit, native test results, DMG filename, and SHA-256 with the release.
Place the verified DMG on the Desktop of `beraadim` on the Mac mini. Do not install
it or replace the running application automatically; the user performs installation.

On 2026-10-09, `tools/check.sh` passed on the Apple Silicon Mac mini: 247 workspace
tests and 17 desktop tests, the CLI build, and the TypeScript/Vite production build.
Live client acceptance and comparative performance measurements remain manual.

Regression suites cover authentication/control deadlines, acknowledgement state,
capture and encoding queues, bitmap row groups, clipboard limits and reassembly,
Keychain migration/rollback, configuration validation, log retention/JSON, and
connection-history transactions. Run the UI tests as well as the workspace tests.
Use [display testing](virtual-display.md) for the opt-in native display smoke test.

## Manual acceptance: Guacamole and Windows Remote Desktop Connection

Repeat the applicable checks with **both clients**. Record client and guacd/FreeRDP
versions, macOS version, display backend, dimensions, codec path, and network.

- [ ] Install the Desktop DMG, launch 0.1.1, and verify Screen Recording and
  Accessibility permissions. Existing settings remain intact. Legacy credentials
  still connect after Keychain migration; the saved UI TOML contains no password.
- [ ] Correct credentials connect. Incorrect or empty credentials are rejected.
  Change the password, disconnect, and verify the old password no longer works.
- [ ] Reconnect repeatedly and stop/restart the server. There is no stale active
  connection, stuck input, extra virtual display, or incorrect physical layout.
- [ ] Exercise typing, Norwegian characters, modifiers, dragging, buttons, wheel,
  audio, and clipboard text/images. Transfer an empty file, several files, a file
  larger than 64 KiB, and the same filename twice; compare transferred contents.
  With `clipboard.file_transfer = false` in CLI configuration, text still works
  and file transfer is unavailable.
- [ ] Use automatic resolution and resize repeatedly while idle and while
  scrolling. Guacamole must use **Resize method: Display update** with width and
  height unset; see [resize troubleshooting](guacamole-troubleshooting.md).
  In mstsc, also reconnect at another display size. Check pointer alignment and
  virtual-display restoration using [mouse testing](mouse-coordinate-testing.md).
- [ ] Compare idle desktop, text scrolling, window dragging, and video on the
  same LAN/WAN conditions and resolution. Watch delivered updates/s, throughput, queue
  times, encode/write times, and ACK latency. Record codec fallback and visible
  freezes, corruption, or lag; compare full-frame rates only for the same codec path.
- [ ] Set a short idle timeout, restart, and verify disconnect after no input.
  Input extends the session; graphics/audio traffic alone does not. Restore the
  intended timeout afterward.
- [ ] Restart the application and confirm prior logs and connection history
  remain available, durations are nonzero for real sessions, and traffic totals
  match the session. Share relevant diagnostics after reviewing them for private
  hostnames, addresses, filenames, or other session information.
