# Greenmole RDP 0.1.39

Windows testing found a regression introduced by 0.1.38: the server closed
otherwise working sessions with `Invalid graphics client envelope`. Each of the
three logged attempts had already received acknowledgements for ten decoded
frames. The application and RDP helper remained alive; Windows displayed a
network error after the server terminated the session. This was separate from
the earlier black-screen investigation.

The 0.1.38 validation incorrectly required the first graphics PDU's declared
length to equal the entire channel message, and rejected all messages on
nonzero reserved flags before classifying the command. The logs identify that
guard as the immediate failure but do not identify which condition or command
triggered it. No actual Windows packet shape is inferred from these logs.

The receiver now bounds and classifies individual graphics PDUs. It processes
supported commands in order, validates supported headers and bodies before
changing graphics state, and preserves the existing tolerance for reserved
flags and unsupported optional messages. A well-framed unsupported PDU can be
skipped while processing later commands. If an unsupported message cannot be
safely framed, its remaining bounded channel data is ignored instead of parsing
its body as another header. The supported-command length checks, cache-offer
limits and malformed-wrapper protections remain in place.

This follows the command-first processing order in
[MS-RDPEGFX 3.1.5.1](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/d1db62cf-a9f3-40c7-9503-ebcbdb9d5621).
The upstream [FreeRDP server receiver](https://github.com/FreeRDP/FreeRDP/blob/master/channels/rdpgfx/server/rdpgfx_main.c)
also processes individual PDUs within a channel read rather than requiring a
whole-read length match.

Bounded numeric envelope observations record command ID, reserved flags,
declared/available byte counts and stream offset. They contain no message body
or packet dump and are explicitly selected in sanitized schema-7 exports. The
0.1.38 cache reply correction and startup capture/surface diagnostics are kept.
The original 1920×1200 black-screen cause remains unconfirmed.

Regression tests use independent hand-built wire messages, including frame
acknowledgements, optional QoE traffic, batched commands, cache exchanges and
malformed supported-message boundaries. They do not inject live input or open a
desktop session. The DMG is placed on the current Mac mini Desktop for the user
to deploy. Packaging does not replace or restart the active app, RDP service,
credentials or Core Audio, and the microphone driver is unchanged.

Acceptance should first verify a Windows connection remains open beyond the
first ten decoded frames at 640×480. Then repeat connect/disconnect/reconnect at
1920×1200 to continue the black-screen investigation. Native Windows acceptance
of this replacement remains pending user deployment.

The complete standard checks passed: 1,037 Rust tests, 39 Python tests,
11 JavaScript tests, native microphone tests and the production frontend build.
Seven existing manual Rust tests remain ignored. Independent wire tests
reproduce the old guard's rejection and pass with the replacement. Independent
receive-boundary review, staged whitespace/secret checks and dependency-pin
comparison passed.
