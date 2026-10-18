# Release 0.1.23: distinguish idle telemetry from active frame work

The dashboard and tray no longer present the last completed frame's encode
latency as current activity after a client disconnects. The server's one-second
metrics heartbeat remains fresh while listening, with an explicit session-active
flag and cleared live capture, encoder, queue and transport measurements when
there is no authenticated client. Cumulative bytes and the fixed disconnect
outcome remain available, and the completed session's final timing summaries
are preserved in diagnostics.

The dashboard shows **Idle · no client connected**, clears its live chart history
and displays dashes for session-only measurements. The connection count remains
zero. A late connection-list poll cannot overwrite a newer disconnect event,
and reconnect waits for active telemetry instead of reviving the idle sample.
The exported diagnostic metrics retain the activity flag for troubleshooting.

This corrects metrics reporting. Native screen capture and video encoding remain
owned by the active session and already stop on disconnect. The application
update does not change capture settings, input handling, display restoration or
server configuration.

## Validation and test package

Focused regression checks cover retained three-millisecond encode measurements,
idle heartbeats, preserved final session timings, reconnect rates, dashboard
history and connection polling races. Full serial release checks passed: 736 Rust tests, six existing opt-in ignored
tests, eight Python tests and twelve JavaScript checks, plus CLI validation,
TypeScript and the production frontend build. Eleven focused metrics tests and
eight UI regression tests passed. Existing compiler and bundle-size warnings
remain. Package verification and Desktop delivery follow the source commit. Live display, input and remote-session actions
are reserved for the user after installation.

Connection setup also initializes graphics before notifying the input handler.
The graphics mutex is released before either callback, avoiding a lock inversion
with coherent live metrics sampling. An actual connection-activation regression
verifies unlocked callbacks, reset counters and timings, context/connection
ordering, peer propagation and unchanged resize-reactivation semantics.
