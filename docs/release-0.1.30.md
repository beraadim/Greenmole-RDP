# Greenmole RDP 0.1.30

macrdp is now **Greenmole RDP**, the macOS RDP server companion to the Greenmole
browser client. Windows Remote Desktop and the existing RDP transports continue
using the same implementation.

The application bundle, window titles, navigation, About page, menu-bar controls,
diagnostic export name, CLI help/version label and DMG use the new brand. The
existing Greenmole mole artwork supplies the application, menu-bar and frontend
icons. [Artwork provenance and licenses](../branding/README.md) are included in
the app alongside the existing GPLv3 license. Stale MIT labels are corrected to
match the repository's GPLv3 license; third-party licenses are unchanged.

Chinese comments, help, configuration examples, README text and historical design
notes are translated into English. Unicode clipboard fixtures retain exactly the
same test data through escapes, preserving non-ASCII coverage. The English README
is now the main README; the previous English filename remains a compatibility
link. Historical release evidence is preserved.

## Upgrade compatibility

The bundle identifier stays `com.macrdp.app`. Existing configuration, Keychain
credentials, TLS files, database/log paths, monitor identity and its legacy
`macRDP Remote Display` descriptor are unchanged. Internal crate names, the
`macrdp-server` executable, helper flags, diagnostic targets and wake-assertion
labels retain their technical names. No data migration or new default profile
is introduced.

The login-item name remains `macrdp`, avoiding a duplicate LaunchAgent. On a
normal launch from a stable installed bundle, an already-enabled saved startup
preference and existing registration allow its executable path to be refreshed.
Missing registrations and saved opt-outs are not enabled implicitly. Development
builds, mounted volumes and translocated/temporary apps cannot perform this
automatic refresh. The Settings toggle remains available for explicit changes.

Display creation, promotion, window movement/restoration, capture/encoder
ownership, guarded-session policy and connect/disconnect behavior are preserved.
The Greenmole browser client and Oberon deployment are not changed.

## Install and test

Quit the old macrdp application, including its menu-bar process. Copy
**Greenmole RDP.app** from the new Desktop DMG to a stable installation location,
then launch that installed copy. Remove the old app manually after confirming the
upgrade; only previous installer files are removed during DMG delivery.

Confirm the new name and mole icons, saved graphics and connection settings,
login-startup preference, a normal connection/disconnect and physical window
restoration. The application is built and delivered for user installation;
release checks do not launch it or exercise real RDP sessions.

Completed checks and package details are recorded in
[the validation record](release-0.1.30-validation.json).

## Release checks

875 Rust tests passed in serial mode, with seven opt-in tests ignored. All
39 Python tests, eleven JavaScript tests, TypeScript and the production frontend
build passed. Nine pure startup-identity checks cover preservation and migration
guards; seven mocked UI checks cover branding, light/dark themes, minimum window
size and the popover. Source audits found no remaining tracked Chinese/CJK text,
verified unchanged Unicode fixture bytes, confirmed exact example configuration
values and checked that third-party locked dependencies were unchanged.

## Verified package

Desktop test package: `Greenmole_RDP_0.1.30_aarch64_e4209df5146a.dmg`, with SHA-256 and build metadata
sidecars.

- Source: `e4209df5146aa8efd1469e74e20ca15157d1259a`.
- SHA-256: `192966e6511b5bbaf0b2a8dfdb6d9aa3d3096afccf12568a72adae76752adeb8`.
- Size: 14,651,702 bytes; aarch64, ad-hoc signed.
- Mounted bundle name, stable identifier, version/build metadata and actual arm64
  executable match. Deep strict signature and image checksum verification pass.
- Application icon bytes match the committed mascot icon. Bundled GPLv3 and
  artwork Apache license/NOTICE/provenance match their source files.
- Invalid helper/recovery arguments exit before UI/native actions. Standalone CLI
  branding and legacy command usage are verified. The read-only mount is removed.
- Desktop copies are verified before deleting the previous macrdp 0.1.29 DMG
  and its two matching sidecars. Installed app/config, existing login entries and
  stable monitor-identity hashes remain unchanged. The app is not installed or
  launched by the delivery process.
