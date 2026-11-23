//! The laptop's side of the peer link as the engine sees it ([`csm_engine::PeerClientHost`]):
//! pairing with another CodeShellManager from its link, and the list of the hosts this app has.
//!
//! **Nothing listens.** [`ClientPeer::new`] touches no disk and no network. The only thing that
//! dials out is a pairing the person started in the app (`peer_client_pair`, refused for every
//! other caller by the engine) and, later, a connection to a host already in the list.
//!
//! Files live in one folder the app gives (`<state folder>/peer-client/`), limited to the current
//! user: `device.key` (this app's own key pair, created on the first pairing and never replaced
//! once it exists; a damaged file is an error) and `hosts.json` ([`crate::hosts`]).
//!
//! A pairing runs on its own thread (the engine thread never waits on the network). Its progress
//! is one status value, which the tools read: connecting, the check code to compare, paired,
//! failed (with why), cancelled.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use csm_engine::{ClientPairStatus, HostInfo, PairState, PeerClientHost, ToolError};
use csm_peer_proto::msg::valid_name;
use csm_peer_proto::pairing::PeerOffer;

use crate::client::{self, ClientConfig, ClientError, PairedHost};
use crate::fsperm;
use crate::hosts::HostStore;
use crate::identity::Identity;
use crate::unix_secs;

/// The file that holds this app's device key, in the client folder (`peer-client/`).
pub const DEVICE_KEY: &str = "device.key";
/// The file that lists the paired hosts, in the client folder.
pub const HOSTS_FILE: &str = "hosts.json";

/// The client. Cloning shares it.
#[derive(Clone)]
pub struct ClientPeer {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    device_name: String,
    st: Mutex<St>,
    /// Serialises the read-modify-write of `hosts.json` (one writer at a time).
    hosts: Mutex<()>,
}

#[derive(Default)]
struct St {
    status: ClientPairStatus,
    /// The cancel flag of the pairing thread that is running, if one is.
    running: Option<Arc<AtomicBool>>,
}

impl ClientPeer {
    /// A client whose files will live in `dir`. Does nothing else.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir,
                device_name: machine_name(),
                st: Mutex::new(St::default()),
                hosts: Mutex::new(()),
            }),
        }
    }

    /// The device name the host lists this app under (the computer's name).
    pub fn device_name(&self) -> &str {
        &self.inner.device_name
    }
}

/// The computer's name when it is a valid name, else a fixed one.
fn machine_name() -> String {
    let raw = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let name = raw.trim().to_owned();
    if valid_name(&name) { name } else { "CodeShellManager".to_owned() }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, f: impl FnOnce(&mut ClientPairStatus)) {
        f(&mut self.lock().status);
    }

    /// The pairing thread ends: its status is final and no thread is running any more.
    fn finish(&self, status: ClientPairStatus) {
        let mut st = self.lock();
        st.status = status;
        st.running = None;
    }

    fn hosts_path(&self) -> PathBuf {
        self.dir.join(HOSTS_FILE)
    }

    fn store_host(&self, name: &str, paired: &PairedHost) -> Result<(), String> {
        let _guard = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        fsperm::ensure_private_dir(&self.dir).map_err(|e| format!("could not make the folder for remote access: {e}"))?;
        let mut store = HostStore::load(&self.hosts_path()).map_err(|e| e.to_string())?;
        store.add(name, &paired.address, &paired.host_public, unix_secs()).map(|_| ()).map_err(|e| format!("could not save the host: {e}"))
    }

    fn run_pair(&self, offer: PeerOffer, name: String, cancel: Arc<AtomicBool>) {
        let outcome = self.pair_once(&offer, &name, &cancel);
        let host_id = offer.h.clone();
        let status = match outcome {
            Ok(()) => ClientPairStatus { state: PairState::Paired, host_name: Some(name), host_id: Some(host_id), check_code: None, message: None },
            Err(ClientError::Cancelled) => ClientPairStatus { state: PairState::Cancelled, host_name: Some(name), host_id: Some(host_id), check_code: None, message: None },
            Err(e) => ClientPairStatus { state: PairState::Failed, host_name: Some(name), host_id: Some(host_id), check_code: None, message: Some(e.to_string()) },
        };
        self.finish(status);
    }

    /// The pairing itself. A storage problem after the host allowed us is reported as a failure:
    /// the host then has a device it will not see again until pairing is repeated.
    fn pair_once(&self, offer: &PeerOffer, name: &str, cancel: &AtomicBool) -> Result<(), ClientError> {
        fsperm::ensure_private_dir(&self.dir).map_err(|e| ClientError::Link(format!("could not make the folder for remote access: {e}")))?;
        let me = Identity::load_or_create(&self.dir.join(DEVICE_KEY)).map_err(|e| ClientError::Link(format!("this app's device key: {e}")))?;
        let cfg = ClientConfig::new(&self.device_name);
        let paired = client::pair_cancellable(offer, &me, &cfg, cancel, &mut |code| {
            self.update(|s| {
                s.state = PairState::CheckCode;
                s.check_code = Some(code.to_owned());
            });
        })?;
        self.store_host(name, &paired).map_err(ClientError::Link)
    }
}

impl PeerClientHost for ClientPeer {
    fn status(&self) -> ClientPairStatus {
        self.inner.lock().status.clone()
    }

    fn pair_start(&self, link: &str, name: Option<&str>) -> Result<ClientPairStatus, ToolError> {
        let offer = PeerOffer::decode(link).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let addr = offer.addr().map_or_else(|| offer.a.clone(), |a| a.to_url());
        let name = match name {
            Some(n) if valid_name(n) => n.trim().to_owned(),
            Some(_) => return Err(ToolError::InvalidParams("the host's name must be 1 to 64 characters with no control characters".into())),
            None => addr.clone(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let started = {
            let mut st = self.inner.lock();
            if st.running.is_some() {
                return Err(ToolError::Refused("a pairing is already running: cancel it, or wait for it to finish".into()));
            }
            st.running = Some(cancel.clone());
            st.status = ClientPairStatus { state: PairState::Connecting, host_name: Some(name.clone()), host_id: Some(offer.h.clone()), check_code: None, message: None };
            st.status.clone()
        };
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new().name("csm-peer-pair".into()).spawn(move || inner.run_pair(offer, name, cancel));
        if let Err(e) = spawned {
            self.inner.finish(ClientPairStatus { state: PairState::Failed, message: Some(format!("could not start the pairing thread: {e}")), ..ClientPairStatus::default() });
            return Err(ToolError::Failed(format!("could not start the pairing thread: {e}")));
        }
        Ok(started)
    }

    fn pair_cancel(&self) -> ClientPairStatus {
        let st = self.inner.lock();
        if let Some(c) = &st.running {
            c.store(true, Ordering::Release);
        }
        st.status.clone()
    }

    fn hosts(&self) -> Result<Vec<HostInfo>, ToolError> {
        let _guard = self.inner.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        let store = HostStore::load(&self.inner.hosts_path()).map_err(|e| ToolError::Failed(e.to_string()))?;
        Ok(store
            .list()
            .iter()
            .map(|h| HostInfo { host_id: h.host_id.clone(), name: h.name.clone(), address: h.address.clone(), paired_at: h.paired_at, last_connected: h.last_connected })
            .collect())
    }

    fn forget(&self, host_id: &str) -> Result<bool, ToolError> {
        let _guard = self.inner.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        let mut store = HostStore::load(&self.inner.hosts_path()).map_err(|e| ToolError::Failed(e.to_string()))?;
        store.remove(host_id).map_err(|e| ToolError::Failed(format!("could not save the host list: {e}")))
    }

    fn shutdown(&self) {
        if let Some(c) = &self.inner.lock().running {
            c.store(true, Ordering::Release);
        }
    }
}
