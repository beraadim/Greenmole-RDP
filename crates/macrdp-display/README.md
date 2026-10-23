# macrdp-display

Owns the display selected for a Greenmole RDP server. `Main` pins the display ID that
was primary at startup and only reads its geometry. `Virtual` creates one extended
display in the existing macOS login session when a connection activates and
changes that display's mode as the RDP viewport changes. It becomes the primary
display, giving the remote desktop the menu bar and primary desktop behavior.
Before primary activation, a connection snapshots ordinary movable windows on
the physical displays, including the original main display. Activation gathers
only windows originally on non-main physical displays; macOS handles the original
main display's windows during the primary transition. Disconnect restores the
physical primary and saved display origins and returns captured windows to their
original monitors and frames. By default it then removes the virtual screen and
helper. The opt-in retention constructor instead keeps that exact helper and
native object with the display disabled between sessions. Physical
display modes and persistent display preferences are never changed.

The virtual framebuffer uses one pixel per logical point and is limited to
320×200 through 3840×2160 pixels. Each requested dimension rounds down to a
multiple of eight before RDP negotiation, matching native mode granularity.
The controller checks live dimensions and logical bounds after a mode change;
a mismatch is an error. Physical Retina displays keep their actual backing
pixel mode; virtual HiDPI modes are not offered.

The native implementation uses private `CGVirtualDisplay` classes discovered at
runtime. This is an experimental macOS integration, not a supported Apple SDK
contract. It checks the selectors and their Objective-C ABI encodings before
calling them. The declarations were checked against
[Chromium's macOS virtual-display test integration](https://chromium.googlesource.com/chromium/src/+/HEAD/ui/display/mac/test/virtual_display_util_mac.mm).
The implementation here is independently written Rust.

Virtual-display objects live on the main thread of one persistent helper process.
The helper creates its display before any CoreGraphics display query, avoiding
macOS's stale mode cache. When explicit mode selection is needed, a fresh
short-lived selector reads the current mode list and selects the exact mode.
Its transaction uses login-session scope so the mode survives selector exit;
it does not write permanent display preferences.
It verifies the owner's display ID, vendor, product and serial, and refuses
unowned, built-in or mirrored displays. The owned virtual display can be primary
while its mode changes. The persistent owner services reconfiguration
events while the selector runs. Actual mode pixels and global bounds return over
bounded JSON pipes; the selector has its own native-call deadline.
Resolution changes reuse the connection's process and display. Default reconnects
create a new owner after the previous one has been removed and reaped. Retained
reconnects snapshot the current physical arrangement before enabling the existing
object, and verify the requested mode through a fresh selector. Main-display
queries use the existing worker and never change a physical mode.

Primary layout changes use another fresh selector and the same bounded native
transaction mechanism. The owner retains one snapshot per connection, including
the original primary, and restores it on explicit disconnect, shutdown or EOF.
If a physical display disappears, another remaining physical display becomes
primary; Quartz may adjust origins to resolve gaps or overlap. `queue_remote_capture`
orders activation and capture preparation before a following disconnect even
when the async capture task is cancelled before its blocking waiter starts.

Window snapshots retain Accessibility identities in memory, not window titles.
Snapshot, gather and restore phases each have a three-second budget; minimized,
full-screen, unsupported and unresponsive windows can be skipped. The mover
does not query its own PID through Accessibility. The isolated automatic helper
queries GUI applications out of process from its main thread; the manual action
uses main-thread AppKit for its own GUI windows. Browser resizing never recaptures
the original frames. Restoration ignores closed windows and fits windows from
missing monitors visibly onto surviving physical screens. Applications may
constrain placement or size. With default removal, windows opened after the
snapshot are left to macOS when the virtual screen disappears. Retained mode
performs a bounded recovery pass for remaining ordinary remote windows before
disabling. An incomplete pass declines parking and uses full removal. The manual Dashboard gather action
remains available and does not overwrite the original snapshot.

Cloned controllers carry only channels and values across threads. Dropping the
last controller requests helper shutdown, waits briefly for process exit, then
forces termination if necessary. EOF also ends the helper when its parent exits.
Process exit lets WindowServer remove mode-switched virtual displays that can
outlive release of their Objective-C owner. Reply waits and shutdown are bounded.

`new_with_owner_identity_slot_and_retention` opts into server-lifetime display
ownership. It creates nothing until the first connection; disabled displays
provide no capture geometry. Private enable/disable symbols are checked at
runtime, and only the exact leased virtual display may be toggled. Parking
verifies absence from both public online and active lists. Final retirement
reaps the helper first, then checks all display slots, including disabled ones.
Ordinary constructors retain removal-on-disconnect behavior. No capture or
encoding is kept running by retention.

Executables embedding this crate must call `run_display_helper_if_requested()`
at the start of `main`, before initializing the application or querying displays.
If it returns `Some`, report any error to stderr and exit immediately. The
private `--macrdp-display-helper` argument reserves stdout for the protocol.
Missing hooks are detected to prevent recursive child launches.

Initialization or mode failures never select a different display implicitly.
This protects against accidentally exposing the physical desktop when a
virtual display is unavailable. Creating a virtual display does not create a
separate user session; windows, focus and input belong to the logged-in user.

Portable checks:

```text
cargo test -p macrdp-display --lib
cargo check -p macrdp-display --target aarch64-apple-darwin
```

The cross-target check verifies Rust/Objective-C types without linking. Actual
creation, mode changes, headless operation, window placement and shutdown still
require testing on the supported macOS versions.

An ignored native smoke test creates a temporary extended screen in the current
logged-in macOS session and gathers/restores ordinary application windows.
Run it only when those live desktop changes are intended. It checks dimension alignment, minimum and maximum
sizes, stable display IDs within a connection, primary-display activation through
resizes, exact physical geometry restoration, absence while idle, removal on
disconnect, recreation on reconnect, and final-controller-drop cleanup.
One test keeps mode changes serial, and fresh read-only observer subprocesses
verify actual native pixel modes independently of the creator's display-mode
cache. It also checks that the default main-display
controller does not change the physical display mode. Adding and removing
screens may cause macOS to rearrange windows; keep the display layout stable
while the test runs. The test does not launch a server or inject input.

Run the standard 1:1 viewport test explicitly on the Mac with the display awake.
The bounded wake assertion below is released when the command finishes. Normal
server connection handling already wakes the display before capture preparation.

```text
caffeinate -d -u -t 120 cargo test -p macrdp-display --test native_smoke -- --ignored --nocapture
```

The smoke executable has its own helper entry hook and read-only observer mode.
Its normal invocation skips display creation; `--ignored` explicitly enables
the native test. It verifies every backing mode through a fresh observer process.

Add `--retained` to the explicitly enabled smoke command to check three
disconnect/reconnect cycles at different sizes, preservation of the same display
ID, disabled capture rejection, physical geometry restoration and final owner
retirement. This also changes the live desktop and requires device acceptance;
it is not enabled by ordinary release checks.

The default test run skips the native smoke test. Its Rust types can be checked from
another host with `cargo check -p macrdp-display --tests --target aarch64-apple-darwin`.
