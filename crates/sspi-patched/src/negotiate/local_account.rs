//! Opt-in embedding-server policy for domainless local NTLM accounts. This
//! selects credentials only; it never changes tokens, proofs, MICs or flags.

use crate::{AuthIdentity, Credentials, Username, UsernameParts};

pub(super) fn candidates(
    configured: &[AuthIdentity],
    supplied: &Username,
    allow_local_mapping: bool,
) -> Vec<Credentials> {
    configured
        .iter()
        .filter_map(|identity| {
            // Preserve the upstream default: format-aware full identity matching.
            if identity.username.eq_ignore_ascii_case(supplied) {
                return Some(identity.clone());
            }
            if !allow_local_mapping {
                return None;
            }
            let (UsernameParts::DownLevelLogonName(local), UsernameParts::DownLevelLogonName(wire)) =
                (identity.username.parts(), supplied.parts())
            else {
                return None;
            };
            if local.netbios_domain().is_some() || local.account_name() != wire.account_name() {
                return None;
            }
            // NTLMv2 keys depend on the original supplied user/domain. Retaining
            // that identity also leaves upstream's MIC verification unchanged.
            let mut candidate = identity.clone();
            candidate.username = supplied.clone();
            Some(candidate)
        })
        .map(Credentials::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Negotiate;
    use crate::negotiate::NegotiateConfig;
    use crate::ntlm::NtlmConfig;

    fn identity(name: &str) -> AuthIdentity {
        AuthIdentity {
            username: Username::parse(name).unwrap(),
            password: "local-password".to_owned().into(),
        }
    }

    fn selected(configured: &str, supplied: &str, allow: bool) -> Option<AuthIdentity> {
        candidates(&[identity(configured)], &Username::parse(supplied).unwrap(), allow)
            .into_iter()
            .next()
            .and_then(|credentials| credentials.to_auth_identity())
    }

    #[test]
    fn local_mapping_requires_explicit_opt_in_and_retains_wire_identity() {
        assert!(selected("local-user", "CLIENT-DOMAIN\\local-user", false).is_none());
        for domain in ["CLIENT-DOMAIN", "OTHER-DOMAIN", "."] {
            let supplied = format!("{domain}\\local-user");
            let candidate = selected("local-user", &supplied, true).unwrap();
            assert_eq!(candidate.username.inner(), supplied);
            assert_eq!(candidate.password.as_ref(), "local-password");
        }
    }

    #[test]
    fn mapping_does_not_relax_account_or_qualified_identity_checks() {
        for (configured, supplied) in [
            ("local-user", "CLIENT\\other-user"),
            ("local-user", "CLIENT\\Local-User"),
            ("SERVER\\local-user", "CLIENT\\local-user"),
            ("SERVER\\local-user", "local-user"),
            ("local-user@server.example", "local-user@client.example"),
            ("local-user", "local-user@client.example"),
        ] {
            assert!(selected(configured, supplied, true).is_none());
        }
        assert!(selected("SERVER\\local-user", "server\\LOCAL-USER", false).is_some());
    }

    #[test]
    fn policy_is_default_off_cloned_by_the_builder_and_ignored_for_clients() {
        let config = NegotiateConfig::new(Box::<NtlmConfig>::default(), None, "client".into());
        assert!(!config.protocol_config.local_account_domain_mapping());
        let opted_in = config.with_local_account_domain_mapping();
        assert!(opted_in.clone().protocol_config.local_account_domain_mapping());
        let client = Negotiate::new_client(opted_in.clone()).unwrap();
        assert!(!client.local_account_domain_mapping);
        let server = Negotiate::new_server(opted_in, vec![identity("local-user")]).unwrap();
        assert!(server.local_account_domain_mapping);
    }
}
