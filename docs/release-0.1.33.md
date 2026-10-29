# Greenmole RDP 0.1.33

This release adds experimental client microphone redirection for Windows Remote
Desktop Connection and Greenmole/Guacamole. A new **Settings → Microphone** page
provides a separate default-off policy and explicit installation/removal of the
bundled **Greenmole Remote Microphone** virtual input device. The driver requires
administrator installation, and installation/removal restarts Core Audio. See
[setup and acceptance testing](microphone.md).

The server negotiates the standard MS-RDPEAI `AUDIO_INPUT` channel, accepts
bounded PCM16 packets at 44.1/48 kHz in mono/stereo, and converts them to a fixed
48 kHz stereo Float32 Core Audio input. Each authenticated connection owns its
queue, resampling history and driver generation. Disconnect, channel close and
backend errors end the source; the driver's independent lease rejects media
from a stalled producer. Stale/congested packets are dropped with counters and
reset handling instead of accumulating conversational delay.

The original input-only HAL plug-in uses Core Audio's supported CFPropertyList
custom-property IPC. It has no network listener, shared audio file, kernel
extension or physical microphone capture. Its real-time callbacks use a
preallocated atomic timeline ring, preserve the same quantum for simultaneous
consumers and silence unknown/foreign-user consumers. The active console user's
UID remains a trust boundary: another process from that user can claim an idle
producer slot. The driver does not become the default audio input.

The application embeds reviewed installer text and the signed payload digest.
The elevated installer stages only fixed files in a private root-owned directory,
verifies the digest before parsing or publishing them, checks the signature and
protects privileged destination paths. Status checks and runtime workers have
admission limits so stalled Core Audio calls cannot accumulate threads through
repeated refreshes or reconnects.

RDP receive guards bound static/dynamic fragmentation before the older IronRDP
reassemblers allocate incoming media. Microphone protocol/consumer failures
quarantine that optional channel. Raw MCS debug logging is replaced by channel
and length metadata so debug logging cannot include incoming microphone bytes.
Health diagnostics contain format/timing/error counters rather than media.

Automated checks cover negotiation/client format ordering, permissions denial,
malformed/fractured packets and receive budgets, per-connection teardown, PCM
conversion/resampling, fragmented capture resume, bounded cancellation and
installation/configuration state. Native synthetic tests check malformed HAL
messages, UID/PID/generation ownership, lease revocation, simultaneous readers,
concurrent ring overwrites and stale media. These checks do not install the
driver, restart Core Audio or capture a real microphone.

Actual HAL registration/sandbox process inspection, Teams audio, browser/native
client interoperability, sleep/wake and fast user switching require target-Mac
acceptance. Capture can continue throughout an enabled RDP session even when no
Mac app is recording. Camera forwarding and Teams VDI media optimization are
outside this release.

The complete check script passed: 952 Rust tests (including 190 protocol-server,
11 microphone bridge and 107 desktop tests), 39 Python tests, 11 JavaScript
tests, the frontend production build and native UBSan/concurrent-ring tests.
Seven existing manual Rust tests remain ignored. Hardware encoder fixtures now
run serially in the check script because their shared retirement budget can be
exhausted by parallel fixtures. Production encoder behavior is unchanged.

The optimized ARM64 DMG was built, verified read-only and copied to the current
Mac mini's Desktop with its checksum and build metadata. The packaged app and
universal driver signatures, version/revision, compiled installer digest and
fixed-file staging signature were verified. The running Desktop app's executable
and metadata hashes remain unchanged. No driver was installed and Core Audio
was not restarted; the user will deploy and test later. See the
[release validation record](release-0.1.33-validation.json) for provenance and
remaining acceptance checks.
