# Input timing diagnostics

Version 0.1.37 adds aggregate input stages to the one-second metrics history and
to the rotating operational log. These measurements help investigate a desktop
that opens but responds slowly, without recording keys, text, pointer positions,
button identities, clipboard contents or client-provided timestamps.

## What is measured

`input_timing.received_events` counts decoded input events. The timestamp is
captured when the server finishes reading their PDU, then retained across
dispatch and resize queues. It is not the time the user generated the input or
the time its first network byte arrived. Legacy activation events whose read
timestamp is unavailable increment `untimed_events` and do not manufacture a
zero-latency sample. QoE and unused protocol events are excluded.

`dispatched_events` counts protocol events delivered to the input handler.
`dispatch_wait` measures the time from the completed server read to handler
dispatch, including retained activation/resize queues and lock waits. `handler`
measures the synchronous handler call. It includes translation, wake handling,
event creation and posting where applicable.

`completed_dispatches`, `dispatch_in_progress` and `current_dispatch_age_ms`
also expose a handler that is still running: an ongoing stall must not remain
invisible merely because its completed-duration sample has not arrived yet.

`native_callbacks`, `blocked_callbacks`, `unavailable_callbacks` and
`injection_failures` distinguish callbacks that ran, private-session protection
that inhibited input, unavailable injectors, and native processing failures.
One pointer PDU can produce several callbacks or posting calls, and some valid
events need no native post. These counts are therefore not expected to match
one-to-one.

`post_attempts` and `post_returns` count actual posting calls and their returns.
`post_errors` counts preparation failures before posting, such as failure to
stamp an event source; the void posting API cannot report acceptance errors.
`native_post` measures posting calls; `read_to_post` measures from the
server read to the last posting return observed in a callback. A CoreGraphics
posting call has no application-consumption acknowledgement: a return proves
only that the API call returned. These numbers do not measure client-to-server
one-way latency, WindowServer processing, or an application's response time.

`last_receive_age_ms`, `last_dispatch_age_ms` and `last_post_age_ms` show how old
the latest observed stages are at the sample time. Missing measurements are
null in exported metrics. Timing percentiles use bounded windows of up to
1,024 actual stage samples, independently of the one-second sampling interval.
Native observations per callback are also bounded, with omitted sample counts
reported separately. Counters saturate rather than wrapping.

Input measurements reset on a real new connection and survive resize
reactivation. Final session snapshots remain in the sanitized diagnostics
history after disconnect. Idle metrics do not repeat old input as live work.
Capture deliveries, display updates sent, and client-reported decoded frames
are included for comparison. Graphics counters follow graphics activation
boundaries; a resize can reset them while the connection's input history remains.

## Reading a reproduction

Connect from Windows, drag a window for around ten seconds, and leave the
session open briefly. Disconnect before returning to chat if necessary. The
app retains 120 metric samples and 20 terminal outcomes until it exits; export
them using **Logs → Export diagnostics**.

- No new received events during attempted movement points toward the client,
  transport or an earlier protocol-reading stage.
- Received events with high dispatch wait identify delay inside server dispatch
  or a retained activation queue.
- Prompt dispatch with long handler or native posting times isolates a later
  server-side input stage. Error, blocked and unavailable counters add context.
- Prompt posting plus continuing capture, output and graphics acknowledgements
  narrows the remaining investigation to host application response or client
  presentation; it does not by itself prove either one is responsible.

The `Input pipeline progress` log records selected aggregate fields once per
second only while a session is active, plus a final record on disconnect. The
existing bounded transition-diagnostics target keeps these available at the
default warning log setting. Flat log fields use `-1` for missing ages/timings;
the structured metric export uses null. No input callback writes a per-event
log record or waits for file I/O.

## Input families and capture comparisons in 0.1.41

The new `keyboard_events`, `mouse_move_events`, `mouse_relative_move_events`,
`mouse_button_down_events`, `mouse_button_up_events` and `mouse_scroll_events`
count decoded native-handler callbacks, including callbacks blocked or unable
to inject. They are not wire event counts: an absolute button packet may expand
into a pointer-position callback and a button transition. Keyboard synchronization
is included in the keyboard family. No button identity, key or position is saved.

`keyboard_posts`, `mouse_move_posts`, `mouse_button_down_posts`,
`mouse_button_up_posts`, `mouse_drag_posts`, `mouse_scroll_posts` and `other_posts`
classify actual native posting calls that returned. These include cleanup
performed inside a timed callback, while cleanup outside a callback remains
excluded. A successful call return still does not prove OS or application
consumption. If button callbacks arrive but no corresponding native posts occur,
blocked/unavailable/failure counts help identify the server-side boundary.

Capture health now samples up to 256 BGR points at most once per second for the
first minute, up to 61 observations per desktop activation. `comparison_available`
and `compared_sample_points` identify an eligible previous sample;
`changed_sample_points` counts changed BGR points, or is `-1` when unavailable.
Up to 256 color triplets live temporarily in private memory only; no sampled
colors, image hashes or coordinates enter diagnostics. NV12 stays unsampled.
Unchanged sparse samples are not proof that every desktop pixel was unchanged.
Timestamp booleans describe progress of source timestamps between observations,
without retaining their values or inferring display latency.
A delayed first frame can produce one baseline after that minute without
reopening the observation window.

`capture_geometry` records source/output/display dimensions and the first
frame's content transform once per native stream. These describe capture
geometry rather than pointer positions. Comparing them across working and
frozen sessions can reveal different capture-to-input transforms that the output
buffer dimensions alone cannot expose. Schema 8 exports select these fields
explicitly and retain at most 400 operational events within the existing byte
limits.
