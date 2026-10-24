//! User-configurable AI provider endpoints and privacy classification.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use url::{Host, Url};

use crate::{DataLocality, ProviderId};

pub const MAX_PROVIDER_NAME_CHARS: usize = 64;
pub const MAX_BASE_URL_CHARS: usize = 512;

/// Wire protocol spoken by a configured provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub enum ProviderBackend {
    /// Ollama native API (`/api/chat`, `/api/tags`, ...).
    Ollama,
    /// OpenAI-compatible API (`/v1/chat/completions`, `/v1/models`, ...):
    /// LM Studio, llama.cpp server, vLLM, LocalAI, ...
    OpenAiCompatible,
    /// Official OpenAI API (`https://api.openai.com/v1`).
    OpenAi,
    /// Anthropic Messages API (`https://api.anthropic.com/v1`).
    Anthropic,
    /// Google Gemini API (`https://generativelanguage.googleapis.com/v1beta`).
    Gemini,
}

impl ProviderBackend {
    /// Usual endpoint of the backend's reference server.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Ollama => "http://127.0.0.1:11434",
            Self::OpenAiCompatible => "http://127.0.0.1:1234/v1",
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        }
    }
}

/// A provider endpoint configured by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub backend: ProviderBackend,
    pub display_name: String,
    /// Base URL, e.g. `http://127.0.0.1:11434` (Ollama) or
    /// `http://127.0.0.1:1234/v1` (OpenAI-compatible). Must not contain
    /// credentials.
    pub base_url: String,
    pub enabled: bool,
}

impl ProviderConfig {
    /// Default local endpoints: Ollama and LM Studio on this machine.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                id: ProviderId::new("ollama"),
                backend: ProviderBackend::Ollama,
                display_name: "Ollama".to_owned(),
                base_url: ProviderBackend::Ollama.default_base_url().to_owned(),
                enabled: true,
            },
            Self {
                id: ProviderId::new("lm-studio"),
                backend: ProviderBackend::OpenAiCompatible,
                display_name: "LM Studio".to_owned(),
                base_url: ProviderBackend::OpenAiCompatible
                    .default_base_url()
                    .to_owned(),
                enabled: true,
            },
        ]
    }

    /// Validates the configuration and returns the parsed base URL.
    pub fn validate(&self) -> Result<Url, String> {
        if !is_valid_provider_id(self.id.as_str()) {
            return Err(format!(
                "provider id \"{}\" must be 1-64 lowercase letters, digits, '-', '_' or '.'",
                self.id
            ));
        }
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > MAX_PROVIDER_NAME_CHARS {
            return Err(format!(
                "provider \"{}\": name must be 1-{MAX_PROVIDER_NAME_CHARS} characters",
                self.id
            ));
        }
        parse_base_url(&self.base_url).map_err(|err| format!("provider \"{}\": {err}", self.id))
    }

    /// Where requests to this provider end up. Fails for invalid URLs.
    pub fn locality(&self) -> Result<DataLocality, String> {
        parse_base_url(&self.base_url).map(|url| classify_locality(&url))
    }
}

pub fn is_valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// Parses and checks a provider base URL: `http`/`https`, a host, no
/// credentials, no query or fragment.
pub fn parse_base_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    if raw.len() > MAX_BASE_URL_CHARS {
        return Err(format!(
            "URL must be at most {MAX_BASE_URL_CHARS} characters"
        ));
    }
    let url = Url::parse(raw).map_err(|err| format!("invalid URL: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("URL must use http or https".to_owned());
    }
    if url.host().is_none() {
        return Err("URL must contain a host".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL must not contain credentials".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("URL must not contain a query or fragment".to_owned());
    }
    Ok(url)
}

/// Classifies where data sent to `url` ends up. Unknown public hosts are
/// treated as third-party cloud, which the privacy policy blocks by default.
pub fn classify_locality(url: &Url) -> DataLocality {
    match url.host() {
        Some(Host::Ipv4(ip)) => classify_ipv4(ip),
        Some(Host::Ipv6(ip)) => classify_ipv6(ip),
        Some(Host::Domain(domain)) => classify_domain(domain),
        None => DataLocality::ThirdPartyCloud,
    }
}

fn classify_ipv4(ip: Ipv4Addr) -> DataLocality {
    if ip.is_loopback() {
        DataLocality::OnDevice
    } else if ip.is_private() || ip.is_link_local() || is_shared_address_space(ip) {
        DataLocality::LocalNetwork
    } else {
        DataLocality::ThirdPartyCloud
    }
}

/// 100.64.0.0/10 (carrier-grade NAT, also used by VPN meshes such as Tailscale).
fn is_shared_address_space(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

fn classify_ipv6(ip: Ipv6Addr) -> DataLocality {
    if ip.is_loopback() {
        DataLocality::OnDevice
    } else if let Some(v4) = ip.to_ipv4_mapped() {
        classify_ipv4(v4)
    } else if ip.is_unique_local() || ip.is_unicast_link_local() {
        DataLocality::LocalNetwork
    } else {
        DataLocality::ThirdPartyCloud
    }
}

fn classify_domain(domain: &str) -> DataLocality {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if domain == "localhost" || domain.ends_with(".localhost") {
        DataLocality::OnDevice
    } else if !domain.contains('.')
        || [".local", ".lan", ".home.arpa", ".internal"]
            .iter()
            .any(|suffix| domain.ends_with(suffix))
    {
        DataLocality::LocalNetwork
    } else {
        DataLocality::ThirdPartyCloud
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locality(url: &str) -> DataLocality {
        classify_locality(&parse_base_url(url).expect("valid url"))
    }

    #[test]
    fn classifies_hosts_conservatively() {
        assert_eq!(locality("http://127.0.0.1:11434"), DataLocality::OnDevice);
        assert_eq!(locality("http://localhost:1234/v1"), DataLocality::OnDevice);
        assert_eq!(locality("http://[::1]:8080"), DataLocality::OnDevice);
        assert_eq!(
            locality("http://192.168.1.20:11434"),
            DataLocality::LocalNetwork
        );
        assert_eq!(locality("http://10.0.0.5"), DataLocality::LocalNetwork);
        assert_eq!(locality("http://100.101.1.2"), DataLocality::LocalNetwork);
        assert_eq!(locality("http://[fd12::1]"), DataLocality::LocalNetwork);
        assert_eq!(
            locality("http://gpu-box:8000/v1"),
            DataLocality::LocalNetwork
        );
        assert_eq!(
            locality("http://nas.local:11434"),
            DataLocality::LocalNetwork
        );
        assert_eq!(
            locality("https://api.openai.com/v1"),
            DataLocality::ThirdPartyCloud
        );
        assert_eq!(locality("http://8.8.8.8"), DataLocality::ThirdPartyCloud);
        assert_eq!(
            locality("http://[2001:db8::1]"),
            DataLocality::ThirdPartyCloud
        );
    }

    #[test]
    fn rejects_unsafe_urls() {
        assert!(parse_base_url("ftp://127.0.0.1").is_err());
        assert!(parse_base_url("http://user:pass@127.0.0.1").is_err());
        assert!(parse_base_url("http://127.0.0.1/?key=1").is_err());
        assert!(parse_base_url("http://127.0.0.1/#x").is_err());
        assert!(parse_base_url("not a url").is_err());
        assert!(parse_base_url(&format!("http://h/{}", "a".repeat(600))).is_err());
        assert!(parse_base_url(" http://127.0.0.1:11434 ").is_ok());
    }

    #[test]
    fn validates_provider_configs() {
        for config in ProviderConfig::defaults() {
            config.validate().expect("defaults are valid");
            assert_eq!(config.locality(), Ok(DataLocality::OnDevice));
        }
        let mut bad = ProviderConfig::defaults().remove(0);
        bad.id = ProviderId::new("Bad Id");
        assert!(bad.validate().is_err());

        let mut unnamed = ProviderConfig::defaults().remove(0);
        unnamed.display_name = "  ".into();
        assert!(unnamed.validate().is_err());
    }
}
