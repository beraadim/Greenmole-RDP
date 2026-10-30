# Greenmole RDP 0.1.36

Windows connection attempts against 0.1.35 at 12:50:55 and 12:51:26 Oslo time on
30 October 2026 failed with local CredSSP `LogonDenied`, followed by TLS fallback
credential rejection. The client reported `0x904`, extended error `0x7`. The
existing warning log does not identify the precise rejected check, so those
attempts do not establish whether the supplied account, password or an
interoperability check caused the rejection.

Review found two NTLM validation defects. Authentication now verifies the
received NTLMv2 proof over the exact received response blob, and derives the
session key only after that verification succeeds. Reconstructing the response
with the server timestamp could fail when an independent client supplied other
authenticated bytes; accepting a missing MIC also previously skipped proof
validation. Proof and MIC comparisons now use constant-time HMAC verification.

The MIC parser now follows the response's `MsvAvFlags` MIC bit independently of
the ignored AUTHENTICATE `NEGOTIATE_TARGET_INFO` flag. A complete declared MIC
must precede the payload, and missing, overlapping or corrupted MICs still fail.
This corrects 0.1.35's overly strict outer-flag requirement without removing
integrity checks or changing token bytes.

Authentication rejection logs now distinguish account lookup, proof and MIC
validation, selected raw NTLM/SPNEGO format, and TLS ClientInfo account/password
rejection. They use fixed categories and identity-shape booleans, never incoming
usernames, passwords, hashes or token contents. TLS password comparison also
uses constant-time comparison. The mandatory SSPI debug/trace filter remains.

Regression coverage includes an independent Microsoft NTLMv2 example,
password/proof/challenge tampering without a MIC, valid MICs with the ignored
outer flag cleared, malformed MIC framing, and complete raw NTLM/SPNEGO
CredSSP exchanges. Native Windows acceptance still requires a fresh user test;
the observed 0.1.35 failure cannot be conclusively attributed to these defects.

The microphone driver payload is unchanged. Building and copying the DMG does
not install the app, alter credentials, restart the RDP service or restart Core
Audio. The user deploys and tests the package when convenient.
