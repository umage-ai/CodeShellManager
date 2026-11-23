//! Configuration: a TOML file (`--config` / `CSM_RELAY_CONFIG`) with environment overrides
//! (`CSM_RELAY_*`), resolved into [`Settings`]. Nothing here touches the network.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use csm_remote_proto::ids::{ct_eq, hex, is_valid_host_id, sha256, unhex};
use csm_remote_proto::limits as L;
use serde::Deserialize;

/// The push services a relay will POST to unless the operator says otherwise. A leading `*.`
/// matches any subdomain (not the bare domain).
pub const DEFAULT_PUSH_HOSTS: &[&str] = &["*.push.apple.com", "fcm.googleapis.com", "updates.push.services.mozilla.com", "*.notify.windows.com"];
const MIN_ENROL_TOKEN_CHARS: usize = 16;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Address to listen on. Plain HTTP/WS: put a TLS proxy (Caddy) in front.
    pub bind: String,
    /// Take the client address from the last `X-Forwarded-For` entry (set this behind a proxy
    /// that adds it; never when clients can reach the relay directly).
    pub trust_forwarded_for: bool,
    /// Browser origins allowed to open `/v1/client` (the phone app's origin). Empty = any. Defence
    /// in depth, not security: a non-browser client sends any Origin it likes.
    pub allowed_origins: Vec<String>,
    /// `text` or `json`.
    pub log_format: String,
    pub access: AccessConfig,
    pub limits: LimitsConfig,
    pub push: PushConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".to_owned(),
            trust_forwarded_for: false,
            allowed_origins: Vec::new(),
            log_format: "text".to_owned(),
            access: AccessConfig::default(),
            limits: LimitsConfig::default(),
            push: PushConfig::default(),
        }
    }
}

/// Who may connect as a host. Closed unless configured: see `README.md`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    /// Admit every host (a relay for yourself on a private network, or a public free relay).
    pub open: bool,
    /// Admitted host ids.
    pub allowed_hosts: Vec<String>,
    /// A file with one admitted host id per line (`#` comments). Re-read at every host hello, so
    /// adding a host needs no restart.
    pub allowed_hosts_file: Option<PathBuf>,
    /// Enrolment tokens: a host that presents one in its hello is admitted, whatever its id. Each
    /// entry is the secret itself (at least 16 characters) or `sha256:<hex of SHA-256(secret)>`.
    pub enrol_tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_hosts: usize,
    pub max_channels_per_host: usize,
    pub max_allow_entries: usize,
    pub max_sockets_per_ip: usize,
    pub new_connections_per_ip_per_minute: usize,
    pub frames_per_second_per_channel: u32,
    pub bytes_per_second_per_channel: u32,
    pub host_silence_secs: u64,
    pub client_idle_secs: u64,
    pub hello_timeout_secs: u64,
    pub max_pairing_attempts: u8,
    pub max_pairing_secs: u32,
    pub max_frame_bytes: usize,
    /// CSM peer channels (the terminal streams): their own caps, higher than the phone's.
    pub peer_max_channels_per_host: usize,
    pub peer_frames_per_second_per_channel: u32,
    pub peer_bytes_per_second_per_channel: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_hosts: L::MAX_HOSTS,
            max_channels_per_host: L::MAX_CHANNELS_PER_HOST,
            max_allow_entries: L::MAX_ALLOW_ENTRIES,
            max_sockets_per_ip: L::MAX_SOCKETS_PER_IP,
            new_connections_per_ip_per_minute: L::MAX_NEW_CONNECTIONS_PER_IP_PER_MINUTE,
            frames_per_second_per_channel: L::FRAMES_PER_SECOND_PER_CHANNEL,
            bytes_per_second_per_channel: L::BYTES_PER_SECOND_PER_CHANNEL,
            host_silence_secs: L::HOST_SILENCE_SECS,
            client_idle_secs: L::CLIENT_IDLE_SECS,
            hello_timeout_secs: L::HELLO_TIMEOUT_SECS,
            max_pairing_attempts: L::MAX_PAIRING_ATTEMPTS,
            max_pairing_secs: L::PAIRING_SLOT_MAX_SECS,
            max_frame_bytes: L::MAX_FRAME_BYTES,
            peer_max_channels_per_host: L::PEER_MAX_CHANNELS_PER_HOST,
            peer_frames_per_second_per_channel: L::PEER_FRAMES_PER_SECOND_PER_CHANNEL,
            peer_bytes_per_second_per_channel: L::PEER_BYTES_PER_SECOND_PER_CHANNEL,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PushConfig {
    pub enabled: bool,
    /// The VAPID private key (base64url of the 32-byte P-256 scalar). Created on first run if
    /// missing. Keep it: every phone's push subscription is bound to the matching public key.
    pub vapid_key_file: PathBuf,
    /// The VAPID `sub` claim: `mailto:` or `https:` contact for the push services.
    pub vapid_subject: String,
    /// Push service hosts the relay may POST to (see [`DEFAULT_PUSH_HOSTS`]).
    pub allowed_hosts: Vec<String>,
    pub per_host_per_hour: usize,
    pub per_endpoint_per_hour: usize,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            vapid_key_file: PathBuf::from("vapid.key"),
            vapid_subject: "mailto:admin@example.invalid".to_owned(),
            allowed_hosts: DEFAULT_PUSH_HOSTS.iter().map(|s| (*s).to_owned()).collect(),
            per_host_per_hour: L::PUSHES_PER_HOST_PER_HOUR,
            per_endpoint_per_hour: L::PUSHES_PER_ENDPOINT_PER_HOUR,
        }
    }
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(msg.into()))
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
}

fn truthy(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError(format!("config file: {e}")))
    }

    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError(format!("cannot read {}: {e}", path.display())))?;
        Self::from_toml(&text)
    }

    /// Environment overrides (`CSM_RELAY_*`), through a lookup function so tests need no real env.
    pub fn apply_env(&mut self, get: &dyn Fn(&str) -> Option<String>) {
        if let Some(v) = get("CSM_RELAY_BIND") {
            self.bind = v;
        }
        if let Some(v) = get("CSM_RELAY_TRUST_FORWARDED_FOR") {
            self.trust_forwarded_for = truthy(&v);
        }
        if let Some(v) = get("CSM_RELAY_ALLOWED_ORIGINS") {
            self.allowed_origins = split_list(&v);
        }
        if let Some(v) = get("CSM_RELAY_LOG_FORMAT") {
            self.log_format = v;
        }
        if let Some(v) = get("CSM_RELAY_OPEN") {
            self.access.open = truthy(&v);
        }
        if let Some(v) = get("CSM_RELAY_ALLOWED_HOSTS") {
            self.access.allowed_hosts = split_list(&v);
        }
        if let Some(v) = get("CSM_RELAY_ALLOWED_HOSTS_FILE") {
            self.access.allowed_hosts_file = Some(PathBuf::from(v));
        }
        if let Some(v) = get("CSM_RELAY_ENROL_TOKENS") {
            self.access.enrol_tokens = split_list(&v);
        }
        if let Some(v) = get("CSM_RELAY_PUSH") {
            self.push.enabled = truthy(&v);
        }
        if let Some(v) = get("CSM_RELAY_VAPID_KEY_FILE") {
            self.push.vapid_key_file = PathBuf::from(v);
        }
        if let Some(v) = get("CSM_RELAY_VAPID_SUBJECT") {
            self.push.vapid_subject = v;
        }
        if let Some(v) = get("CSM_RELAY_PUSH_ALLOWED_HOSTS") {
            self.push.allowed_hosts = split_list(&v);
        }
        if let Some(n) = get("CSM_RELAY_MAX_HOSTS").and_then(|v| v.trim().parse().ok()) {
            self.limits.max_hosts = n;
        }
    }

    /// Checks everything and resolves it. Refuses to start in a state that admits no host at all
    /// (closed, with nobody listed): that is almost certainly a mistake.
    pub fn into_settings(self) -> Result<Settings, ConfigError> {
        let bind: SocketAddr = self.bind.parse().map_err(|e| ConfigError(format!("bind `{}`: {e}", self.bind)))?;
        let json_logs = match self.log_format.as_str() {
            "text" => false,
            "json" => true,
            other => return err(format!("log_format `{other}`: use `text` or `json`")),
        };
        let a = &self.access;
        let mut hosts = HashSet::new();
        for h in &a.allowed_hosts {
            if !is_valid_host_id(h) {
                return err(format!("access.allowed_hosts: `{h}` is not a host id (26 characters, a-z and 2-7)"));
            }
            hosts.insert(h.clone());
        }
        let mut token_hashes = Vec::new();
        for t in &a.enrol_tokens {
            token_hashes.push(parse_enrol_token(t)?);
        }
        if !a.open && hosts.is_empty() && a.allowed_hosts_file.is_none() && token_hashes.is_empty() {
            return err(
                "no host could ever connect: set access.open = true, or list access.allowed_hosts / allowed_hosts_file / enrol_tokens (see README.md)",
            );
        }
        let l = &self.limits;
        if l.max_frame_bytes < 1024 || l.max_pairing_secs == 0 || l.max_channels_per_host == 0 {
            return err("limits: max_frame_bytes must be at least 1024 and max_pairing_secs / max_channels_per_host above 0");
        }
        let p = &self.push;
        if p.enabled && !(p.vapid_subject.starts_with("mailto:") || p.vapid_subject.starts_with("https://")) {
            return err("push.vapid_subject must start with mailto: or https://");
        }
        let origins = self.allowed_origins.iter().map(|o| o.trim_end_matches('/').to_ascii_lowercase()).collect();
        Ok(Settings {
            bind,
            trust_forwarded_for: self.trust_forwarded_for,
            allowed_origins: origins,
            json_logs,
            access: Access { open: a.open, hosts, hosts_file: a.allowed_hosts_file.clone(), token_hashes },
            limits: self.limits,
            push: self.push,
        })
    }
}

fn parse_enrol_token(entry: &str) -> Result<[u8; 32], ConfigError> {
    if let Some(h) = entry.strip_prefix("sha256:") {
        return unhex(h).and_then(|b| <[u8; 32]>::try_from(b).ok()).ok_or_else(|| ConfigError("access.enrol_tokens: `sha256:` needs 64 hex characters".to_owned()));
    }
    if entry.chars().count() < MIN_ENROL_TOKEN_CHARS || entry.chars().any(char::is_whitespace) {
        return err(format!("access.enrol_tokens: a token must be at least {MIN_ENROL_TOKEN_CHARS} characters with no spaces (or sha256:<hex>)"));
    }
    Ok(sha256(entry.as_bytes()))
}

/// The resolved configuration the relay runs with.
#[derive(Debug, Clone)]
pub struct Settings {
    pub bind: SocketAddr,
    pub trust_forwarded_for: bool,
    pub allowed_origins: Vec<String>,
    pub json_logs: bool,
    pub access: Access,
    pub limits: LimitsConfig,
    pub push: PushConfig,
}

/// Host access control (the operator's, not the users').
#[derive(Debug, Clone)]
pub struct Access {
    pub open: bool,
    hosts: HashSet<String>,
    hosts_file: Option<PathBuf>,
    token_hashes: Vec<[u8; 32]>,
}

impl Access {
    /// May this host connect? `token` is the enrolment token from its hello, if any.
    pub async fn admits(&self, host_id: &str, token: Option<&str>) -> bool {
        if self.open || self.hosts.contains(host_id) {
            return true;
        }
        if let Some(t) = token {
            let h = sha256(t.as_bytes());
            // Compare against every entry (no early exit) so timing says nothing about which matched.
            let mut hit = false;
            for known in &self.token_hashes {
                hit |= ct_eq(known, &h);
            }
            if hit {
                return true;
            }
        }
        if let Some(path) = &self.hosts_file
            && let Ok(text) = tokio::fs::read_to_string(path).await
        {
            return text.lines().map(|l| l.split('#').next().unwrap_or_default().trim()).any(|l| l == host_id);
        }
        false
    }

    /// A short summary for the startup log (no secrets).
    pub fn describe(&self) -> String {
        if self.open {
            return "open (any host)".to_owned();
        }
        format!(
            "closed: {} listed host(s), hosts file {}, {} enrolment token(s)",
            self.hosts.len(),
            if self.hosts_file.is_some() { "set" } else { "unset" },
            self.token_hashes.len()
        )
    }
}

/// `sha256:<hex>` for an enrolment token, for pasting into a config file.
pub fn enrol_token_entry(secret: &str) -> String {
    format!("sha256:{}", hex(&sha256(secret.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_remote_proto::ids::host_id;

    fn open() -> Config {
        let mut c = Config::default();
        c.access.open = true;
        c
    }

    #[test]
    fn default_config_refuses_to_start_closed_and_empty() {
        let e = Config::default().into_settings().unwrap_err();
        assert!(e.0.contains("no host could ever connect"), "{e}");
        assert!(open().into_settings().is_ok());
    }

    #[test]
    fn toml_parses_and_rejects_unknown_keys() {
        let c = Config::from_toml("bind = \"0.0.0.0:9\"\n[access]\nopen = true\n[limits]\nmax_hosts = 3\n").unwrap();
        let s = c.into_settings().unwrap();
        assert_eq!((s.bind.port(), s.limits.max_hosts), (9, 3));
        assert!(Config::from_toml("bnid = 1").is_err());
        assert!(Config::from_toml("[access]\nopne = true").is_err());
    }

    #[test]
    fn env_overrides_file() {
        let mut c = open();
        let env = |k: &str| match k {
            "CSM_RELAY_BIND" => Some("127.0.0.1:1".to_owned()),
            "CSM_RELAY_OPEN" => Some("0".to_owned()),
            "CSM_RELAY_ENROL_TOKENS" => Some("0123456789abcdef0123, sha256:".to_owned() + &"ab".repeat(32)),
            "CSM_RELAY_ALLOWED_ORIGINS" => Some("https://phone.example/ ,https://b.example".to_owned()),
            _ => None,
        };
        c.apply_env(&env);
        let s = c.into_settings().unwrap();
        assert_eq!(s.bind.port(), 1);
        assert!(!s.access.open);
        assert_eq!(s.access.token_hashes.len(), 2);
        assert_eq!(s.allowed_origins, ["https://phone.example", "https://b.example"]);
    }

    #[test]
    fn bad_values_are_named() {
        let mut c = open();
        c.access.allowed_hosts = vec!["nope".into()];
        assert!(c.into_settings().unwrap_err().0.contains("not a host id"));
        let mut c = open();
        c.access.enrol_tokens = vec!["short".into()];
        assert!(c.into_settings().unwrap_err().0.contains("at least 16"));
        let mut c = open();
        c.bind = "nonsense".into();
        assert!(c.into_settings().unwrap_err().0.contains("bind"));
        let mut c = open();
        c.push.vapid_subject = "admin".into();
        assert!(c.into_settings().unwrap_err().0.contains("vapid_subject"));
    }

    #[tokio::test]
    async fn access_rules() {
        let id = host_id(&[1; 32]);
        let other = host_id(&[2; 32]);
        let mut c = Config::default();
        c.access.allowed_hosts = vec![id.clone()];
        c.access.enrol_tokens = vec!["a-long-enrolment-secret".into()];
        let a = c.into_settings().unwrap().access;
        assert!(a.admits(&id, None).await);
        assert!(!a.admits(&other, None).await);
        assert!(!a.admits(&other, Some("wrong-wrong-wrong-wrong")).await);
        assert!(a.admits(&other, Some("a-long-enrolment-secret")).await);
        assert!(open().into_settings().unwrap().access.admits(&other, None).await);
    }

    #[tokio::test]
    async fn hosts_file_is_reread() {
        let dir = std::env::temp_dir().join(format!("csm-relay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hosts.txt");
        let id = host_id(&[3; 32]);
        std::fs::write(&file, "# hosts\n").unwrap();
        let mut c = Config::default();
        c.access.allowed_hosts_file = Some(file.clone());
        let a = c.into_settings().unwrap().access;
        assert!(!a.admits(&id, None).await);
        std::fs::write(&file, format!("# hosts\n{id}  # Allan's PC\n")).unwrap();
        assert!(a.admits(&id, None).await);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enrol_token_entry_matches_parse() {
        let e = enrol_token_entry("a-long-enrolment-secret");
        assert_eq!(parse_enrol_token(&e).unwrap(), sha256(b"a-long-enrolment-secret"));
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    #[test]
    fn the_example_file_parses_and_needs_an_access_rule() {
        let c = Config::from_toml(include_str!("../relay.example.toml")).unwrap();
        // As shipped nobody is admitted, so the relay refuses to start until the operator chooses.
        assert!(c.clone().into_settings().unwrap_err().0.contains("no host could ever connect"));
        let mut c = c;
        c.access.open = true;
        assert!(c.into_settings().is_ok());
    }
}
