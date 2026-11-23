//! A stand-in peer server for `--demo`, screenshots and UI tests.
//!
//! It implements the engine's [`PeerHost`] over plain memory: it opens no socket, reads no network
//! and writes no file, so the Settings page and the title bar indicator can be shown and tested
//! without a listener. It starts **off**, like the real server, and the real engine's rules (the
//! UI-only precheck for the tools that turn it on) apply on top of it, because the tools are the
//! engine's. Tests drive "the other side" through [`DemoPeerHost::ask_to_pair`] and
//! [`DemoPeerHost::set_connected`].

use std::sync::{Arc, Mutex, MutexGuard};

use csm_engine::{
    AuditEvent, DeviceInfo, EnableRequest, NetInterface, PairAnswer, PairingStarted, PeerHost, PeerStatus, PendingPair, Revoke, ToolError,
};

#[derive(Default)]
struct State {
    enabled: bool,
    listening: Option<String>,
    pairing_open: bool,
    pending: Option<(String, String)>,
    devices: Vec<DeviceInfo>,
    /// What the engine asked of the host, "enable 10.8.0.2:47300 by ui", for tests.
    calls: Vec<String>,
    audit: Vec<AuditEvent>,
    next_id: u32,
}

/// The demo host. Cloning shares the state.
#[derive(Clone)]
pub struct DemoPeerHost {
    st: Arc<Mutex<State>>,
    now_secs: u64,
}

impl DemoPeerHost {
    /// A host that is off, with no devices. `now_secs` stamps the devices it pairs.
    pub fn new(now_secs: u64) -> Self {
        Self { st: Arc::new(Mutex::new(State::default())), now_secs }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A device that is already paired (a previous run), `age_secs` ago.
    pub fn with_device(self, name: &str, age_secs: u64) -> Self {
        {
            let mut s = self.st();
            s.next_id += 1;
            let id = format!("DEMODEVICE{:02}", s.next_id);
            s.devices.push(DeviceInfo {
                id,
                name: name.into(),
                level: "full".into(),
                paired_at: self.now_secs.saturating_sub(age_secs),
                last_seen: None,
                connected: false,
            });
        }
        self
    }

    /// The other side connected and finished its handshake: it now waits for Allow or Deny.
    pub fn ask_to_pair(&self, device_name: &str, check_code: &str) {
        self.st().pending = Some((device_name.into(), check_code.into()));
    }

    /// A paired device connects or disconnects.
    pub fn set_connected(&self, name: &str, connected: bool) {
        let now = self.now_secs;
        for d in self.st().devices.iter_mut().filter(|d| d.name == name) {
            d.connected = connected;
            d.last_seen = Some(now);
        }
    }

    /// The calls the engine made on the host, oldest first ("enable 10.8.0.2:47300 by ui").
    pub fn calls(&self) -> Vec<String> {
        self.st().calls.clone()
    }

    /// How many devices are paired.
    pub fn devices_len(&self) -> usize {
        self.st().devices.len()
    }

    pub fn audit_log(&self) -> Vec<AuditEvent> {
        self.st().audit.clone()
    }

    fn status_of(s: &State) -> PeerStatus {
        PeerStatus {
            available: true,
            enabled: s.enabled,
            listening: s.listening.clone(),
            waiting: None,
            pairing_open_secs: s.pairing_open.then_some(600),
            pending_pair: s.pending.as_ref().map(|(n, c)| PendingPair { device_name: n.clone(), check_code: c.clone(), expires_in_secs: 600 }),
            devices: s.devices.len(),
            connected: s.devices.iter().filter(|d| d.connected).count(),
            private_files_restricted: true,
        }
    }
}

/// A link with the shape and length of a real one (`csm://pair#` and base64url), made of nothing.
fn demo_link() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut x: u32 = 0x9E37_79B9;
    let body: String = (0..256)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            char::from(ALPHABET[((x >> 24) & 63) as usize])
        })
        .collect();
    format!("csm://pair#{body}")
}

impl PeerHost for DemoPeerHost {
    fn status(&self) -> PeerStatus {
        Self::status_of(&self.st())
    }

    fn enable(&self, req: EnableRequest, by: &str) -> Result<PeerStatus, ToolError> {
        let mut s = self.st();
        s.calls.push(format!("enable {} by {by}", req.addr));
        s.enabled = true;
        s.listening = Some(req.addr.to_string());
        Ok(Self::status_of(&s))
    }

    fn disable(&self, by: &str) -> PeerStatus {
        let mut s = self.st();
        s.calls.push(format!("disable by {by}"));
        s.enabled = false;
        s.listening = None;
        s.pairing_open = false;
        s.pending = None;
        for d in &mut s.devices {
            d.connected = false;
        }
        Self::status_of(&s)
    }

    fn pair_start(&self, by: &str) -> Result<PairingStarted, ToolError> {
        let mut s = self.st();
        s.calls.push(format!("pair_start by {by}"));
        let Some(at) = s.listening.clone() else { return Err(ToolError::Refused("turn remote access on first".into())) };
        s.pairing_open = true;
        Ok(PairingStarted { link: demo_link(), address: format!("tcp://{at}"), expires_in_secs: 600 })
    }

    fn pair_answer(&self, answer: PairAnswer, by: &str) -> Result<PeerStatus, ToolError> {
        let mut s = self.st();
        s.calls.push(format!("pair_answer allow={} by {by}", answer.allow));
        let Some((name, code)) = s.pending.clone() else { return Err(ToolError::NotFound("no device is waiting to pair".into())) };
        if answer.allow {
            if answer.check_code.as_deref() != Some(code.as_str()) {
                return Err(ToolError::Refused("that is not the check code; compare the four digits on both screens".into()));
            }
            s.next_id += 1;
            let id = format!("DEMODEVICE{:02}", s.next_id);
            s.devices.push(DeviceInfo { id, name, level: answer.level, paired_at: self.now_secs, last_seen: None, connected: false });
        }
        s.pending = None;
        s.pairing_open = false;
        Ok(Self::status_of(&s))
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        self.st().devices.clone()
    }

    fn revoke(&self, target: Revoke, by: &str) -> Result<usize, ToolError> {
        let mut s = self.st();
        s.calls.push(format!("revoke {target:?} by {by}"));
        let before = s.devices.len();
        match target {
            Revoke::All => s.devices.clear(),
            Revoke::One(id) => s.devices.retain(|d| d.id != id),
        }
        Ok(before - s.devices.len())
    }

    fn audit(&self, event: AuditEvent) {
        self.st().audit.push(event);
    }

    fn interfaces(&self) -> Vec<NetInterface> {
        let iface = |name: &str, address: &str, network: &str, wireguard: bool, loopback: bool| NetInterface {
            name: name.into(),
            address: address.into(),
            network: Some(network.into()),
            wireguard,
            loopback,
        };
        vec![
            iface("wg", "10.8.0.2", "10.8.0.0/24", true, false),
            iface("Ethernet", "192.168.1.20", "192.168.1.0/24", false, false),
            iface("Loopback Pseudo-Interface 1", "127.0.0.1", "127.0.0.0/8", false, true),
        ]
    }

    fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_starts_off_and_a_wrong_code_does_not_pair() {
        let h = DemoPeerHost::new(1_000);
        assert!(!h.status().enabled && h.status().listening.is_none());
        h.enable(EnableRequest { addr: "10.8.0.2:47300".parse().unwrap(), allow_all_interfaces: false }, "ui").unwrap();
        h.ask_to_pair("Laptop", "4821");
        let wrong = PairAnswer { allow: true, check_code: Some("1111".into()), level: "full".into() };
        assert!(h.pair_answer(wrong, "ui").is_err());
        assert!(h.devices().is_empty());
        let right = PairAnswer { allow: true, check_code: Some("4821".into()), level: "full".into() };
        assert_eq!(h.pair_answer(right, "ui").unwrap().devices, 1);
    }

    #[test]
    fn the_demo_link_has_the_shape_of_a_real_one() {
        let l = demo_link();
        assert!(l.starts_with("csm://pair#") && l.len() > 250 && l.len() < 300, "{}", l.len());
    }
}
