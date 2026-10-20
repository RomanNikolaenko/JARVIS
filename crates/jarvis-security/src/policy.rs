//! Security Policy Engine: decides for every tool call whether it may run,
//! needs the user's approval or is refused.
//!
//! Rules, in order:
//! 1. the tool must be turned on (allowlist);
//! 2. every path must be inside the workspace and outside JARVIS's data;
//! 3. programs must be on the command allowlist;
//! 4. network access must be turned on, only to public http(s) hosts;
//! 5. files that look like secrets always need approval;
//! 6. high-risk calls always need approval; low and medium risk need it
//!    unless the user chose to auto-approve that level.

use std::collections::BTreeSet;
use std::net::IpAddr;

use url::{Host, Url};

use crate::scope::{FileScope, is_sensitive};
use crate::types::{ActionRequest, Capability, PolicyDecision, RiskLevel};

/// The user's security settings, as the policy needs them.
#[derive(Debug, Clone, Default)]
pub struct PolicyConfig {
    /// Tools that may be used at all.
    pub enabled_tools: BTreeSet<String>,
    pub scope: FileScope,
    /// Program names (without extension, case-insensitive) that may run.
    pub allowed_programs: BTreeSet<String>,
    pub allow_network: bool,
    pub auto_approve_low: bool,
    pub auto_approve_medium: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SecurityPolicy {
    config: PolicyConfig,
}

impl SecurityPolicy {
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    pub fn scope(&self) -> &FileScope {
        &self.config.scope
    }

    pub fn evaluate(&self, action: &ActionRequest) -> PolicyDecision {
        let config = &self.config;
        let deny = |reason: String| PolicyDecision::Deny { reason };

        if !config.enabled_tools.contains(&action.tool) {
            return deny(format!("the tool \"{}\" is turned off", action.tool));
        }
        for path in &action.paths {
            if let Err(err) = config.scope.ensure_inside(path) {
                return deny(err.to_string());
            }
        }
        if let Some(program) = &action.program {
            if let Err(reason) = check_program(program, &config.allowed_programs) {
                return deny(reason);
            }
        } else if action.capabilities.contains(&Capability::ProcessSpawn) {
            return deny("no program was given".to_owned());
        }
        if action.capabilities.contains(&Capability::NetFetch) {
            if !config.allow_network {
                return deny("network access for tools is turned off".to_owned());
            }
            match action.url.as_deref().map(check_url) {
                Some(Ok(_)) => {}
                Some(Err(reason)) => return deny(reason),
                None => return deny("no URL was given".to_owned()),
            }
        }
        if action.capabilities.contains(&Capability::OpenExternal)
            && let Some(Err(reason)) = action.url.as_deref().map(check_url)
        {
            return deny(reason);
        }

        if action.paths.iter().any(|p| is_sensitive(p)) {
            return PolicyDecision::RequireApproval {
                reason: "the file may contain secrets (keys, credentials)".to_owned(),
            };
        }
        let auto = match action.risk {
            RiskLevel::Low => config.auto_approve_low,
            RiskLevel::Medium => config.auto_approve_medium,
            RiskLevel::High => false,
        };
        if auto {
            PolicyDecision::Allow
        } else {
            PolicyDecision::RequireApproval {
                reason: format!("{} risk action", action.risk.as_str()),
            }
        }
    }
}

/// A program name without a path or shell syntax (`git`, `npm.cmd`).
pub fn is_plain_program_name(program: &str) -> bool {
    !(program.is_empty()
        || program.len() > 64
        || program.contains([
            '/', '\\', ' ', '"', '\'', '&', '|', ';', '<', '>', '`', '$', '%', '\0',
        ])
        || program.starts_with('-')
        || program.starts_with('.'))
}

/// Program names on the allowlist match by file stem, case-insensitively.
/// Paths and shell syntax are refused: tools run programs without a shell.
fn check_program(program: &str, allowed: &BTreeSet<String>) -> Result<(), String> {
    if !is_plain_program_name(program) {
        return Err(format!("\"{program}\" is not a plain program name"));
    }
    let name = program.to_lowercase();
    let stem = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".cmd"))
        .or_else(|| name.strip_suffix(".bat"))
        .unwrap_or(&name);
    if allowed.iter().any(|a| a.eq_ignore_ascii_case(stem)) {
        Ok(())
    } else {
        Err(format!(
            "\"{program}\" is not on the list of programs tools may run"
        ))
    }
}

/// Only public http(s) URLs: loopback, private, link-local and similar
/// addresses are refused so tools cannot reach local services (SSRF).
/// Host names are re-checked after DNS resolution by the tool.
pub fn check_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|err| format!("invalid URL: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with credentials are not allowed".to_owned());
    }
    match url.host() {
        None => Err("the URL has no host".to_owned()),
        Some(Host::Domain(domain)) => {
            let domain = domain.to_ascii_lowercase();
            if domain == "localhost" || domain.ends_with(".localhost") || domain.ends_with(".local")
            {
                Err("local hosts are not allowed".to_owned())
            } else {
                Ok(url)
            }
        }
        Some(Host::Ipv4(ip)) => check_ip(IpAddr::V4(ip)).map(|()| url),
        Some(Host::Ipv6(ip)) => check_ip(IpAddr::V6(ip)).map(|()| url),
    }
}

/// Refuses addresses that are not on the public internet.
pub fn check_ip(ip: IpAddr) -> Result<(), String> {
    let private = match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64 // CGNAT
                || ip.octets()[0] >= 224
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (ip.segments()[0] & 0xffc0) == 0xfe80 // link local
                || ip.to_ipv4_mapped().is_some_and(|v4| check_ip(IpAddr::V4(v4)).is_err())
        }
    };
    if private {
        Err(format!("{ip} is not a public address"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn policy(dir: &tempfile::TempDir) -> SecurityPolicy {
        std::fs::create_dir_all(dir.path().join("work")).expect("mkdir");
        SecurityPolicy::new(PolicyConfig {
            enabled_tools: ["read_file", "write_file", "run_command", "fetch_url"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            scope: FileScope::new([dir.path().join("work")], []),
            allowed_programs: ["git".to_owned()].into_iter().collect(),
            allow_network: true,
            auto_approve_low: true,
            auto_approve_medium: false,
        })
    }

    fn action(tool: &str, risk: RiskLevel, capability: Capability) -> ActionRequest {
        ActionRequest {
            tool: tool.to_owned(),
            capabilities: vec![capability],
            risk,
            summary: String::new(),
            paths: Vec::new(),
            program: None,
            url: None,
        }
    }

    fn in_work(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        dir.path().join("work").join(name)
    }

    #[test]
    fn risk_decides_about_approval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = policy(&dir);
        let mut read = action("read_file", RiskLevel::Low, Capability::FsRead);
        read.paths = vec![in_work(&dir, "notes.md")];
        assert_eq!(policy.evaluate(&read), PolicyDecision::Allow);

        let mut write = action("write_file", RiskLevel::Medium, Capability::FsWrite);
        write.paths = vec![in_work(&dir, "notes.md")];
        assert!(matches!(
            policy.evaluate(&write),
            PolicyDecision::RequireApproval { .. }
        ));

        let mut run = action("run_command", RiskLevel::High, Capability::ProcessSpawn);
        run.program = Some("git".into());
        assert!(matches!(
            policy.evaluate(&run),
            PolicyDecision::RequireApproval { .. }
        ));
    }

    #[test]
    fn disabled_tools_outside_paths_and_unknown_programs_are_denied() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = policy(&dir);
        let denied = |a: &ActionRequest| matches!(policy.evaluate(a), PolicyDecision::Deny { .. });

        assert!(denied(&action(
            "delete_path",
            RiskLevel::High,
            Capability::FsDelete
        )));
        let mut outside = action("read_file", RiskLevel::Low, Capability::FsRead);
        outside.paths = vec![dir.path().join("secret.txt")];
        assert!(denied(&outside));

        let mut run = action("run_command", RiskLevel::High, Capability::ProcessSpawn);
        for program in [
            "powershell",
            "git; rm -rf /",
            "C:\\tools\\git.exe",
            "-c",
            "",
        ] {
            run.program = Some(program.to_owned());
            assert!(denied(&run), "{program}");
        }
        run.program = Some("GIT.EXE".to_owned());
        assert!(!denied(&run));
        run.program = None;
        assert!(denied(&run));
    }

    #[test]
    fn secrets_always_need_approval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = policy(&dir);
        let mut read = action("read_file", RiskLevel::Low, Capability::FsRead);
        read.paths = vec![in_work(&dir, ".env")];
        assert!(matches!(
            policy.evaluate(&read),
            PolicyDecision::RequireApproval { reason } if reason.contains("secrets")
        ));
    }

    #[test]
    fn network_access_is_limited_to_public_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = policy(&dir);
        let mut fetch = action("fetch_url", RiskLevel::Medium, Capability::NetFetch);
        fetch.url = Some("https://example.com/page".into());
        assert!(matches!(
            policy.evaluate(&fetch),
            PolicyDecision::RequireApproval { .. }
        ));
        for url in [
            "http://127.0.0.1:11434/api/tags",
            "http://localhost:8080",
            "http://192.168.1.10/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://printer.local/",
            "file:///etc/passwd",
            "https://user:pw@example.com/",
            "ftp://example.com/",
        ] {
            fetch.url = Some(url.into());
            assert!(
                matches!(policy.evaluate(&fetch), PolicyDecision::Deny { .. }),
                "{url}"
            );
        }

        let mut offline = policy.config().clone();
        offline.allow_network = false;
        fetch.url = Some("https://example.com/".into());
        assert!(matches!(
            SecurityPolicy::new(offline).evaluate(&fetch),
            PolicyDecision::Deny { .. }
        ));
    }
}
