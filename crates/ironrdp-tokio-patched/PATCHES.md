# Local compatibility patch

Source: the published `ironrdp-tokio` 0.8.0 crate, upstream commit
`81e246596ba0a84477acfc382a03382156d42dad`. Upstream licenses and VCS metadata
are retained alongside the source.

Only the SSPI dependency pin changes, to 0.23.1, so its optional network client
uses the same request and error types as the patched connector and acceptor.
The Tokio transport and network-client implementation remain upstream code.

`Cargo.toml.orig` retains the upstream manifest layout with that dependency pin
updated; Cargo uses the normalized `Cargo.toml`.
