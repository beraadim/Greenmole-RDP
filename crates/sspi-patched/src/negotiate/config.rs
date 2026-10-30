use std::fmt::Debug;

use crate::{NegotiatedProtocol, Result};

pub trait ProtocolConfig: Debug + Send + Sync {
    fn new_instance(&self) -> Result<NegotiatedProtocol>;
    fn new_server_instance(&self) -> Result<NegotiatedProtocol> {
        self.new_instance()
    }
    fn box_clone(&self) -> Box<dyn ProtocolConfig>;

    /// Local NTLM account mapping is disabled unless explicitly requested by
    /// the embedding server. Client contexts never apply this policy.
    fn local_account_domain_mapping(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct LocalAccountProtocolConfig(Box<dyn ProtocolConfig>);

impl ProtocolConfig for LocalAccountProtocolConfig {
    fn new_instance(&self) -> Result<NegotiatedProtocol> {
        self.0.new_instance()
    }

    fn new_server_instance(&self) -> Result<NegotiatedProtocol> {
        self.0.new_server_instance()
    }

    fn box_clone(&self) -> Box<dyn ProtocolConfig> {
        Box::new(Self(self.0.box_clone()))
    }

    fn local_account_domain_mapping(&self) -> bool {
        true
    }
}

#[derive(Debug)]
pub struct NegotiateConfig {
    pub protocol_config: Box<dyn ProtocolConfig>,
    pub package_list: Option<String>,
    /// Computer name, or "workstation name", of the client machine performing the authentication attempt
    ///
    /// This is also referred to as the "Source Workstation", i.e.: the name of the computer attempting to logon.
    pub client_computer_name: String,
}

impl NegotiateConfig {
    /// Permit a server's domainless local NTLM account to match the exact same
    /// account with a client-supplied NetBIOS domain. Qualified configured
    /// accounts and UPN identities retain their existing identity semantics.
    /// The original wire identity is retained for all NTLM proof/MIC checks.
    /// This opt-in has no effect on clients or non-NTLM mechanisms.
    pub fn with_local_account_domain_mapping(mut self) -> Self {
        self.protocol_config = Box::new(LocalAccountProtocolConfig(self.protocol_config));
        self
    }

    /// Creates a new instance of [NegotiateConfig].
    ///
    /// `package_list` specifies allowed security packages for user authorization.
    /// Security packages are specified as a comma-separated list of package names in lowercase.
    /// If the security package is not allowed, then prepend '!' to its name. Examples:
    ///
    /// - "kerberos,ntlm" - allows both Kerberos and NTLM but not PKU2U.
    /// - "kerberos,!ntlm" - allows Kerberos but not NTLM and not PKU2U. Forces the use of Kerberos.
    /// - "!kerberos,ntlm" - allows NTLM but not Kerberos and not PKU2U. Forces the use of NTLM.
    /// - "!ntlm" - allows Kerberos and PKU2U but not NTLM.
    ///
    /// If `package_list` is None, then all packages are allowed and Kerberos is preferred.
    pub fn new(
        protocol_config: Box<dyn ProtocolConfig>,
        package_list: Option<String>,
        client_computer_name: String,
    ) -> Self {
        Self {
            protocol_config,
            package_list,
            client_computer_name,
        }
    }

    pub fn from_protocol_config(protocol_config: Box<dyn ProtocolConfig>, client_computer_name: String) -> Self {
        Self {
            protocol_config,
            package_list: None,
            client_computer_name,
        }
    }
}

impl Clone for NegotiateConfig {
    fn clone(&self) -> Self {
        Self {
            protocol_config: self.protocol_config.box_clone(),
            package_list: self.package_list.clone(),
            client_computer_name: self.client_computer_name.clone(),
        }
    }
}
