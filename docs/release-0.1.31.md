# Greenmole RDP 0.1.31

Greenmole RDP now explicitly credits the original
[MacRDP project](https://github.com/x6nux/macrdp), created by
[x6nux](https://github.com/x6nux), and its contributors. The acknowledgment is
visible near the top of the README, in its acknowledgments, and in the desktop
application's About page. The new root [NOTICE](../NOTICE) accompanies the app as
`Contents/Resources/licenses/NOTICE.txt` alongside the existing GPLv3 license.

Provenance was checked against the original upstream commit
[`6f01a66170f30efe9bb067fde1faaf19f0c0c08f`](https://github.com/x6nux/macrdp/commit/6f01a66170f30efe9bb067fde1faaf19f0c0c08f),
which is preserved in this repository. Original authorship, the GPLv3 license,
existing third-party acknowledgments, and the Greenmole artwork's separate
license and notice remain intact. The credit identifies Greenmole RDP as an
independently maintained fork.

The About page uses the canonical Greenmole-RDP repository and releases links.
Its content can scroll within a smaller window. Configuration, credentials,
monitor identity, capture/encoder logic and connect/disconnect behavior are
unchanged. No runtime dependency versions change.

## Validation and testing

TypeScript and the production frontend build pass. The actual About page was
rendered with mocked native IPC at 960×620 and 800×500 in light and dark themes;
original-project and author credits, external link safety, layout and absence of
page errors were checked. This does not start the server or a real RDP session.

The new DMG is for manual installation and testing. After installing it, open
About and verify the MacRDP/x6nux acknowledgment and links. Package and delivery
evidence is recorded in [the validation record](release-0.1.31-validation.json).
