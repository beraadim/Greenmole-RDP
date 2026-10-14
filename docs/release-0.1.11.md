# macrdp 0.1.11

Release candidate: 2026-10-14. Improves cursor responsiveness, fallback pacing,
RemoteFX compression cost and detection of unreachable RDP peers.

## Cursor responsiveness

Due cursor shape/visibility updates run between queued bitmap regions. At most
one pointer update leads a region before graphics get their turn. Large/full
bitmap updates also poll pointers after each complete encoded partition, never
between its Fast-Path fragments. This keeps RDP reassembly and full-frame diff
state intact while allowing shape/hide changes during a slow screen refresh.
Existing deadlines and shape deduplication still bound polling and pointer traffic.

## Adaptive graphics delivery

Bitmap partitions start with a 128 KiB working budget, capped by the negotiated
complete-update limit. Successful write service estimates target about 50 ms per
partition, between 16 and 512 KiB; a smaller negotiated hard limit always wins.
Slowdown applies immediately, while increases are gradual and spaced apart.
Tiny/instantaneous writes do not inflate the budget. This measures socket service
cost, not end-to-end bandwidth or browser display latency. Pointer updates keep
their independent negotiated size limit. The effective budget is in metrics and
sanitized diagnostics.

Uncompressed GFX fallback now adapts to completed PDU preparation and write costs.
It has a conservative 120 ms minimum interval (roughly eight capture frames per
second), slows further under service/confirmed ACK pressure, and never waits for
an ACK to resume. Additional post-write waiting is bounded at 250 ms except when
the user selected a lower frame rate. Waiting happens before consuming captures,
so newer pixels and damage coalesce in the capture mailbox. Fixed multi-second
payload/geometry suppression was removed: repeated geometry with changed pixels
is delivered. Normal AVC resumes its video controller rate after startup;
existing bitmap FPS pacing remains active.

## RemoteFX compression and transport

RemoteFX compresses each candidate's tiles once, calculates exact serialized
block sizes, then serializes once. Growing the output no longer repeats tile
compression. Tile-component and output scratch caches each retain at most 4 MiB.
Rejected candidates cannot advance video frame indices; complete updates retain
independent headers, exact damage coverage and the negotiated size cap. Quality,
quantization and entropy negotiation are unchanged.

TCP keepalive uses 60 seconds idle, 15 seconds between probes and four retries
on macOS and supported Linux/Windows builds. Optional tuning failures log a
warning without rejecting a usable connection. No socket read timeout was added;
quiet desktops and the separate keyboard/mouse idle policy remain supported.
Native timeouts report `transport_timeout`; typed application output deadlines
report `write_timeout`. Keepalive covers macrdp-to-client/guacd, not the separate
browser tunnel, and cannot resume an already broken TCP session.

## Guacamole log investigation

Read-only inspection found Guacamole web/guacd 1.6.0, no container restarts or
OOM kills, and eighteen `guac_display_remove_layer()` assertion failures in the
collected window. Seven previously reviewed Mac peer resets/TLS closes correlate
with assertion failures immediately beforehand. This matches the reported
[GUACAMOLE-2321](https://issues.apache.org/jira/browse/GUACAMOLE-2321), linked upstream
to [GUACAMOLE-2118](https://issues.apache.org/jira/browse/GUACAMOLE-2118).
A child connection process can fail while the container remains healthy.

On 14 October around 13:22 Oslo time, both Windows and Mac browser tunnels closed,
guacd reported nonresponding users, then macrdp observed peer EOF. Closing the
laptop lid is a plausible explanation, consistent with the user's account;
the logs alone do not distinguish sleep, network loss or proxy closure. At 15:24,
macrdp separately logged its configured thirty-minute input idle timeout.
These are distinct from the guacd assertion failures. No gateway configuration,
container restart or automatic deployment was performed.

## Validation and acceptance

`tools/check.sh` passed on Apple Silicon: 588 Rust tests, eight Python tests,
eight JavaScript tests, CLI compilation and TypeScript/Vite production build.
Five optional experiments were ignored by the normal suite. The dedicated
release-mode compression benchmark also passed, with identical output bytes
and a warmed shared worker pool before both strategies.

One observed run of twenty deterministic noisy images per case gave:

| Image | Simulated prior strategy | Reused encode-once strategy | Tile compressions |
| --- | ---: | ---: | ---: |
| 1 x 257 | 15.063 ms | 3.940 ms | 300 -> 100 |
| 513 x 257 | 21.859 ms | 16.352 ms | 900 -> 900 |

These are synthetic compression timings, not end-to-end FPS or latency claims.
The narrow case required output-size retries; the larger case measures cache
reuse without removing any tile compression.

Native tests use synthetic pixels, private pasteboards, loopback sockets and
fake blocked writers. The wire regression verifies pointer ordering outside
First/Next/Last fragments, bitmap-only service accounting and preserved full-frame
diff state. Tests also cover exact noisy-image partition coverage, both entropy
modes, cache retention limits, changed same-geometry fallback convergence,
native socket options and typed deadline classification.

The test DMG remains ad-hoc signed. Package verification checks the app signature,
DMG integrity, version, committed revision, ARM64 architecture and Desktop copy
checksum. The user installs and tests the package. Check Guacamole and mstsc with
large screen changes, cursor shape/hide changes, quality changes, resize and
reconnect. Laptop sleep may terminate the browser tunnel; reconnect afterwards.
