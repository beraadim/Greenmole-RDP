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
5. Determine NTLM AUTHENTICATE MIC presence from NTLMv2 `MsvAvFlags`, including
   when other AV flag bits are unknown. `NEGOTIATE_TARGET_INFO` describes the
   CHALLENGE message's TargetInfo field and must not select AUTHENTICATE MIC
   parsing. The earlier local guard rejected otherwise valid messages with
   that outer flag clear; the upstream parser could instead skip verification.
   Require the complete declared MIC to precede all nonempty payload buffers,
   and retain every original token byte for cryptographic verification. See
   [MS-NLMP server response checking](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/34a9417d-7cc0-43b0-b61c-1f19740df66f).
   Regression tests cover a complete valid NTLM exchange with that outer flag
   clear, a corrupted MIC, a removed declared MIC with adjusted payload offsets,
   payload overlap, and unknown AV flag bits.

6. Verify the received NTLMv2 proof in constant time over the exact client blob,
   then derive the session key from that verified proof. The upstream server
   reconstructed the blob with its own timestamp and did not compare the
   received proof. Verification now also applies to responses without a MIC.
   MIC comparison uses constant-time HMAC verification. The original token is
   retained, and a bounded view of its NT response security buffer supplies the
   proof/blob without adding another stored token copy. Remove private parsed
   fields that were used only by the reconstructed response. Independent
   Microsoft test vectors cover the published response and session key, a
   different server-side timestamp, wrong passwords, every response byte,
   changed server challenges and truncated security buffers. Legacy fabricated
   positive fixtures now use complete valid exchanges.
7. Emit warning-level fixed account/proof/MIC rejection categories and username
   format/match booleans. Incoming names, passwords, hashes and token contents
   are never recorded by these events. The application's mandatory debug/trace
   filter continues to suppress verbose upstream instrumentation.

Only the local-password RDP acceptor opts into this policy. The connector/tokio
adapters use this same modern SSPI runtime while retaining their existing RDP
APIs. All other consumers retain the upstream default identity policy.
