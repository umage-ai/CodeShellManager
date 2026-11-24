//! Web Push forwarding. The add-on encrypts the payload (RFC 8291); the relay only adds the VAPID
//! `Authorization` header (RFC 8292) and POSTs the opaque body to a push service on the allow list.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use csm_remote_proto::ids::{b64_decode, b64_encode};
use csm_remote_proto::relay::Urgency;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};

const JWT_LIFETIME_SECS: u64 = 12 * 3600;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct PushError(pub String);

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PushError {}

/// The relay's VAPID key pair.
pub struct Vapid {
    key: SigningKey,
    subject: String,
}

impl Vapid {
    pub fn new(key: SigningKey, subject: &str) -> Self {
        Self { key, subject: subject.to_owned() }
    }

    /// Loads the key from `path`, or creates it (random, mode 0600 on Unix) when the file is missing.
    pub fn load_or_create(path: &Path, subject: &str) -> Result<Self, PushError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let raw = b64_decode(text.trim()).ok_or_else(|| PushError(format!("{}: not base64url", path.display())))?;
                let key = SigningKey::from_slice(&raw).map_err(|_| PushError(format!("{}: not a P-256 private key", path.display())))?;
                Ok(Self::new(key, subject))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = random_key()?;
                write_private(path, &b64_encode(&key.to_bytes()))?;
                Ok(Self::new(key, subject))
            }
            Err(e) => Err(PushError(format!("cannot read {}: {e}", path.display()))),
        }
    }

    /// The uncompressed public point (65 bytes), base64url: the `applicationServerKey` a phone
    /// subscribes with, and the `k=` of the Authorization header.
    pub fn public_key(&self) -> String {
        b64_encode(self.key.verifying_key().to_sec1_point(false).as_bytes())
    }

    /// An ES256 JWT for `aud` (the push service origin), valid 12 hours from `now_secs`.
    pub fn jwt(&self, aud: &str, now_secs: u64) -> String {
        let header = b64_encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({"aud": aud, "exp": now_secs + JWT_LIFETIME_SECS, "sub": self.subject});
        let signing_input = format!("{header}.{}", b64_encode(claims.to_string().as_bytes()));
        let sig: Signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", b64_encode(&sig.to_bytes()))
    }

    /// The `Authorization` header value.
    pub fn authorization(&self, aud: &str, now_secs: u64) -> String {
        format!("vapid t={}, k={}", self.jwt(aud, now_secs), self.public_key())
    }
}

fn random_key() -> Result<SigningKey, PushError> {
    for _ in 0..16 {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(|e| PushError(format!("no randomness: {e}")))?;
        if let Ok(k) = SigningKey::from_slice(&raw) {
            return Ok(k);
        }
    }
    Err(PushError("could not generate a VAPID key".to_owned()))
}

fn write_private(path: &Path, text: &str) -> Result<(), PushError> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| PushError(format!("cannot create {}: {e}", path.display())))?;
    f.write_all(text.as_bytes()).map_err(|e| PushError(format!("cannot write {}: {e}", path.display())))
}

/// The host part of an `https://` endpoint, lowercase; `None` for anything else. The rule is shared
/// with the add-on (`csm_remote_proto::endpoint`), so both refuse the same endpoints.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    csm_remote_proto::endpoint::push_endpoint_host(endpoint)
}

/// Is the endpoint's host on the allow list? `*.example.com` matches subdomains only.
pub fn endpoint_allowed(endpoint: &str, allowed: &[String]) -> bool {
    let Some(host) = endpoint_host(endpoint) else { return false };
    allowed.iter().any(|pat| match pat.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes().get(host.len() - suffix.len() - 1) == Some(&b'.'),
        None => host == pat.to_ascii_lowercase(),
    })
}

/// One push to deliver.
#[derive(Debug, Clone)]
pub struct PushJob {
    pub endpoint: String,
    pub ttl: u32,
    pub urgency: Option<Urgency>,
    pub topic: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOutcome {
    /// The service's HTTP status when it answered.
    pub status: Option<u16>,
    /// 2xx.
    pub ok: bool,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Delivers a push. The real one POSTs over HTTPS; tests inject a fake.
pub trait PushSender: Send + Sync {
    fn send(&self, job: PushJob) -> BoxFuture<'_, PushOutcome>;
    /// The VAPID public key phones subscribe with; `None` disables `GET /v1/vapid`.
    fn public_key(&self) -> Option<String>;
}

/// POSTs with `ureq` (blocking, on the blocking pool) over rustls.
pub struct HttpPushSender {
    vapid: std::sync::Arc<Vapid>,
    agent: ureq::Agent,
}

impl HttpPushSender {
    pub fn new(vapid: Vapid) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(HTTP_TIMEOUT)).http_status_as_error(false).max_redirects(0).build().into();
        Self { vapid: std::sync::Arc::new(vapid), agent }
    }
}

impl PushSender for HttpPushSender {
    fn send(&self, job: PushJob) -> BoxFuture<'_, PushOutcome> {
        let vapid = self.vapid.clone();
        let agent = self.agent.clone();
        Box::pin(async move {
            let failed = PushOutcome { status: None, ok: false };
            let Some(host) = endpoint_host(&job.endpoint) else { return failed };
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let auth = vapid.authorization(&format!("https://{host}"), now);
            let task = tokio::task::spawn_blocking(move || {
                let mut req = agent
                    .post(&job.endpoint)
                    .header("Authorization", &auth)
                    .header("TTL", &job.ttl.to_string())
                    .header("Content-Encoding", "aes128gcm")
                    .header("Content-Type", "application/octet-stream");
                if let Some(u) = job.urgency {
                    req = req.header("Urgency", u.as_str());
                }
                if let Some(t) = &job.topic {
                    req = req.header("Topic", t);
                }
                req.send(&job.body[..]).map(|r| r.status().as_u16())
            });
            match task.await {
                Ok(Ok(status)) => PushOutcome { status: Some(status), ok: (200..300).contains(&status) },
                _ => failed,
            }
        })
    }

    fn public_key(&self) -> Option<String> {
        Some(self.vapid.public_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    fn allowed() -> Vec<String> {
        crate::config::DEFAULT_PUSH_HOSTS.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn endpoint_allow_list() {
        let a = allowed();
        assert!(endpoint_allowed("https://fcm.googleapis.com/fcm/send/abc", &a));
        assert!(endpoint_allowed("https://web.push.apple.com/QL...", &a));
        assert!(endpoint_allowed("https://updates.push.services.mozilla.com/wpush/v2/x", &a));
        assert!(endpoint_allowed("https://wns2-par02p.notify.windows.com/?token=x", &a));
        assert!(endpoint_allowed("https://FCM.googleapis.com:443/x", &a));
        // Not on the list, or not a plain https endpoint.
        assert!(!endpoint_allowed("https://evil.example/x", &a));
        assert!(!endpoint_allowed("http://fcm.googleapis.com/x", &a));
        assert!(!endpoint_allowed("https://fcm.googleapis.com.evil.example/x", &a));
        assert!(!endpoint_allowed("https://evilfcm.googleapis.com/x", &a));
        assert!(!endpoint_allowed("https://fcm.googleapis.com@evil.example/x", &a));
        assert!(!endpoint_allowed("https://fcm.googleapis.com:8443/x", &a));
        assert!(!endpoint_allowed("https://push.apple.com/x", &a), "wildcard needs a subdomain");
        assert!(!endpoint_allowed("https://127.0.0.1/x", &a));
        assert!(!endpoint_allowed("https://localhost/x", &a));
        assert!(!endpoint_allowed("", &a));
    }

    #[test]
    fn jwt_is_a_valid_es256_signature_over_the_expected_claims() {
        let key = random_key().unwrap();
        let v = Vapid::new(key.clone(), "mailto:ops@example.test");
        let jwt = v.jwt("https://fcm.googleapis.com", 1_000);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value = serde_json::from_slice(&b64_decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_000 + JWT_LIFETIME_SECS);
        assert_eq!(claims["sub"], "mailto:ops@example.test");
        let sig = Signature::from_slice(&b64_decode(parts[2]).unwrap()).unwrap();
        let vk = VerifyingKey::from_sec1_bytes(&b64_decode(&v.public_key()).unwrap()).unwrap();
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
        assert_eq!(b64_decode(&v.public_key()).unwrap().len(), 65);
        assert!(v.authorization("https://x", 1).starts_with("vapid t="));
    }

    #[test]
    fn key_file_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("csm-relay-vapid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("vapid.key");
        let a = Vapid::load_or_create(&file, "mailto:a@example.test").unwrap();
        let b = Vapid::load_or_create(&file, "mailto:a@example.test").unwrap();
        assert_eq!(a.public_key(), b.public_key());
        std::fs::write(&file, "garbage!").unwrap();
        assert!(Vapid::load_or_create(&file, "mailto:a@example.test").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
