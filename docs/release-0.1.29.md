# Release 0.1.29: graphics profiles and session acceptance diagnostics

This release adds saved graphics preferences, a readable session summary and an
explicitly opt-in repeated-session acceptance tool. It preserves the existing
connect/disconnect policy: virtual-display creation, promotion, window movement,
restoration and removal follow the same paths and ordering as 0.1.28. The stable
monitor identity, retained-display default, guarded-session policy and native
cleanup timeouts are unchanged. No Greenmole changes are included.

## Graphics profiles

Settings offers three starting points. None is selected automatically, and
installing this release does not replace existing graphics preferences.

| Profile | Frame rate | Video bitrate | Quality | Encoder | Color detail |
| --- | ---: | ---: | --- | --- | --- |
| Mobile | 30 fps | 8 Mbps | Balanced | Auto | AVC420 |
| Desktop | 60 fps | 32 Mbps | High quality | Auto | AVC420 |
| Limited bandwidth | 15 fps | 2 Mbps | Low latency | Auto | AVC420 |

A profile saves the five related settings together and sends one validated
graphics update to the existing worker. Live controls apply without restarting
the server. Color-detail negotiation still requires a fresh client connection.
A browser already using image fallback also needs reconnecting to resume video.
Bitrate controls video; image updates retain their existing compression policy.

Manual edits remain available and mark the selection as custom settings. Save
up to sixteen custom profiles, update one by saving its existing name, or delete
it without changing current graphics settings. Custom names and values are
validated before persistence. Passwords continue using the existing Keychain
storage; profiles contain graphics settings only.

If the worker does not acknowledge an update, the saved target remains available
for retry and the UI reports that confirmation is pending. Retry sends the
current saved group, including any later manual overrides. It does not restart
the connection or silently roll back a potentially applied update.

## Session summary

Dashboard and Statistics show the current phase and recent session outcomes,
including the host transport and encoder last observed. This describes the
macrdp side; it does not prove that a browser has presented a video frame.
Disconnect reasons are fixed categories such as peer closure, timeout and
server shutdown, rather than raw protocol errors.

Cleanup is reported separately for capture, physical-display restoration,
native encoder ownership, wake assertions and private-session protection.
Disconnect alone cannot produce a verified result. Pending work, missing proof
and failure remain visible; late completions are correlated with their original
operation and session. Retained and removed virtual displays are distinguished.
The summary is bounded and kept in memory; restarting the app clears it.

Overall cleanup confirmation currently targets virtual-display sessions. Sharing
the physical main display has no virtual-display disposition marker, so its
individual cleanup evidence can be shown while the overall result remains
unconfirmed. Exceeding the bounded observation capacity also prevents an overall
confirmation.

Worker diagnostic-queue loss is explicitly reported outside the droppable log
budget. It revokes cleanup confirmation for the observed service generation,
including its recent and later sessions, until a new service generation starts.
File-log loss also invalidates the file-based acceptance tool's proof.

New fixed transition markers observe existing session, wake and guard
boundaries. They do not add recovery actions, native API calls or per-frame
logging, and contain no keyboard content, frame contents or credentials.

## Repeated-session acceptance

The tool documented in [session-soak.md](session-soak.md) compares each idle
snapshot with the original baseline. It checks monitor registrations and stable
identity, physical topology, wake assertions, process ownership/memory and
observed capture/encoder retirement. ColorSync process observations provide
additional context. Missing probes or lifecycle evidence remain unknown.

The tool requires explicit confirmation that the server is idle. Automated
cycles additionally require a user-supplied local client driver. It never stores
client credentials, starts the server, changes display settings, cleans monitor
registrations or restarts services. The release checks use synthetic drivers
and events; they do not open actual remote sessions.

## Testing the DMG

Quit the old application, install the new Desktop DMG and launch it normally.
Existing configuration is preserved. Check these points during normal use:

1. Select a graphics profile while connected, make a manual adjustment and save
   it under a custom name. Confirm that live controls update without a server
   restart and reconnect only when changing negotiated color detail.
2. Connect and disconnect using the same display settings as before. Check the
   session summary, physical window/display restoration and local cursor
   responsiveness. Allow asynchronous native retirement to finish; missing
   evidence should remain unconfirmed rather than be shown as success.
3. For repeated-session acceptance, follow the opt-in tool instructions with a
   separately configured test client. Review unknown results as well as failures.

The application is supplied for user installation and device testing. Build
checks do not establish long-session performance or real client compatibility.
Package details and completed checks are recorded in
[the validation record](release-0.1.29-validation.json).

## Release checks

866 Rust tests passed, with seven opt-in tests left ignored. The 39 Python tests
include 31 synthetic soak checks; eleven JavaScript tests, TypeScript, the
frontend production build and standalone CLI build also passed. Mocked React
checks exercise profile operations and expanded cleanup states, including the
minimum application window size. No real RDP session or installed app launch
was used for these checks.

## Verified package

Test package: `macrdp_0.1.29_aarch64_9af2c4dcbced.dmg`, delivered on the Mac mini Desktop with
SHA-256 and build metadata sidecars.

- Source: `9af2c4dcbced9f1fee08bc34cc88429b352ad732`.
- SHA-256: `24ea2a0aea327940eaa3db989584b1e6b5f740bca0583826a15c858ddf463809`.
- Size: 15,784,701 bytes; aarch64, ad-hoc signed.
- Image checksum, mounted application deep strict signature, binary architecture,
  version and embedded source metadata passed. The read-only mount was removed.
- Packaged helper/recovery extra-argument rejection and standalone CLI checks
  passed without launching the app or running valid helper/recovery actions.
- Desktop copies were verified before removing the previous 0.1.28 installer
  and its two matching sidecars. The installed application metadata/binary and
  saved configuration hashes remained unchanged.

The implementation is committed and the installer is ready for user installation
and real-client acceptance. No actual RDP connection, physical-display mutation,
monitor/profile cleanup or native-service restart was performed during release
verification.
