# Local compatibility patch

Source: the published `ironrdp-connector` 0.8.0 crate, upstream commit
`81e246596ba0a84477acfc382a03382156d42dad`. Upstream licenses and VCS metadata
are retained alongside the source.

The local patch keeps the existing RDP version and state machines while using
SSPI 0.23.1 consistently with the patched acceptor. Its cryptographic dependency
requires `picky` 7.0.0-rc.27. The small API adaptations select the same explicit
KDC URL strategy, construct Negotiate through its constructor, and propagate
fallible CredSSP message-size errors before allocating the output buffer.

`Cargo.toml.orig` retains the upstream manifest layout with the corresponding
dependency pins updated; Cargo uses the normalized `Cargo.toml`.
