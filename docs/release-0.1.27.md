# Release 0.1.27: display transition diagnostics

Record the sequence and duration of display creation, mode application,
primary-display promotion, physical layout restoration and virtual-display
retirement. Capture records identify stop requests, native stream shutdown and
release of the display owner. Encoder records identify queued retirement,
VideoToolbox invalidation and final resource release. Process, owner and
operation identifiers connect overlapping phases without logging window titles,
input events or image contents.

The bounded `macrdp_transition` INFO target remains available at the usual
warning log level, including after a live log-level update. Explicit global
logging-off and explicit target directives are respected. No per-frame or
adaptive-frame-rate logging is added. Existing bounded JSONL logging and the
server diagnostic bridge carry these records.

Isolated display helpers forward small, validated lifecycle records through
stderr without changing their command protocol. Parent topology observations
run on one diagnostic worker with an eight-entry queue, no idle polling and no
shutdown join. Records distinguish exact phase timestamps from delayed
observations and label CoreGraphics mode data as cached. A blocked diagnostic
query cannot delay a real display transition or grow additional workers.

Capture, encoder and display resource ordering, watchdog deadlines and timeout
policies remain unchanged. A regression assertion checks that timing stream
destruction does not restart the existing native-stop watchdog deadline.

## Existing behavior and user testing

Use the existing virtual-display settings to compare consecutive sessions.
**Reuse virtual display between sessions remains off by default.** This release
does not enable retention, change monitor identity, alter physical display
modes, or change window placement/restoration policy. Quit the old application
before replacing it with the test package. The installer is delivered for the
user to install; the running application is not replaced automatically.

After connecting, resizing and disconnecting, lifecycle records should show
paired phases and outcomes. Check cursor responsiveness on the physical
screens, window restoration, absence of a remote screen while disconnected,
and normal idle sleep. Diagnostic observations may follow their triggering
phase; use the recorded request/query times rather than assuming their topology
was sampled synchronously before the operation.

## Separate authorized maintenance on the test Mac

The historical monitor cleanup is a one-time maintenance action, not automatic
application behavior. It archives and unregisters 165 verified obsolete macrdp
identities through Apple's ColorSync API, preserving the current stable monitor
identity and three physical registrations. Original registry bytes, ICC files,
metadata, identity files and display preferences have root-private backups.
A supported unregister/register rollback rehearsal succeeded before the batch.

A strict preservation guard initially stopped the batch when macOS regenerated
a physical ICC file. The removed entries were restored. Comparison established
that only its ICC creation timestamp changed: every other byte, including all
color tags and hardware identity, was identical. The completed cleanup accepts
only that narrow timestamp refresh and records it; physical color data,
identity and arrangement are preserved. No WindowServer preference history was
deleted, no OS cache was manually rewritten, and no service restart was used.

This removes historical lookup cost; it does not establish that the recurring
WindowServer/ColorSync work or cursor regression is fixed. ColorSync load was
already variable before maintenance. Consecutive-session testing with the same
display behavior remains necessary to assess the result.

## Validation

Full serial checks passed: 825 Rust tests, six existing opt-in tests ignored,
eight Python tests and eleven JavaScript tests. The display library passes
93 tests, including bounded stderr transport and blocked-observer queue tests.
TypeScript and the production frontend build passed. The final explicit logging
directive adjustment also passed focused filter tests. Existing unrelated
compiler and frontend bundle warnings remain.

Independent reviews covered cleanup identity proofs/rollback safeguards,
display diagnostic transport and worker bounds, capture/codec lifetime ordering,
and preservation of watchdog deadlines. Package verification and Desktop
delivery are recorded against the source commit after the build. No live remote
session or display create/remove experiment was started by release checks.

## Verified package and maintenance observations

Test package: `macrdp_0.1.27_aarch64_e33e728c864f.dmg` on the Mac mini Desktop.

- Source: `e33e728c864f55ac040b0f56ddf84c1af837ce14`.
- SHA-256: `1b01aa34f1dc8dc1803d31f74e2ba45a895e031792a8278f495b175d0743edcb`.
- Size: 15,673,721 bytes; aarch64, ad-hoc signed.
- Image checksum, read-only mounted application deep strict signature, version,
  architecture and embedded source metadata passed. The mount was removed.
- Packaged helper argument rejection and standalone CLI version checks passed;
  no valid helper, recovery command or remote session was invoked.
- Desktop installer and sidecars were copied atomically and verified; the older
  0.1.26 installer and its two sidecars were removed. The installed app was kept.

The completed cleanup leaves four registrations and four ICC files, including
one macrdp profile. The ColorSync registry decreased from 453,983 to 5,027 bytes.
Three preserved profiles remain byte-identical to the initial backup; one
physical profile has only the OS-refreshed creation timestamp described above.
Independent checks confirm the current serial, all four profile color/identity
contents and the physical topology. A saved restore tool passes its dry run;
the earlier supported API rollback rehearsal also passed.

Settled idle measurements show zero ColorSync CPU over ten seconds and zero
display-information requests in the following nominal two-minute log window.
WindowServer has zero color-restoration waits in 2,367 main-thread samples,
compared with 580 of 2,411 samples before maintenance. Native service PIDs stayed
the same. This demonstrates that the observed workload cleared; notifications
from cleanup may also refresh transient OS state, so registry size alone is not
isolated as the cause. Physical cursor acceptance and durability across further
remote sessions remain user tests.

[The validation record](release-0.1.27-validation.json) contains package checks
and the limited maintenance observations without publishing host-state backups.
