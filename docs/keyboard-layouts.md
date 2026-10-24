# Keyboard layouts and shortcuts

Greenmole RDP 0.1.32 separates the layout used to interpret text from the policy
used for shortcuts. A Windows or Mac client can use a different keyboard layout
from the host Mac for the supported text layouts. The client must advertise the
layout it actually uses on the RDP connection, or the server needs an explicit
override. This does not guarantee every browser, input method or application
will handle every key combination identically.

## Recommended defaults

Leave **Settings → Keyboard → Remote keyboard layout** at **Automatic (client
layout)** and **Shortcut preference** at **Automatic** unless the client reports
the wrong layout or you want a different shortcut policy. Both server settings
require a service restart. Existing configurations without these fields use
`auto` for both; installing this release does not select a different layout.

The equivalent top-level TOML settings are:

```toml
keyboard_layout = "auto"
keyboard_shortcuts = "auto"
```

For a native Mac RDP client, select `keyboard_shortcuts = "mac"` when you want
native Command, Option and Control behavior. Native clients without the new
Greenmole preference hint retain Windows-friendly shortcuts under `auto`.
The server does not infer a client's operating system or layout from its name.

## Text layouts

`keyboard_layout = "auto"` selects a profile using the exact negotiated Windows
keyboard layout identifier. It does not select by browser language, country
alone or the host Mac's current input source.

| Layout | Negotiated ID | Explicit override |
| --- | --- | --- |
| English (US) | `0x00000409` | `us` |
| English (UK) | `0x00000809` | `uk` |
| Norwegian | `0x00000414` | `no` |
| Swedish | `0x0000041D` | `se` |
| Danish | `0x00000406` | `dk` |
| German | `0x00000407` | `de` |
| French AZERTY | `0x0000040C` | `fr` |

These profiles interpret ordinary text, Shift, Caps Lock and their supported
AltGr/dead-key combinations before injecting text into macOS. French and German
Caps behavior includes the appropriate number-row and punctuation keys, rather
than assuming that Caps affects letters only. Already-translated RDP Unicode
events continue using the existing Unicode path.

An override, for example `keyboard_layout = "no"`, replaces the advertised
layout only for text interpretation. Use it to correct a known client-reporting
problem, not to choose the language you want to type. The stored/diagnosed
negotiated identity remains available for troubleshooting.

An unknown ID, zero, or an unsupported variant such as US Dvorak is not treated
as the nearest supported locale. It uses physical scan-code mapping and the
Mac's selected input source, with a warning for unsupported automatic selection.
`keyboard_layout = "physical"` explicitly chooses that same escape hatch. It
disables these text tables; it does not disable the separately selected shortcut
policy. Match the host input source to the desired physical interpretation when
using this mode. Unknown configuration strings are rejected.

The accepted Norwegian mapping is protected by a frozen 124-chord fixture from
0.1.31, including its existing compatibility AltGr and dead-key combinations.
The standard Norwegian dead-tilde key is accepted as well as the existing
AltGr+P compatibility chord.

## Greenmole browser connections

Greenmole reads browser characters/keysyms, then guacd encodes them using the
connection's **Keyboard layout** (`server-layout`). This is the layout on the
RDP wire, not necessarily the physical keyboard's layout. Leaving this setting
blank uses the established US transport layout (`en-us-qwerty`), with Unicode
events for text that the transport layout cannot represent directly.

For the existing Norwegian Windows-to-Mac setup, keep the Greenmole connection
layout as it is and leave the server layout at `auto`. Norwegian characters
remain available even when the negotiated transport ID is US. Changing the host
override to Norwegian while guacd still sends US scan codes would create a
mismatch. Explicit non-US Greenmole connection layouts also work with their
matching supported profiles. The updated gateway accepts `nb-no-qwerty` as an
alias for its canonical `no-no-qwerty` name.

In the updated Greenmole connection menu, **Input method → Greenmole RDP
shortcuts** selects the browser preference. **Automatic (this device)** chooses
Mac shortcuts on macOS/iOS/iPadOS and Windows-friendly shortcuts elsewhere;
this detection never guesses the text layout. A browser preference change takes
effect after disconnect/reconnect. See the companion [Greenmole keyboard
guide](https://github.com/beraadim/Greenmole/blob/main/docs/keyboard-layouts.md).

## Shortcut policy

| Server value | Behavior |
| --- | --- |
| `auto` | Uses a recognized Greenmole shortcut hint; otherwise preserves Windows-friendly behavior. |
| `windows` | Maps Ctrl+A/X/C/V and Ctrl+Enter to the corresponding Command shortcuts. Left Alt acts as Command and Windows/Super acts as Option. AltGr text retains its layout-specific handling. |
| `mac` | Keeps native Command, Option and Control meanings, including genuine Control+C in Terminal. |
| `physical` | Keeps Control, maps Windows/Super to Command and Alt to Option, without the Ctrl-to-Command editing substitutions. Shown as **Standard modifiers** in the server app. |

Explicit server policy takes priority over the browser/gateway preference.
With server `auto`, a Greenmole administrator's explicit
`greenmole-keyboard-mode` connection parameter takes priority over the browser
preference. Missing or unrecognized versioned hints keep the legacy Windows
policy. The hint is a preference only; it grants no permissions and does not
change authentication or desktop ownership.

Text layout and shortcut policy remain independent. Supported ASCII-letter
shortcuts use the source letter and the Mac's current ASCII-capable input source
to find its native key position, so French Ctrl+A is not confused with Ctrl+Q.
If that host lookup is unavailable, ordinary chords retain the physical mapping;
the Windows-friendly editing substitutions report an error instead of risking
the wrong editing action. Held keys retain their selected native mapping through repeats
and release; disconnect and synchronization clear session key state. Native
modifiers temporarily cleared for Unicode typing are restored before the next
physical action, such as Shift+Arrow.

AltGr synthesized by guacd encodes text even for a browser on a Mac. It must not
be reinterpreted as a native Option chord. A native Mac client using explicit
Mac policy retains native Option handling. These paths are intentionally
distinguished by the recognized Greenmole hint.

## Diagnostics and acceptance

The new `Selected session keyboard profiles` log message reports
`keyboard_layout`, `keyboard_subtype`, `text_profile` and `shortcut_profile`.
These selection messages and the unsupported-layout warning contain no typed
characters. Shareable diagnostic exports include only allowlisted keyboard
preferences and continue to exclude raw input events and typed contents. This
does not make existing raw trace/debug logs or the live input-debug view safe
to share as a substitute for the sanitized export.

After installing the test build, use ordinary text and shortcuts in TextEdit
and Terminal to check your actual client/host combination:

1. Confirm letters, punctuation, `@`, braces, backslash, accented characters,
   Caps Lock and dead-key composition for the selected layout. For Norwegian,
   include `æøå ÆØÅ` and the existing AltGr combinations.
2. Check the selected editing shortcuts and, in Mac/Standard-modifier mode,
   genuine Control+C in Terminal. Check Shift+Arrow after typing an uppercase
   letter while continuing to hold Shift.
3. Release modifiers, reconnect and confirm typing starts cleanly. Use normal
   connect/disconnect and verify window restoration as before.

Automated tests use recorded input or non-posting event inspection; they do not
prove that every physical keyboard, browser, native RDP client or Mac application
behaves identically. Pre-existing browser handling of Option with navigation
keys can consume or reinterpret a chord before it reaches the server. Browser
and operating-system reserved shortcuts remain subject to those restrictions.
Supplementary Unicode characters outside the BMP, including many emoji, and
full IME composition are not fixed or validated end to end by this release;
the existing Unicode transport/injection limitations still apply.

The additional tables retain their Apache Guacamole attribution and license in
[the layout module](../crates/macrdp-core/src/handler/layouts/). French dead
tilde/grave and German literal AltGr keys were checked against Microsoft's
[French](https://github.com/microsoft/Windows-driver-samples/blob/main/input/layout/all_kbds/kbdfr/kbdfr.c)
and [German](https://github.com/microsoft/Windows-driver-samples/blob/main/input/layout/all_kbds/kbdgr/kbdgr.c)
keyboard samples.
