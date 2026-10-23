# Repeated-session acceptance test

`tools/session_soak.py` observes repeated normal connections and disconnects on
the Mac. It is an explicit development/support tool, never an app startup task.
It does not change the server's connection, capture, display, privacy or recovery
policy. Running it with `--help` performs no observations or connections.

There are two modes:

- **Manual:** you connect and disconnect using your normal client; the tool
  observes fresh session boundaries and cleanup.
- **Automated:** an explicitly provided, user-owned local executable controls
  your existing client. The tool requests serial `connect` and `disconnect`
  actions and verifies each cycle from the server's structured lifecycle logs.
  A driver exiting successfully does not prove a connection or cleanup worked.

The tool itself does not start/restart macrdp, lock the screen, inject input,
change networking, invoke recovery, remove monitor registrations/profiles,
restart ColorSync, change physical displays, or request administrator access.
Native probes use read-only CoreGraphics/ColorSync queries, `ps`, and
`pmset -g assertions`. No real sessions are exercised by the automated unit tests.

## Prepare the Mac after installing the test build

1. Start the installed macrdp normally. Use a build with the fixed
   `macrdp_transition` session, wake and private-guard markers (0.1.29 or later).
   Keep your existing connection/display/privacy settings.
2. Make one normal warm-up connection and disconnect. Wait until the dashboard
   reports idle/cleanup complete. This avoids treating first-use native service
   activation or an initially created display identity as gradual accumulation.
3. Leave only one macrdp server running and no client connected. Keep physical
   display connections, arrangement, resolution and the selected graphics mode
   unchanged for the test. Avoid unrelated profile/system updates during it.
4. Find the root macrdp UI/server process PID in Activity Monitor. The root process
   must be named `macrdp`, `macrdp-ui` or `macrdp-server`; its descendants are
   included in the process/RSS observations. Do not select a transient worker.
5. Run the observer on this Mac as the same user, with a fresh report filename.
   Reports are created exclusively with mode `0600`; existing files are never
   replaced. The parent output directory must already exist.

The normal JSONL log is
`~/Library/Application Support/macrdp/macrdp-logs.jsonl`; use `--log` if yours is
elsewhere. Only complete records appended after the observer starts are used.
These forced transition records do not require verbose input/frame logging.
Rotation, truncation, malformed records or oversized records invalidate proof
for an affected cycle instead of silently accepting incomplete evidence.

## Manual cycles

Replace `12345` with the root PID, and run from the repository root:

```sh
python3 tools/session_soak.py --manual --confirm-idle --pid 12345 \
  --cycles 10 --encoder-mode hardware \
  --output "$HOME/Desktop/macrdp-session-soak.jsonl"
```

Wait for `Cycle 1: waiting for a new authenticated connection`, connect normally,
use the session briefly, then disconnect normally. Wait for the verified cycle
result and the next connection prompt before reconnecting. Do not stop the server
between cycles. The default connection timeout is 60 seconds, the manual active
session limit is 300 seconds, and cleanup has 45 seconds to settle.

Use `--encoder-mode bitmap` when testing explicit bitmap mode, or `software` for
a software encoder. The default `hardware` requires observed native encoder
retirement as well as capture, display, wake and private-guard cleanup. This
selector declares the test expectation; it does not change the configured codec.
Use `--backend main` only when deliberately testing capture of the physical main
display; this omits the persisted virtual-monitor-identity check.

## Automated cycles with an existing client driver

The repository does not ship a general-purpose authenticated RDP connector.
Client automation is explicitly user configured. Point `--driver` at an absolute
path to your own regular executable, owned by your user and not writable by group
or other users. A symlink is rejected. The tool invokes exactly:

```text
/absolute/path/to/my-cycle-driver connect 1
/absolute/path/to/my-cycle-driver disconnect 1
/absolute/path/to/my-cycle-driver connect 2
/absolute/path/to/my-cycle-driver disconnect 2
...
```

The two arguments are a fixed action and a positive cycle index. The driver must
use an already configured normal client and keep credentials/configuration
outside this harness. Do not place passwords, connection URLs or tokens in
driver arguments, report names, source files, or submitted reports. The harness
does not accept credentials, arbitrary connection metadata or extra driver
arguments. It never saves the driver path, command line, environment, stdout or
stderr. Your executable can use a private client configuration of its own.

Driver requirements:

- `connect` opens only the test connection, returns zero promptly when its request
  is issued, and lets the established client remain alive for `disconnect`.
- `disconnect` closes only that driver-owned connection, is safe to request if
  connection establishment failed, and returns zero promptly. It must not kill
  macrdp, alter displays/settings, lock/restart the Mac, or close other sessions.
- Both actions run without interactive input and finish within `--action-timeout`
  (30 seconds by default). Stdout and stderr are discarded, not captured.
- Abnormal action cleanup can terminate only the subprocess group created for
  that action. Do not daemonize/escape the group on failure; a deliberately
  detached client is the driver's responsibility to close on `disconnect`.
  A successful connect may leave its client running for the later action.

```sh
python3 tools/session_soak.py --driver "$HOME/bin/my-cycle-driver" \
  --confirm-idle --pid 12345 --cycles 20 --active-seconds 5 \
  --encoder-mode hardware \
  --output "$HOME/Desktop/macrdp-session-soak-automated.jsonl"
```

This requests a new connection, waits for authenticated connection evidence,
keeps it active for five seconds, requests disconnect, and waits for verified
cleanup plus settled idle resources before proceeding. It stops at the first
failed or unconfirmed cycle. There is no built-in automated sleep, resize,
screen-lock or input workload; configure separate, deliberate hardware tests
when those transitions need validation.

On interruption or failure before requesting disconnect, the tool makes one
bounded best-effort `disconnect` request through that same driver. Failed or
timed-out actions have their owned subprocess group terminated without killing
unrelated processes. An already attempted but failed disconnect remains
`unconfirmed` in the result. A successful disconnect request still needs server
cleanup evidence; it is never reported as verified cleanup just from exit zero.
In manual mode the tool performs no disconnect action; close your own client if
you interrupt the observer.

## What the report establishes

The report contains only bounded, allowlisted observations: counts, resource
sizes, boolean lifecycle evidence, fixed reason codes, and SHA-256 fingerprints.
It contains no raw server log records, peer addresses, user/application/window
names, monitor serials/UUIDs, typed content, file paths, URLs or client settings.
Each run is capped at 1,000 cycles, 24 hours of observations and 8 MiB of JSONL,
with bounded native/command/driver timeouts. A final disconnect can use its
additional action timeout (at most 120 seconds). The defaults are ten cycles
and one hour.

Every cycle is compared with the **original idle baseline**, including:

- ColorSync display-device registration count, profile-entry count, and a
  fingerprint of registered display identities. The enumeration covers display
  devices in the user ColorSync registry; it does not delete obsolete entries.
- The persisted macrdp virtual-monitor identity for `--port` (3389 by default).
  Missing/unreadable identity is unconfirmed; the tool never creates one.
- Physical display count/topology, including arrangement, main-display status,
  dimensions and rotation, reduced to a fingerprint before reporting.
- Online macrdp virtual-display count, compared with the idle baseline. A retained
  disabled display is supported; the test does not require zero by assumption.
- macrdp's named wake assertion count, which must return to zero.
- Root process generation, process-tree count, root/tree RSS, and process liveness.
  RSS growth has a configurable allowance (64 MiB by default) against the first
  baseline, so a gradual increase cannot be hidden by resetting the baseline.
- Matched capture/restore operations, successful native encoder retirements for
  every observed native owner, wake release and private-guard completion (or an
  observed guard-not-required admission). A disconnect marker alone is not proof.

The tool requires two successful idle observations separated by the settle
interval. Bounded probes run primarily at baseline and cleanup, not continuously
during active playback. ColorSync process count and `ps` CPU values are included
as informational snapshots; they are not a precise per-cycle CPU benchmark.
Optional missing data stays missing and produces **unknown**, never a fabricated
zero. Failed native encoder creation without enough ownership/retirement evidence
is unconfirmed; it is not automatically labelled a leaked session.

Exit status is `0` for all requested cycles verified, `1` for an observed failure
or driver failure, `2` for incomplete/unavailable evidence, and `3` for user
interruption. The final JSONL record has the result and verified cycle count.
Monitor/profile/process differences and RSS threshold breaches are acceptance
signals to investigate, not proof by themselves that macrdp leaked a resource.
The observer cannot measure physical cursor smoothness or prove that a remote
image was presented; check both during the hardware test and correlate with the
report and existing transition diagnostics.

## Automated synthetic verification

```sh
python3 -m unittest discover -s tools -p 'test_session_soak.py' -v
```

The suite simulates repeated connections, cleanup settling, missing/foreign/stale
evidence, failed restoration, multiple encoder owners, crashes, interruption,
original-baseline resource growth, log loss, secret filtering and private report
limits. Tiny temporary local executables verify the action protocol and abnormal
owned-group cleanup, including a descendant that ignores TERM after its leader
exits. These tests never run an RDP client, macrdp app, native display mutation,
power assertion, recovery action or real network connection.
