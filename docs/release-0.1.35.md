# Greenmole RDP 0.1.35

Fixes native Windows Remote Desktop NLA negotiation. The observed failures in
0.1.34 included `mixed username format` and SPNEGO tokens rejected by a raw NTLM
parser before password verification. Entering the correct plain account name
could therefore still fail to authenticate.

The local-account authentication path uses a separately versioned, maintained
SSPI-rs server implementation with the upstream SPNEGO integrity and CredSSP
message-state fixes. Initial raw NTLM and SPNEGO connections select their
appropriate server modes without retrying a different format mid-context. The
existing optional Kerberos path retains its API. This isolates the change from
the rest of the RDP graphics, audio and input stack.

The existing connector/Tokio adapter versions retain their RDP APIs, with small
compatibility patches for the newer authentication dependency. A narrowly scoped
local-account policy allows a domainless configured account to use a Windows
computer/domain qualifier with the same account name. It preserves the original
client identity for password-proof and integrity validation. This policy is an
explicit server opt-in; standard authentication consumers retain full identity
matching by default.

Adversarial handshake tests also reproduced an upstream NTLM parser case where
an outer flag could suppress a declared message integrity check. The vendored
runtime rejects that inconsistent framing, including unknown extra flag bits,
while retaining upstream proof, key, signing and sealing algorithms. Its exact
source provenance and the small local changes are recorded beside the vendor.

TLS-only fallback no longer sets the authenticated flag merely because the NLA
helper returned without performing NLA. Its success state and admission counters
now depend on actual credential validation. Failed password attempts remain
subject to retry accounting.

A mandatory logging metadata filter suppresses SSPI debug/trace events and spans,
including during filter reloads, before field values can be recorded. Verbose
upstream instrumentation can otherwise contain serialized credentials or token
data. Acceptor-side diagnostics use stage/format/size or fixed error categories
instead of token dumps. The packaged attribution includes SSPI-rs licenses.

Saved usernames/passwords and the active installation are not changed by the
build. See [Windows authentication](windows-authentication.md) for explicit
credential entry and acceptance tests. The microphone driver from 0.1.34 does not
need to be reinstalled for this update. Source builds now declare Rust 1.93 as
the minimum for the newer authentication dependency; the pinned 1.99.0 toolchain
is unchanged.

The complete repository checks passed: 959 workspace/desktop Rust tests, 39
Python tests, 11 JavaScript tests, the frontend production build and native
driver tests. The separately patched acceptor passed all 19 tests, including
complete raw NTLM, SPNEGO and Kerberos-first/NTLM-selected handshakes and
credential/integrity/binding rejection cases. Five focused vendor policy/parser
checks also passed. Seven existing manual workspace tests remain ignored. The
standard check script now runs the acceptor suite explicitly.

The optimized ARM64 DMG was verified read-only and copied to the current Mac
mini's Desktop with its checksum and build metadata. Package signatures,
version/revision, bundled authentication/RDP licenses and microphone installer
digest were verified. The microphone payload matches 0.1.34. Active app hashes,
app processes and the Core Audio process were preserved; credentials, installed
app and driver were not changed. See the
[validation record](release-0.1.35-validation.json). A real native Windows retest
remains necessary after the user deploys the update.
