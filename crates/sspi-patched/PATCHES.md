# Local changes to SSPI 0.23.1

The upstream package, commit, checksum and licenses are recorded in `UPSTREAM.md`.
The published sources are retained without global formatting or dependency edits.

1. Add `NegotiateConfig::with_local_account_domain_mapping()`, implemented through
   a private `ProtocolConfig` decorator and a default-false trait method. Existing
   public config struct literals and constructors retain their shape and defaults.
2. Enable a private policy bit only for a server context, and apply it only to
   NTLM credential candidate selection. A domainless configured down-level local
   account can match the exact same, case-sensitive account with a supplied
   NetBIOS domain. Qualified configured identities and UPNs are never relaxed.
3. The mapped candidate retains the client's original `Username` before entering
   the existing upstream NTLM proof, MIC, signing and sealing implementation.
   No authentication token bytes, negotiate flags, mechanism lists, cryptographic
   checks, or CredSSP public-key/credential binding are rewritten or bypassed.
4. Unit tests cover default-off/client behavior, opt-in cloning, domain mapping,
   original wire identity retention, wrong/case-different accounts, qualified
   identity isolation, and unchanged upstream full-identity matching.
5. Reject NTLM AUTHENTICATE framing that suppresses the MIC while NTLMv2
   `MsvAvFlags` declares it present. The original parser used an outer negotiate
   flag to decide whether to capture the MIC; clearing that flag could skip the
   existing verification. A full CredSSP handshake regression reproduced this
   bypass before the fix. The presence guard examines the known MIC bit even
   when other AV flag bits are unknown, rejects inconsistent framing, and leaves
   every token byte and upstream cryptographic verification unchanged. See
   [MS-NLMP server response checking](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/34a9417d-7cc0-43b0-b61c-1f19740df66f).
   The upstream parser fixture also has a regression test for the changed flag
   combined with a corrupted MIC.

Only the local-password RDP acceptor opts into this policy. The connector/tokio
adapters use this same modern SSPI runtime while retaining their existing RDP
APIs. All other consumers retain the upstream default identity policy.
