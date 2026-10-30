# Greenmole RDP 0.1.37

The Windows reconnect investigation showed normal capture and display cleanup,
continuous capture, and no sustained graphics acknowledgement or socket backlog.
The available samples could not show whether input was received and posted
promptly. This release adds the missing input timing evidence; it does not claim
to fix an established cause of the reported freeze.

Aggregate diagnostics separate server PDU read, input dispatch, synchronous
handler work and native CoreGraphics posting return. They cover fast-path,
slow-path and Additional Input, retain timestamps through deferred resize input,
and distinguish untimed legacy activation input. Native callbacks report blocked,
unavailable and failed processing separately. Counters and timing windows are
bounded, reset on real reconnects, and survive resize reactivation. Capture,
output and decoded-frame counters provide graphics correlation.

The one-second history and final disconnect metrics are included in sanitized
diagnostics schema 6. Selected aggregate progress also persists in the rotating
log under the existing bounded operational-diagnostics target, without changing
the user's log level. Keys, text, pointer coordinates and other input payloads
are excluded. A posting return is explicitly distinguished from application
consumption or client-to-server latency. See
[input timing diagnostics](input-timing-diagnostics.md) for field semantics and
the next Windows reproduction.

Tests use synthetic timing and posting boundaries without injecting live input.
The build preserves dependency versions and authentication behavior. The
microphone driver is unchanged. The DMG is built and copied to this Mac mini's
Desktop for the user to deploy; the active app, service, credentials and Core
Audio remain untouched.

The complete standard checks passed: 1,001 Rust tests including the patched
acceptor authentication suite, 39 Python tests, 11 JavaScript tests, the frontend
production build and native microphone driver tests. Seven existing manual Rust
tests remain ignored. Focused tests cover queued lock waits, protocol variants,
resize/reconnect boundaries, ongoing dispatches, scoped native posting, absent
injectors, failures, cleanup exclusion, bounded storage and sanitized exports.
An independent integration review found no blocking issues.

The optimized ARM64 DMG was mounted read-only and verified for its application
and driver signatures, version/revision, architecture, bundled licenses, input
diagnostics markers, installer payload and checksum. It and both sidecars are
on the Desktop. Active app/configuration hashes, app/RDP worker processes and
Core Audio were preserved. See the
[package validation record](release-0.1.37-validation.json). The new diagnostics
still need a native Windows reproduction after the user deploys this build.
