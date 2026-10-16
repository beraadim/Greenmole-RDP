# Release 0.1.13: Command shortcuts and cursor memory cleanup

## Keyboard shortcuts

Ctrl+C, Ctrl+V and Ctrl+Enter now send Cmd+C, Cmd+V and Cmd+Enter to macOS.
Both Ctrl keys work with the Norwegian Windows layout and Guacamole. Numpad
Enter retains its keypad identity, and Shift is preserved. Other Ctrl shortcuts,
AltGr text entry and existing Alt/Windows modifier mappings remain unchanged.

The requested shortcuts use physical macOS keycodes with explicit Command
flags from an isolated event source. No synthetic Command modifier is held.
Each shortcut remembers its original mapping through repeats and key release,
including Ctrl-first release and context changes. Synchronization, disconnect
and injector cleanup release outstanding shortcuts. Failed posts do not fall
back to typing text. A shortcut cancels any pending dead-key accent.

Keyboard dispatch and shortcut translation now live in separate modules to
keep the connection handler manageable. The input diagnostics show the last
shortcut translation, and debug logging records the shortcut name and Shift
state without recording clipboard contents.

## Failure investigation on 2026-10-16

At 14:43:04 Europe/Oslo, the macOS kernel killed `macrdp-ui` under memory
pressure, reporting `134563 MB` for its compressed process memory. The live
replacement process's allocation summary already contained thousands of
`NSCursor`, `NSImage`, `NSBitmapImageRep`, `CGImage` and `CGDataProvider` objects.

Cursor polling on the long-lived Rust worker had no local autorelease pool.
AppKit creates temporary cursor images even when the cursor has not changed;
Rust wrapper ownership alone does not drain those internal temporary objects.
Each poll now drains its own pool. The cached CGImage remains explicitly
retained, and returned cursor pixels are owned Rust data.

A read-only native regression reproduces the problem without starting a server,
posting input, or modifying the desktop. After 64 warmup polls, 1,024 more polls
retained **617,458,240 bytes** before the fix and **2,560 bytes** after it. The
opt-in test asserts less than 16 MiB growth while its worker is still alive;
waiting until thread exit would hide this defect. It reads allocation totals,
not process-memory contents. Run it alone:

```sh
CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/tools/run-native-test.sh" \
  cargo test -p macrdp-capture --lib --locked \
  repeated_cursor_polling_keeps_native_allocations_bounded \
  -- --ignored --nocapture --test-threads=1
```

At 14:44:21, a separate connection ended with `too much input while capture is
starting`; the application process continued running. Capture startup used to
disconnect once 128 input packets had accumulated. It now pauses socket reads
at that limit and lets TCP apply backpressure, preserving all buffered input in
wire order. Pre-resize, handshake and activation input share one bounded replay
queue, capped at 128 packets and 1 MiB with space reserved for a complete wire
packet. Shutdown, credential changes, idle expiry and the fixed activation
deadline remain responsive. When the queue is full, peer closure is observed
after activation resumes or the deadline ends the wait.

An informational log records buffer occupancy when reads pause. Application
startup now records the process ID, version and build revision to correlate
future failures with system logs. No user input or clipboard payload is added
to these diagnostic messages.

These findings establish one operating-system termination and one startup
disconnect in the reported period. They do not establish a second independent
application crash. The memory test verifies the isolated leak fix; a long RDP
session with the new package still needs user acceptance testing.

## Validation

`tools/check.sh` passed on the Apple Silicon Mac mini: 606 Rust tests, eight
Python tests, eight JavaScript tests, CLI compilation and TypeScript/Vite
production build. Six optional experiments are excluded from the normal Rust
suites; the new native cursor memory regression was also run explicitly and
passed after reproducing the pre-fix failure.

Shortcut tests cover both Ctrl keys, both clients, keypad Enter, Shift, repeats,
release order, AltGr, unrelated combinations, dead-key cleanup, lifecycle resets,
post failures and exact native event flags without posting keyboard events.
Startup tests cover a saturated shared queue, ordered replay, packet-size
headroom and responsive shutdown, credential revocation, idle and deadlines.

## User acceptance

Install the Desktop DMG, reconnect from the Norwegian Windows keyboard, and
check copy, paste and Cmd+Enter actions with both Ctrl keys. Check AltGr symbols,
Shift variants, holding Ctrl across copy then paste, reconnect, and typing after
releasing Ctrl before the letter key. Exercise both Windows Remote Desktop
Connection and Guacamole.

Leave a session connected and confirm memory remains stable while the cursor
changes shape. Move the mouse during connection and resize to verify startup
bursts no longer disconnect. The user installs and tests this package; packaging
does not replace or restart the running server.

Native ownership follows Apple's guidance on
[autorelease pools in long-lived background threads](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/MemoryMgmt/Articles/mmAutoreleasePools.html).

## Delivered artifact

Built on the Mac mini on 2026-10-16 from source commit
`2f998c31909c0598502ba457fa648974a89027b0`, with ad-hoc signing. The verified
ARM64 package, checksum and build metadata are on `beraadim`'s Desktop:

`/Users/beraadim/Desktop/macrdp_0.1.13_aarch64_2f998c31909c.dmg`

SHA-256: `6d0cc4324716343ba29ef71dd9917d2aeb148589b095680f0371bbfe0d351b00`.

Packaging verified the application signature and disk-image checksum. A
read-only mount independently confirmed the embedded version, source revision,
ARM64 executable and application signature. All three Desktop files were
compared with their build outputs. The running application was not replaced or
restarted.
