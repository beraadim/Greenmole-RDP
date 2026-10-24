# Greenmole RDP 0.1.32

Keyboard text layout and shortcut policy are now independent settings. The
server interprets supported layouts reported by native RDP clients and
Greenmole/Guacamole without assuming that all users have a Norwegian keyboard.
The existing Norwegian Windows mapping remains protected by a frozen fixture
covering all 124 previously recorded text chords.

## Layouts and shortcut preferences

**Settings → Keyboard** adds **Remote keyboard layout** and **Shortcut
preference**, also available as `keyboard_layout` and `keyboard_shortcuts` in
TOML. Both default to `auto` for existing configurations and require a service
restart. Unknown setting values are rejected before saving or starting the
service. See [keyboard configuration](keyboard-layouts.md) for exact IDs,
precedence, examples and test guidance.

Automatic text selection matches the complete negotiated layout ID for US, UK,
Norwegian, Swedish, Danish, German and French AZERTY. Explicit overrides are
available for each. Unsupported/zero IDs and unrecognized variants use physical
keys and the host Mac's input source; `physical` explicitly selects that path.
The client name does not select a text profile.

Windows-friendly shortcuts preserve the established Ctrl+A/X/C/V/Enter editing
conveniences, Alt-to-Command and Windows-to-Option mapping. Native Mac and
Standard-modifier modes preserve genuine Control shortcuts. Automatic policy
uses the updated Greenmole browser/gateway preference when recognized; older
and native clients without that hint retain Windows-friendly behavior. An
explicit host preference has final priority. The versioned hint carries no
authentication or security authority.

ASCII-letter shortcuts resolve the source letter against the host input source,
so a French A is not treated as the physical US Q position. Key repeats and
releases keep their original mapping across modifier/context changes. Unicode
typing no longer leaves a still-held modifier inactive for the next physical
action, such as Shift+Arrow. Synchronization and disconnect clear held mappings
through the existing cleanup paths.

The additional layouts cover Shift, Caps Lock, ISO keys and supported AltGr/dead
keys. French/German Caps and AltGr behavior were checked against Microsoft's
layout samples. French AltGr+2/7 start dead tilde/grave composition; German
AltGr+ß produces backslash and AltGr++ produces literal tilde. Undefined text
levels and dead-key releases cannot fall through as unrelated host key events.
Native Mac Option is distinguished from the AltGr that guacd synthesizes to
encode browser text.

## Upgrade and diagnostic behavior

Keep the existing Greenmole connection keyboard layout unchanged for the working
Norwegian setup. Its default US transport layout does not imply a US physical
keyboard; browser characters not represented by that layout use Unicode events.
The companion Greenmole update adds device-aware shortcut preference selection
and matching French/German keymap corrections. Browser preference changes need
a fresh connection.

New profile-selection and unsupported-layout messages contain layout IDs,
subtype and profile names, without typed text. Sanitized diagnostic exports
include allowlisted configuration choices and continue excluding raw key/input
contents. Existing raw logs and live input debugging are not covered by that
export guarantee.

The GPLv3 license, original MacRDP attribution and existing third-party notices
remain intact. Apache Guacamole-derived keyboard tables include their Apache
License 2.0 and NOTICE in the source and packaged license resources.

Capture/encoding, graphics negotiation, stable monitor identity, physical
display/window restoration, guarded sessions and connect/disconnect behavior
are unchanged. Keyboard settings do not migrate credentials or display state.

## Validation and manual testing

Focused automated tests cover exact layout selection, explicit override
precedence, the Norwegian fixture, locale symbols/Caps/dead keys, native Mac
versus browser AltGr, logical shortcuts on a different host layout, held-key
repeats/releases and session reset. Modifier reconciliation tests cover both
Shift keys, shortcut policies and AltGr suppression. These tests do not post
native keyboard events or exercise actual remote sessions.

Install the new Desktop DMG manually after quitting the old application. Keep
the current connection/display settings and test the normal Norwegian typing
and shortcut workflow first. If testing from a Mac browser, also check genuine
Control+C in Terminal with Native Mac policy. Check Shift+Arrow after translated
uppercase text, reconnect, and confirm normal window restoration.

Physical client/host acceptance is still required. Pre-existing browser
Option/navigation handling and reserved shortcuts are not changed. Non-BMP
Unicode, including many emoji, and complete IME composition remain outside this
release's fixes and end-to-end validation. Supported layouts are not a universal
typing guarantee for every application or input method.

The optimized ARM64 DMG was built, its ad-hoc signature, checksum and bundled
licenses verified, and the package delivered to the Mac mini Desktop with its
checksum and build metadata. Older recognized Desktop installer files were
removed after delivery. The installed application and configuration hashes were
unchanged. See [release validation](release-0.1.32-validation.json) for recorded
automated checks and package provenance.
