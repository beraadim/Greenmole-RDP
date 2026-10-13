# Public release preparation

Use one stable application identity (`com.macrdp.app`) and a Developer ID
Application certificate across releases. The local test workflow defaults to
ad-hoc signing; its changing binary identity can require renewed privacy grants.
Notarization checks distribution security and does not preapprove macOS privacy
permissions.

## Signing and notarization

Install the certificate and its private key into the build user's Keychain.
Configure a notarization credential profile locally using Apple's `notarytool
store-credentials` workflow. Keep private keys, passwords and API keys outside
this repository and terminal transcripts.

Run on the Mac build machine:

```sh
export MACRDP_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)'
export MACRDP_NOTARY_PROFILE='macrdp-notary'
export MACRDP_PUBLIC_RELEASE=1
bash macrdp-ui/build.sh
```

Public mode rejects missing signing/notarization configuration before building.
The pipeline patches the Swift runtime first, signs the app with hardened runtime
and a secure timestamp, signs the DMG, submits it for notarization, staples and
validates the ticket, checks Gatekeeper and verifies the final image. Checksums
are generated after signing and stapling. Submission failure stops packaging.

For local testing, omit these variables. The existing ad-hoc signed DMG remains
available without an Apple developer account. Certificate signing without
notarization is possible for internal testing, but public mode requires both.

## Installation and upgrade acceptance

Copy macrdp.app into `/Applications` and launch that installed copy consistently.
Complete required Screen Recording and Accessibility setup on the physical
display before starting the service. Enable clipboard file sharing only when
needed; protected folders can require separate consent. System audio capture
does not require microphone recording.

Before publishing, test an upgrade between two releases signed by the same
developer identity on a separate Mac. Verify privacy grants, Keychain password,
saved preferences, managed TLS identity and login startup. Test fresh permission
denial, repeated Stop, reconnect and diagnostics export. A signed/notarized
release still requires this installation and upgrade acceptance check.

References: [Developer ID](https://developer.apple.com/developer-id/),
[notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution),
[Apple DTS on changing ad-hoc identities](https://developer.apple.com/forums/thread/819406).
