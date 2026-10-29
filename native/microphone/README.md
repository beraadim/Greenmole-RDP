# Remote microphone HAL driver

This input-only AudioServerPlugIn publishes **Greenmole Remote Microphone** with
stable device UID and bundle identifier `com.macrdp.microphone`. It accepts
48,000 Hz stereo interleaved Float32 audio. It does not capture a hardware
microphone, become a default input, expose an output device, use a kernel
extension, or require changes to SIP.

The implementation is original code using Apple's public
[AudioServerPlugIn interface](https://developer.apple.com/documentation/coreaudio/audioserverplugin).
Apple's installed SDK header `CoreAudio/AudioServerPlugIn.h` documents custom
property marshalling, caller process IDs, client registration and IO callbacks.
Custom properties use this supported HAL IPC path; the driver opens no network
socket and creates no shared audio files or shared memory.

## Protocol

Find the device by its UID through Core Audio. The device's global custom
property `gmmv` returns a retained `CFNumberRef` with protocol version **1**.
Release the reference after reading. The global property `gmmf` accepts a
`CFDataRef` through `AudioObjectSetPropertyData`; pass `sizeof(CFDataRef)` and the
address of the reference. The property cannot be read to recover media.

The CFData starts with this 24-byte header, followed by PCM only for opcode 2:

| Offset | Size | Value |
| --- | --- | --- |
| 0 | 4 | ASCII `GMM1` |
| 4 | 4 | Little-endian opcode: 1 begin/refresh, 2 PCM, 3 end |
| 8 | 8 | Little-endian nonzero session generation |
| 16 | 4 | Little-endian frame count, 1–1024 for PCM, zero otherwise |
| 20 | 4 | Reserved; must be zero |
| 24 | count × 8 | Little-endian stereo Float32 samples |

Begin pins the OS-supplied producer PID and its process start time, provided the
producer has the effective/real UID of the active console user. Another process
or generation cannot replace a live producer. PCM and end must match the pin.
Begin/PCM refresh a **250 ms** lease. End revokes the lease immediately; a dead or
stalled producer becomes silent when its lease expires. Lease deadlines use the
system continuous clock, including time spent asleep, so waking cannot revive
pre-sleep media. Start a fresh generation
for every authenticated RDP session, and send end on stop/disconnect. A session
without fresh PCM must not leave a background heartbeat running.

IO callbacks only read atomic, preallocated state. No allocation, locks, process
inspection, property IPC, disk access or logging occurs on the real-time path.
Frames are stamped with their device timeline and internal epoch, so multiple
consumers hear the same quantum and previous sessions cannot replay through a
new session. Missing frames are silent. Scheduling targets 20 ms and rejects
packets that would queue more than 100 ms. Nonfinite samples become zero and
finite samples are clamped to [-1, 1].

Registered consumer processes receive media only when their recorded UID matches
the active producer. Unknown clients receive silence. The device declines default
input roles; select it explicitly in Teams or another app that offers a microphone
selector. Actual Core Audio client
registration, sandbox process inspection, application compatibility and user
switching still require live acceptance testing on each supported macOS release;
this code is not an isolated macOS user session or a hardware privacy boundary.

## Build and installation

`build.sh <resource-directory>` builds a universal arm64/x86_64 driver, signs it
using `MACRDP_SIGNING_IDENTITY` (ad-hoc by default), verifies the signature and
writes `driver.sha256`. `macrdp-ui/build.sh` builds this before compiling the UI,
exports its digest as `MACRDP_MICROPHONE_DIGEST`, and packages the exact signed
bundle under `Resources/microphone` before signing the outer app.

The UI embeds the install/remove script text at compile time and runs that
literal text after the user grants administrator authorization. It never asks a
privileged shell to execute a mutable app-resource script. Installation accepts
only the resource driver path and the compile-time digest. It copies three fixed
files into a private root-owned stage, checks the digest and signature, and then
publishes the fixed destination:

`/Library/Audio/Plug-Ins/HAL/GreenmoleMicrophone.driver`

The installer checks root ownership, symlinks, write permissions and ACLs for
privileged parent directories. The uninstaller only removes that verified driver.
Installing/removing restarts Core Audio and interrupts audio in other apps, so
both actions must be deliberate user operations while the RDP service is stopped.
No driver is installed by the build or the synthetic test suite.

For Developer ID releases the driver is signed first with the same identity as
the app; the existing outer signing/notarization pipeline then includes it. Real
installation, Core Audio discovery and Teams/Guacamole audio routing remain
required release acceptance checks.

## Verification

Run `bash native/microphone/test.sh`. The in-process harness uses synthetic
identities and PCM, without contacting the system HAL. A concurrent writer and three readers stress
ring overwrite/publication across 200,000 frames. The suite checks malformed frame
bounds, property metadata, input-only topology, producer identity/generation
checks, lease/END revocation, foreign-user silence, same-quantum multi-client
reads, stale epoch rejection and nonfinite sample filtering under Undefined
Behavior Sanitizer. `build.sh` separately compiles both CPU architectures with
warnings as errors and verifies the bundled code signature.
