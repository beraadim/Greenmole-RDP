# Vendored SSPI authentication runtime

Source: [sspi 0.23.1 on crates.io](https://crates.io/crates/sspi/0.23.1).
The published package was downloaded from
`https://crates.io/api/v1/crates/sspi/0.23.1/download` and verified against the
crates.io version metadata checksum.

- Package archive SHA-256: `3d3fa5e1405884e88be309bd801760caa84c654be746fe4261ab407496263692`
- Upstream Git commit from `.cargo_vcs_info.json`: `c18aa7c01f746533cf1c4e9b41cd1440d8712b88`
- Upstream licenses: original `LICENSE-MIT` and `LICENSE-APACHE` are retained.

Only the changes documented in `PATCHES.md` differ from the published sources.
The version remains 0.23.1. The root and UI Cargo patches unify authentication
on this modern runtime; the vendored connector/tokio adapters retain their
existing RDP APIs while using the same SSPI version.

The vendored runtime omits upstream tools/, .agents/, .claude/, .github/, AGENTS.md, CLAUDE.md and
the package-development Cargo.lock. These are unrelated tooling/agent metadata;
application dependency versions are pinned in the repository root and UI locks.
The Rust runtime sources and test assets retain their published contents except
for the explicit patches listed in PATCHES.md.
