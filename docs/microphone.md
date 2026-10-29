# Client microphone redirection

Greenmole RDP can receive the connecting computer's microphone through the
standard RDP `AUDIO_INPUT` channel and present it to Mac applications as
**Greenmole Remote Microphone**. This experimental feature requires macOS 14 or
later and is disabled by default. It is independent of the existing Mac sound
forwarding setting. Camera redirection is a separate feature.

## Install and enable

1. Install the complete Greenmole RDP application from the DMG and open it.
2. Stop the RDP service and wait for cleanup to finish.
3. Open **Settings → Microphone** and choose **Install microphone driver**.
   Approve the native administrator prompt. Installation restarts Core Audio,
   which briefly interrupts audio in other applications. Reopen applications
   that were using audio. If the device remains unavailable, restart the Mac.
4. Turn on **Client microphone redirection**, save, and start the service.
5. Enable microphone forwarding in the connecting client and make a fresh
   connection. Client/browser/OS permission is required as well.
6. In a Mac recorder or Teams device settings, explicitly select
   **Greenmole Remote Microphone**. The driver does not replace the Mac's default
   input device. Allow the recording application microphone access if macOS
   requests it; Greenmole does not need to record the Mac's physical microphone.

For the standalone server, configure the installed driver through the app and
add this to its protected TOML configuration before starting it:

```toml
[microphone]
enabled = true
```

Changes to this policy take effect after restarting the service. Existing
configuration files keep redirection disabled. The installed driver is silent
without an active redirected microphone; merely installing it does not start
client capture. Use **Remove driver** while the service is stopped to uninstall
it; this also restarts Core Audio.

## Client setup

**Windows Remote Desktop Connection (`mstsc`):** Open **Local Resources → Remote
audio → Settings**, choose **Record from this computer**, and reconnect. The
equivalent `.rdp` property is `audiocapturemode:i:1`. For two-way calls, keep
remote sound playback enabled on the client too.

**Greenmole / Apache Guacamole:** Enable the connection's **Audio input** option
(`enable-audio-input=true`). Use HTTPS and grant microphone access in the browser
and client operating system. Existing Greenmole browser and gateway versions
already contain the microphone transport. A denied browser permission or an
unsupported client does not fall back to the Mac's physical microphone.

A consenting client can stream its microphone for the **entire enabled RDP
connection**, including when no Mac application is recording. Turn off
microphone access in the client or disconnect to stop capture immediately.
Turning off the saved server setting requires a service restart. A Teams mute
button controls that application's use of audio; it should not be treated as a
guarantee that the remote desktop client has stopped capturing.

## Acceptance testing

The automated tests use synthetic audio and protocol fixtures. Actual Core
Audio registration, sandbox identity inspection and Teams interoperability need
testing on the target Mac.

1. Verify the driver reports available, then select it in a Mac recorder and
   speak into the **client's** microphone. Verify the Mac's physical microphone
   is not the selected input.
2. Test Windows RDP and Greenmole separately. For Greenmole, test permission
   denial and a later allowed connection as well as a closed browser tab.
3. Join a Teams call, select the virtual microphone, and use headphones first.
   Test mute/unmute, client microphone changes and a sustained call. Then test
   speakers for echo. Adjust the configured idle timeout for long periods
   without keyboard/pointer activity if required.
4. Disconnect while speaking. The virtual input must become silent and must not
   replay old audio after reconnect. Test network loss, service Stop, a capture
   pause/resume and sleep/wake as separate cases.
5. Open two recording applications simultaneously and verify they receive the
   same current audio. Test fast user switching: another macOS account must not
   receive redirected samples. The implementation rejects unknown identities;
   silence on this test may indicate an identity lookup incompatibility.
6. Repeat normal desktop sound playback, connection resizing and reconnect
   tests with microphone forwarding both enabled and disabled.

Teams processes the call on the Mac. This implementation does not provide
Microsoft's provider-specific Teams VDI media optimization, and does not promise
the same latency or echo behavior as a local call.

## Diagnostics and trust boundary

If a driver installed by 0.1.33 remains unavailable, update to 0.1.34 or later,
stop the RDP service when convenient and choose **Reinstall driver**. Version
0.1.33 left the root-owned executable at `0700`, preventing Core Audio's separate
service account from reading it. The updated installer fixes this; a status
refresh alone does not change permissions. Reinstallation restarts Core Audio.

Look for `Client microphone` messages in the existing application logs. They
report negotiated sample rate/channel count, capture denial, driver startup or
delivery failure, bounded queue drops, stale packets and discontinuities. PCM
bytes are never included in these messages. The RDP transport also records
channel IDs and message lengths instead of raw MCS payloads.

The first protocol implementation accepts PCM16 at 44.1 or 48 kHz, mono or
stereo. Conversion runs on a bounded worker and feeds the fixed 48 kHz stereo
Float32 HAL device. Dropped or stale data resets the sample history instead of
building a growing playback delay. Driver END revokes media immediately; an
independent 250 ms producer lease makes a stalled source silent. Applications
may still have their own previously consumed audio buffered.

The driver authorizes the active console user's producer PID/start time and
permits consumers from that UID. Another process belonging to that same user
can claim an idle producer slot. This is a shared macOS desktop feature, not
per-application or isolated RDP-user access control. Client media is not written
to disk or exposed by readable driver properties. See the
[native driver design](../native/microphone/README.md) for the installation and
real-time safety model.
