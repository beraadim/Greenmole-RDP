# Windows Remote Desktop authentication

Greenmole RDP authenticates the account configured in its own settings. Use that
account name and password when connecting from Windows; it does not validate
against the Mac's login account or a Windows/Microsoft account.

If Windows suggests a name such as `macrdp@...`, choose **More choices → Use a
different account** and enter the configured plain account name, for example
`macrdp`. Check the saved connection's username if Windows keeps selecting a
different identity. Microsoft documents the explicit credential prompt in
[`mstsc /prompt`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/mstsc).

## Versions 0.1.35 and 0.1.36

Earlier builds used an authentication implementation that forwarded Windows
SPNEGO-wrapped negotiation tokens to a raw NTLM parser. Correct credentials could
therefore fail before password verification, with an error such as
`InvalidToken: Read NTLM signature is invalid`. An email-style username combined
with a separate domain could also produce `mixed username format`.

The updated local-account server path uses the maintained SSPI-rs implementation
for SPNEGO and NTLM. The first token selects the appropriate protocol for the
connection, including raw NTLM clients such as Guacamole. Authentication tokens
are handled by that implementation rather than manually unwrapped or rewritten.
NLA, integrity checks and CredSSP's TLS public-key binding remain enabled.

After replacing the application and restarting the RDP service when convenient,
make a fresh Windows connection with the configured plain username. Confirm that
a correct password connects and an incorrect password is rejected. Also retest
the existing Greenmole/Guacamole connection before testing microphone input.
This app update does not require reinstalling the microphone driver from 0.1.34.

Version 0.1.36 corrects two additional NTLM validation defects. The server now
verifies the received NTLMv2 proof against the exact client response bytes before
deriving session keys. It no longer reconstructs that response using its own
timestamp. MIC presence follows the NTLMv2 `MsvAvFlags` field, independently of
the outer `NEGOTIATE_TARGET_INFO` flag, which is ignored for AUTHENTICATE messages.
Declared MICs remain mandatory and are verified in constant time.

If Windows displays `0x904` with extended error `0x7`, correlate the attempt with
the server log rather than using those client codes to select a fix. In 0.1.35,
`LogonDenied` can describe account lookup, password/proof or integrity rejection;
changing from SPNEGO to raw NTLM can also change the error category for the same
account lookup failure. The code alone does not prove the username or password
was accepted. Version 0.1.36 adds fixed failure stages and reasons at warning
level, including account lookup, NTLMv2 proof, MIC and TLS ClientInfo validation,
without recording incoming usernames, passwords or token contents.

## Diagnostic handling

The application records authentication stages and fixed error categories without
credential or token contents. SSPI debug/trace events and spans are suppressed by
a mandatory metadata filter because upstream verbose instrumentation can contain
serialized credentials and authentication proofs. This restriction persists
through logging configuration reloads; ordinary application diagnostics remain
available.

TLS-only fallback connections are marked authenticated only after the configured
credentials have actually been validated. Invalid attempts remain failures for
connection admission and retry accounting.

Source builds use the pinned Rust toolchain. The newer authentication dependency
requires Rust 1.93 or later; the repository currently pins 1.99.0.
