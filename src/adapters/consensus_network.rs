//! Tokio TCP transport for the peer state machine.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex as StdMutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, lookup_host},
    sync::{Mutex, broadcast, mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

use crate::{
    adapters::{
        consensus_local_state::write_atomically,
        consensus_p2p::{
            CompactBlock, Envelope, Hello, MAX_HELLO_PEERS, MAX_RELAY_CANDIDATES,
            MAX_RELAY_TICKETS, MAX_RELAY_TRANSACTIONS, MAX_WIRE_LINE_BYTES, decode_line,
            encode_line,
        },
        consensus_peer::PeerSession,
        consensus_store::ConsensusChainStore,
    },
    app::consensus_node::Node,
    consensus::{HeartbeatBlock, Sidecar, TicketTransaction, Transaction},
};

const MAX_INBOUND_SESSIONS: usize = 128;
/// One host cannot take every inbound slot.
const MAX_INBOUND_SESSIONS_PER_IP: usize = 8;
const MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW: usize = 24;
const INBOUND_ACCEPT_WINDOW: Duration = Duration::from_secs(10);
/// Per resolved address, so one dead address does not use up the budget of the others.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The remote sends Hello after taking its node lock, so this allows for a busy node.
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
/// A Hello carries a few short fields and at most eight peer addresses. Until one arrives a
/// stranger gets this much buffer, not the full line limit meant for blocks.
const MAX_HELLO_LINE_BYTES: usize = 16 * 1024;
/// Longer than any quiet stretch of a live network; an outbound peer simply reconnects.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// A started line must be finished within this, so a peer cannot park up to 8 MiB of unfinished
/// line in memory for the whole idle timeout. A full line still only needs about 28 KiB/s.
const LINE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// For each chunk, so a slow but moving uplink can finish an 8 MiB block batch.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_CHUNK_BYTES: usize = 64 * 1024;
/// Above this many tracked hosts, stale ones are swept. Below it the sweep is not worth its cost.
const MAX_INBOUND_TRACKED_HOSTS: usize = 1_024;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// Errors such as running out of file descriptors return at once, so the accept loop pauses
/// instead of spinning and starving the supervisor's other work.
const ACCEPT_ERROR_DELAY: Duration = Duration::from_millis(100);
/// Refresh peer discovery on long-lived sessions as the connected topology changes.
const PEER_ADVERTISEMENT_INTERVAL: Duration = Duration::from_secs(60);
const OUTBOUND_CHANNEL_CAPACITY: usize = 1_024;
/// Upper bound on outbound peers an operator can configure at runtime.
pub const MAX_PEER_TARGETS: usize = 32;
/// Hello discovery must never consume the operator's configured-peer allowance.
const MAX_DISCOVERED_PEER_TARGETS: usize = 8;

pub type SharedConsensusNode = Arc<Mutex<Node<ConsensusChainStore>>>;

/// The address put in new Hello messages. The HTTP settings endpoint can replace it live without
/// disturbing established TCP sessions.
#[derive(Clone, Default)]
pub struct P2pAdvertisement(Arc<RwLock<Option<PeerAddress>>>);

impl P2pAdvertisement {
    pub fn new(address: Option<PeerAddress>) -> Self {
        Self(Arc::new(RwLock::new(address)))
    }

    pub fn get(&self) -> Option<PeerAddress> {
        self.0
            .read()
            .expect("P2P advertisement lock poisoned")
            .clone()
    }

    pub fn set(&self, address: Option<PeerAddress>) {
        *self.0.write().expect("P2P advertisement lock poisoned") = address;
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConnectedPeer {
    pub address: String,
    pub direction: &'static str,
    pub connected_at: u64,
    pub heartbeat_height: Option<u64>,
    pub tip_hash: Option<String>,
    /// This node is fetching a range of blocks from the peer. Heights alone miss a side branch:
    /// after a restart the node may have mined its own branch as high as the network's.
    pub syncing: bool,
}

/// An outbound peer the node keeps dialling, with its latest connection diagnostics.
#[derive(Clone, Debug, Serialize)]
pub struct PeerTarget {
    pub address: String,
    /// `startup` for `--peer`, `added` for persisted UI peers, `discovered` for Hello endpoints.
    pub source: &'static str,
    pub connected: bool,
    /// The current session received a valid Hello. Only then is the address worth passing on:
    /// a bare TCP accept says nothing about what listens there.
    #[serde(skip)]
    pub verified: bool,
    pub last_attempt: Option<u64>,
    pub last_error: Option<String>,
}

/// An outbound peer as `IP:port` or `host:port`. A host name resolves on every dial, so a bootstrap
/// node can move without a new release.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerAddress(String);

impl PeerAddress {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for PeerAddress {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        // Normalized, so two spellings of one peer cannot become two targets.
        let value = value.trim();
        if let Ok(address) = value.parse::<SocketAddr>() {
            return Ok(address.into());
        }
        let valid_host = |host: &str| {
            !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        };
        match value.rsplit_once(':') {
            Some((host, port))
                if valid_host(host) && port.parse::<u16>().is_ok_and(|port| port > 0) =>
            {
                Ok(Self(value.to_ascii_lowercase()))
            }
            _ => bail!("use host:port or IP:port, for example iuna.jhx.app:9444"),
        }
    }
}

impl std::fmt::Display for PeerAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<SocketAddr> for PeerAddress {
    fn from(address: SocketAddr) -> Self {
        Self(address.to_string())
    }
}

#[derive(Default)]
struct InboundLimiter {
    active: usize,
    hosts: BTreeMap<IpAddr, InboundHost>,
}

/// One limit per host: an IPv6 host holds a whole /64, and IPv4-mapped addresses are plain IPv4.
fn inbound_host(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(ip) => {
            let mut octets = ip.octets();
            octets[8..].fill(0);
            IpAddr::from(octets)
        }
        ip => ip,
    }
}

#[derive(Default)]
struct InboundHost {
    active: usize,
    accepted_at: VecDeque<Instant>,
}

/// Frees its inbound slot when the session ends.
struct InboundPermit {
    limiter: Arc<StdMutex<InboundLimiter>>,
    ip: IpAddr,
}

impl Drop for InboundPermit {
    fn drop(&mut self) {
        let mut limiter = self.limiter.lock().expect("inbound limiter lock poisoned");
        limiter.active -= 1;
        if let Some(host) = limiter.hosts.get_mut(&inbound_host(self.ip)) {
            host.active -= 1;
        }
    }
}

impl InboundHost {
    fn expire_accepts(&mut self, now: Instant) {
        while self
            .accepted_at
            .front()
            .is_some_and(|accepted| now.duration_since(*accepted) >= INBOUND_ACCEPT_WINDOW)
        {
            self.accepted_at.pop_front();
        }
    }
}

impl InboundLimiter {
    fn acquire(limiter: &Arc<StdMutex<Self>>, ip: IpAddr) -> Option<InboundPermit> {
        let admitted = limiter
            .lock()
            .expect("inbound limiter lock poisoned")
            .admit(ip, Instant::now());
        admitted.then(|| InboundPermit {
            limiter: Arc::clone(limiter),
            ip,
        })
    }

    fn admit(&mut self, ip: IpAddr, now: Instant) -> bool {
        // Hosts without a session age out, so scanning the internet cannot grow this map.
        if self.hosts.len() >= MAX_INBOUND_TRACKED_HOSTS {
            self.hosts.retain(|_, host| {
                host.expire_accepts(now);
                host.active > 0 || !host.accepted_at.is_empty()
            });
        }
        if self.active >= MAX_INBOUND_SESSIONS {
            return false;
        }
        let host = self.hosts.entry(inbound_host(ip)).or_default();
        host.expire_accepts(now);
        if host.active >= MAX_INBOUND_SESSIONS_PER_IP
            || host.accepted_at.len() >= MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW
        {
            return false;
        }
        host.active += 1;
        host.accepted_at.push_back(now);
        self.active += 1;
        true
    }
}

enum PeerCommand {
    Add(PeerAddress),
    Remove(PeerAddress),
}

#[derive(Clone, Default)]
pub struct PeerRegistry {
    peers: Arc<RwLock<BTreeMap<String, ConnectedPeer>>>,
    targets: Arc<RwLock<BTreeMap<PeerAddress, PeerTarget>>>,
    mutations: Arc<StdMutex<()>>,
    commands: Arc<RwLock<Option<mpsc::UnboundedSender<PeerCommand>>>>,
    store_path: Arc<RwLock<Option<PathBuf>>>,
    /// The Hello nonce of every open session. A Hello carrying one of them came from this node
    /// itself. A fresh nonce per session keeps peers from linking one node across addresses.
    session_nonces: Arc<StdMutex<HashSet<u64>>>,
    /// Targets whose Hello carried this node's nonce. A node advertising `0.0.0.0` cannot tell its
    /// public address from an endpoint string, so it learns it here and never dials it again.
    own_addresses: Arc<RwLock<BTreeSet<PeerAddress>>>,
    /// The remote socket of each live outbound session and the target that dialled it. A host
    /// name target and the `IP:port` a Hello names for it reach the same node; only one of them
    /// keeps a session.
    outbound_remotes: Arc<RwLock<HashMap<SocketAddr, PeerAddress>>>,
}

/// Reads the runtime-added peers saved by an earlier run. A missing file means none.
/// Saves `peers` as the runtime peers unless a peer list already exists. Unlike `--peer` these are
/// the user's to remove: a removal is saved, and this never adds them again.
/// Seeds the bootstrap peers once, so a user can remove them for good. A saved peer on a bootstrap
/// host with another port is the same node after a port change, so it follows that change.
pub fn seed_added_peers(path: &Path, peers: &[PeerAddress]) -> Result<()> {
    if peers.is_empty() {
        return Ok(());
    }
    let seeded = if path.exists() {
        let saved = load_added_peers(path)?;
        let host = |peer: &PeerAddress| {
            peer.as_str()
                .rsplit_once(':')
                .map(|(host, _)| host.to_owned())
        };
        let mut moved = Vec::new();
        for peer in &saved {
            let current = peers
                .iter()
                .find(|bootstrap| host(bootstrap) == host(peer))
                .unwrap_or(peer);
            if !moved.contains(current) {
                moved.push(current.clone());
            }
        }
        if moved == saved {
            return Ok(());
        }
        moved
    } else {
        peers.to_vec()
    };
    let seeded = seeded.iter().map(PeerAddress::as_str).collect::<Vec<_>>();
    write_atomically(path, &serde_json::to_vec_pretty(&seeded)?).context("writing bootstrap peers")
}

/// An entry that no longer parses, edited by hand or refused by a stricter release, is skipped
/// with a warning instead of keeping the node from starting.
pub fn load_added_peers(path: &Path) -> Result<Vec<PeerAddress>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Vec<String>>(&bytes)
            .context("decoding saved peers")?
            .iter()
            .filter_map(|address| match address.parse() {
                Ok(address) => Some(address),
                Err(error) => {
                    eprintln!("skipping saved peer {address:?}: {error:#}");
                    None
                }
            })
            .take(MAX_PEER_TARGETS)
            .collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("reading saved peers"),
    }
}

/// A session's Hello nonce, registered while the session lives.
struct SessionNonce {
    nonces: Arc<StdMutex<HashSet<u64>>>,
    nonce: u64,
}

impl Drop for SessionNonce {
    fn drop(&mut self) {
        if let Ok(mut nonces) = self.nonces.lock() {
            nonces.remove(&self.nonce);
        }
    }
}

impl PeerRegistry {
    /// A fresh nonce for one session's Hello, known to this node until the guard is dropped.
    fn open_session_nonce(&self) -> SessionNonce {
        let mut nonces = self
            .session_nonces
            .lock()
            .expect("peer registry lock poisoned");
        loop {
            let mut bytes = [0; 8];
            getrandom::getrandom(&mut bytes).expect("operating system randomness is available");
            let nonce = u64::from_le_bytes(bytes);
            if nonces.insert(nonce) {
                return SessionNonce {
                    nonces: Arc::clone(&self.session_nonces),
                    nonce,
                };
            }
        }
    }

    /// Whether a received Hello nonce belongs to one of this node's own open sessions.
    fn is_own_nonce(&self, nonce: u64) -> bool {
        self.session_nonces
            .lock()
            .expect("peer registry lock poisoned")
            .contains(&nonce)
    }

    /// Records that `peer` holds the outbound session to `remote`. A discovered target is refused,
    /// returning the target already connected there, so the pair of nodes keeps one session; a
    /// configured target is always allowed, as the operator asked for it.
    fn claim_outbound_remote(&self, remote: SocketAddr, peer: &PeerAddress) -> Option<PeerAddress> {
        let remote = canonical_socket(remote);
        let discovered = self
            .targets
            .read()
            .expect("peer registry lock poisoned")
            .get(peer)
            .is_some_and(|target| target.source == "discovered");
        let mut remotes = self
            .outbound_remotes
            .write()
            .expect("peer registry lock poisoned");
        match remotes.get(&remote) {
            Some(holder) if holder != peer && discovered => Some(holder.clone()),
            Some(_) => None,
            None => {
                remotes.insert(remote, peer.clone());
                None
            }
        }
    }

    fn release_outbound_remote(&self, remote: SocketAddr, peer: &PeerAddress) {
        let remote = canonical_socket(remote);
        let mut remotes = self
            .outbound_remotes
            .write()
            .expect("peer registry lock poisoned");
        if remotes.get(&remote) == Some(peer) {
            remotes.remove(&remote);
        }
    }

    fn has_outbound_session_to(&self, address: &PeerAddress) -> bool {
        address.as_str().parse::<SocketAddr>().is_ok_and(|address| {
            self.outbound_remotes
                .read()
                .expect("peer registry lock poisoned")
                .contains_key(&canonical_socket(address))
        })
    }

    /// Forgets that `address` reached this node, when the operator adds or removes it again.
    fn forget_own_address(&self, address: &PeerAddress) {
        self.own_addresses
            .write()
            .expect("peer registry lock poisoned")
            .remove(address);
    }

    /// Remembers that the session behind `peer_key` reached this node itself. An outbound target
    /// is then never learned or dialled again; a discovered one also gives up its slot.
    fn mark_own_session(&self, peer_key: &str) {
        if let Some(address) = peer_key
            .strip_prefix("outbound:")
            .and_then(|address| address.parse::<PeerAddress>().ok())
        {
            self.own_addresses
                .write()
                .expect("peer registry lock poisoned")
                .insert(address);
        }
    }

    fn is_own_address(&self, address: &PeerAddress) -> bool {
        self.own_addresses
            .read()
            .expect("peer registry lock poisoned")
            .contains(address)
    }

    pub fn targets(&self) -> Vec<PeerTarget> {
        self.targets
            .read()
            .expect("peer registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn add_target(&self, address: impl Into<PeerAddress>) -> Result<()> {
        let address = address.into();
        let commands = self.command_sender()?;
        // Added on purpose, so it is dialled again even if it once reached this node.
        self.forget_own_address(&address);
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| anyhow::anyhow!("peer mutation lock poisoned"))?;
        {
            let mut targets = self.targets.write().expect("peer registry lock poisoned");
            if targets.contains_key(&address) {
                bail!("{address} is already a configured peer");
            }
            let configured = targets
                .values()
                .filter(|target| target.source != "discovered")
                .count();
            if configured >= MAX_PEER_TARGETS {
                bail!("at most {MAX_PEER_TARGETS} peers can be configured");
            }
            targets.insert(address.clone(), new_target(&address, "added"));
        }
        if let Err(error) = self.persist() {
            self.targets
                .write()
                .expect("peer registry lock poisoned")
                .remove(&address);
            return Err(error);
        }
        let _ = commands.send(PeerCommand::Add(address));
        Ok(())
    }

    /// Learns a peer from a Hello. The node's own advertised address is never learned: a peer
    /// that dialled this node passes it back, and a session with itself would hold two slots.
    fn discover_target(&self, address: PeerAddress, own: Option<&PeerAddress>) {
        if own == Some(&address)
            || self.is_own_address(&address)
            || self.has_outbound_session_to(&address)
        {
            return;
        }
        let Ok(commands) = self.command_sender() else {
            return;
        };
        let mut targets = self.targets.write().expect("peer registry lock poisoned");
        let discovered = targets
            .values()
            .filter(|target| target.source == "discovered")
            .count();
        if targets.contains_key(&address) || discovered >= MAX_DISCOVERED_PEER_TARGETS {
            return;
        }
        targets.insert(address.clone(), new_target(&address, "discovered"));
        drop(targets);
        let _ = commands.send(PeerCommand::Add(address));
    }

    fn discover_hello_peers(&self, hello: &Hello, own: Option<&PeerAddress>) {
        for address in hello.known_peers.iter().take(MAX_HELLO_PEERS) {
            let Ok(address) = address.parse::<SocketAddr>() else {
                continue;
            };
            if address.port() > 0 && ip_is_publicly_discoverable(address.ip()) {
                self.discover_target(address.into(), own);
            }
        }
    }

    /// A discovered peer is kept only while it works. Once a dial or session fails it is
    /// dropped, so its slot goes to a peer another Hello names; a live one comes back that way.
    /// Returns whether the target was dropped.
    fn forget_discovered(&self, address: &PeerAddress) -> bool {
        let mut targets = self.targets.write().expect("peer registry lock poisoned");
        if targets
            .get(address)
            .is_none_or(|target| target.source != "discovered")
        {
            return false;
        }
        targets.remove(address);
        // Queued under the lock: a Hello that names this address again queues its Add after this
        // Remove, so the supervisor dials it anew instead of keeping a target without a task.
        if let Ok(commands) = self.command_sender() {
            let _ = commands.send(PeerCommand::Remove(address.clone()));
        }
        drop(targets);
        true
    }

    fn verify_outbound(&self, peer_key: &str) {
        if let Some(address) = peer_key
            .strip_prefix("outbound:")
            .and_then(|address| address.parse::<PeerAddress>().ok())
        {
            self.update_target(&address, |target| target.verified = true);
        }
    }

    /// Only outbound targets that answered with a valid Hello have a listener worth sharing.
    /// Inbound source ports are ephemeral and must never be advertised as peers.
    fn known_peers_for_hello(&self, peer_key: &str) -> Vec<String> {
        let current_outbound = peer_key.strip_prefix("outbound:");
        self.targets
            .read()
            .expect("peer registry lock poisoned")
            .iter()
            .filter(|(address, target)| {
                target.verified && current_outbound != Some(address.as_str())
            })
            .take(MAX_HELLO_PEERS)
            .map(|(address, _)| address.to_string())
            .collect()
    }

    pub fn remove_target(&self, address: impl Into<PeerAddress>) -> Result<()> {
        let address = address.into();
        let commands = self.command_sender()?;
        self.forget_own_address(&address);
        let _mutation = self
            .mutations
            .lock()
            .map_err(|_| anyhow::anyhow!("peer mutation lock poisoned"))?;
        let removed = self
            .targets
            .write()
            .expect("peer registry lock poisoned")
            .remove(&address);
        let Some(removed) = removed else {
            bail!("{address} is not a configured peer");
        };
        if let Err(error) = self.persist() {
            self.targets
                .write()
                .expect("peer registry lock poisoned")
                .insert(address, removed);
            return Err(error);
        }
        let _ = commands.send(PeerCommand::Remove(address));
        Ok(())
    }

    fn command_sender(&self) -> Result<mpsc::UnboundedSender<PeerCommand>> {
        self.commands
            .read()
            .expect("peer registry lock poisoned")
            .clone()
            .context("the P2P network is not running")
    }

    /// Saves only runtime-added peers; `--peer` arguments stay owned by the command line.
    fn persist(&self) -> Result<()> {
        let Some(path) = self
            .store_path
            .read()
            .expect("peer registry lock poisoned")
            .clone()
        else {
            return Ok(());
        };
        let added = self
            .targets()
            .into_iter()
            .filter(|target| target.source == "added")
            .map(|target| target.address)
            .collect::<Vec<_>>();
        write_atomically(&path, &serde_json::to_vec_pretty(&added)?).context("writing saved peers")
    }

    fn update_target(&self, address: &PeerAddress, update: impl FnOnce(&mut PeerTarget)) {
        if let Some(target) = self
            .targets
            .write()
            .expect("peer registry lock poisoned")
            .get_mut(address)
        {
            update(target);
        }
    }

    pub fn connected(&self) -> Vec<ConnectedPeer> {
        self.peers
            .read()
            .expect("peer registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn connect(&self, key: String, address: impl ToString, direction: &'static str) {
        self.peers
            .write()
            .expect("peer registry lock poisoned")
            .insert(
                key,
                ConnectedPeer {
                    address: address.to_string(),
                    direction,
                    connected_at: unix_time_secs(),
                    heartbeat_height: None,
                    tip_hash: None,
                    syncing: false,
                },
            );
    }

    fn update_hello(&self, key: &str, hello: &crate::adapters::consensus_p2p::Hello) {
        if let Some(peer) = self
            .peers
            .write()
            .expect("peer registry lock poisoned")
            .get_mut(key)
        {
            peer.heartbeat_height = Some(hello.height);
            peer.tip_hash = Some(hex(&hello.tip_hash));
        }
    }

    /// The Hello height ages from the moment it arrives, so every block a peer sends moves its
    /// height on. That keeps the screen's sync progress honest after the handshake.
    fn observe_block(&self, key: &str, height: u64, hash: &[u8; 32]) {
        if let Some(peer) = self
            .peers
            .write()
            .expect("peer registry lock poisoned")
            .get_mut(key)
            .filter(|peer| peer.heartbeat_height.is_none_or(|known| height > known))
        {
            peer.heartbeat_height = Some(height);
            peer.tip_hash = Some(hex(hash));
        }
    }

    fn update_syncing(&self, key: &str, syncing: bool) {
        if let Some(peer) = self
            .peers
            .write()
            .expect("peer registry lock poisoned")
            .get_mut(key)
        {
            peer.syncing = syncing;
        }
    }

    fn disconnect(&self, key: &str) {
        self.peers
            .write()
            .expect("peer registry lock poisoned")
            .remove(key);
    }
}

/// Whether a peer's advertised endpoint is this node's own. Nodes without a configured public
/// address all advertise `0.0.0.0:<port>`, so an unspecified address never identifies anyone.
fn is_own_endpoint(own: Option<&PeerAddress>, advertised: Option<&str>) -> bool {
    let (Some(own), Some(advertised)) = (own, advertised) else {
        return false;
    };
    own.as_str() == advertised
        && own
            .as_str()
            .parse::<SocketAddr>()
            .map_or(true, |address| !address.ip().is_unspecified())
}

fn canonical_socket(address: SocketAddr) -> SocketAddr {
    SocketAddr::new(address.ip().to_canonical(), address.port())
}

fn ip_is_publicly_discoverable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (18..=19).contains(&b))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || [a, b, c, d] == [255, 255, 255, 255])
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = embedded_ipv4(ip) {
                return ip_is_publicly_discoverable(IpAddr::V4(ip));
            }
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xff00) == 0xff00
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                // Teredo hides the real endpoint behind a relay and an obfuscated address.
                || (segments[0] == 0x2001 && segments[1] == 0))
        }
    }
}

/// The IPv4 address an IPv6 address routes to: mapped, compatible, NAT64 (`64:ff9b::/96`) or
/// 6to4 (`2002::/16`). Each must pass the IPv4 rules, or a private network is one prefix away.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(ip) = ip.to_ipv4_mapped() {
        return Some(ip);
    }
    let segments = ip.segments();
    let from = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Some(Ipv4Addr::new(a, b, c, d))
    };
    if segments[0] == 0x2002 {
        return from(segments[1], segments[2]);
    }
    if segments[..6] == [0; 6] || segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return from(segments[6], segments[7]);
    }
    None
}

#[derive(Clone)]
pub struct ConsensusRelay {
    outbound: broadcast::Sender<Envelope>,
}

impl ConsensusRelay {
    pub fn heartbeat(&self, block: &HeartbeatBlock) {
        let _ = self
            .outbound
            .send(Envelope::CompactBlock(Box::new(CompactBlock::from_block(
                block,
            ))));
    }

    pub fn transactions(&self, transactions: Vec<Transaction>) {
        for transactions in transactions.chunks(MAX_RELAY_TRANSACTIONS) {
            let _ = self.outbound.send(Envelope::Transactions {
                transactions: transactions.to_vec(),
            });
        }
    }

    pub fn tickets(&self, tickets: Vec<TicketTransaction>) {
        for tickets in tickets.chunks(MAX_RELAY_TICKETS) {
            let _ = self.outbound.send(Envelope::Tickets {
                tickets: tickets.to_vec(),
            });
        }
    }

    pub fn candidates(&self, candidates: Vec<Sidecar>) {
        for candidates in candidates.chunks(MAX_RELAY_CANDIDATES) {
            let _ = self.outbound.send(Envelope::Candidates {
                candidates: candidates.to_vec(),
            });
        }
    }
}

pub struct ConsensusNetwork {
    local_addr: SocketAddr,
    peers: PeerRegistry,
    outbound: broadcast::Sender<Envelope>,
    shutdown: watch::Sender<bool>,
    supervisor: JoinHandle<()>,
}

#[derive(Clone)]
struct NetworkContext {
    node: SharedConsensusNode,
    outbound: broadcast::Sender<Envelope>,
    peers: PeerRegistry,
    advertisement: P2pAdvertisement,
}

impl ConsensusNetwork {
    pub async fn start(
        node: SharedConsensusNode,
        bind_addr: SocketAddr,
        outbound_peers: Vec<PeerAddress>,
    ) -> Result<Self> {
        Self::start_with_peer_store(node, bind_addr, outbound_peers, None).await
    }

    /// Like [`Self::start`], but peers added at runtime are saved to and restored from
    /// `peer_store` so they survive a restart.
    pub async fn start_with_peer_store(
        node: SharedConsensusNode,
        bind_addr: SocketAddr,
        outbound_peers: Vec<PeerAddress>,
        peer_store: Option<PathBuf>,
    ) -> Result<Self> {
        Self::start_configured(
            node,
            bind_addr,
            outbound_peers,
            peer_store,
            P2pAdvertisement::default(),
        )
        .await
    }

    pub async fn start_configured(
        node: SharedConsensusNode,
        bind_addr: SocketAddr,
        outbound_peers: Vec<PeerAddress>,
        peer_store: Option<PathBuf>,
        advertisement: P2pAdvertisement,
    ) -> Result<Self> {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("binding P2P listener on {bind_addr}"))?;
        let local_addr = listener
            .local_addr()
            .context("reading P2P listener address")?;
        let (outbound, _) = broadcast::channel(OUTBOUND_CHANNEL_CAPACITY);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let peers = PeerRegistry::default();
        let added = match &peer_store {
            Some(path) => load_added_peers(path)?,
            None => Vec::new(),
        };
        {
            let mut targets = peers.targets.write().expect("peer registry lock poisoned");
            for address in &outbound_peers {
                targets.insert(address.clone(), new_target(address, "startup"));
            }
            for address in &added {
                targets
                    .entry(address.clone())
                    .or_insert_with(|| new_target(address, "added"));
            }
        }
        *peers
            .store_path
            .write()
            .expect("peer registry lock poisoned") = peer_store;
        let (commands, command_rx) = mpsc::unbounded_channel();
        *peers.commands.write().expect("peer registry lock poisoned") = Some(commands);
        let initial = peers
            .targets()
            .iter()
            .filter_map(|target| target.address.parse().ok())
            .collect();
        let context = NetworkContext {
            node,
            outbound: outbound.clone(),
            peers: peers.clone(),
            advertisement,
        };
        let supervisor = tokio::spawn(run_supervisor(
            listener,
            context,
            shutdown_rx,
            initial,
            command_rx,
        ));
        Ok(Self {
            local_addr,
            peers,
            outbound,
            shutdown,
            supervisor,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn relay(&self) -> ConsensusRelay {
        ConsensusRelay {
            outbound: self.outbound.clone(),
        }
    }

    pub fn peer_registry(&self) -> PeerRegistry {
        self.peers.clone()
    }

    pub fn broadcast_block(&self, block: &HeartbeatBlock) {
        self.relay().heartbeat(block);
    }

    pub fn broadcast_transactions(&self, transactions: Vec<Transaction>) {
        self.relay().transactions(transactions);
    }

    pub fn broadcast_tickets(&self, tickets: Vec<TicketTransaction>) {
        self.relay().tickets(tickets);
    }

    pub fn broadcast_candidates(&self, candidates: Vec<Sidecar>) {
        self.relay().candidates(candidates);
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.supervisor.await;
    }
}

fn new_target(address: &PeerAddress, source: &'static str) -> PeerTarget {
    PeerTarget {
        address: address.to_string(),
        source,
        connected: false,
        verified: false,
        last_attempt: None,
        last_error: None,
    }
}

async fn run_supervisor(
    listener: TcpListener,
    context: NetworkContext,
    mut shutdown: watch::Receiver<bool>,
    outbound_peers: Vec<PeerAddress>,
    mut commands: mpsc::UnboundedReceiver<PeerCommand>,
) {
    let inbound_limit = Arc::new(StdMutex::new(InboundLimiter::default()));
    let mut outbound_tasks: HashMap<PeerAddress, JoinHandle<()>> = HashMap::new();
    let task_shutdown = shutdown.clone();
    let task_context = context.clone();
    let spawn_outbound = |peer: PeerAddress| {
        tokio::spawn(run_outbound_peer(
            peer,
            task_context.clone(),
            task_shutdown.clone(),
        ))
    };
    for peer in outbound_peers {
        outbound_tasks.insert(peer.clone(), spawn_outbound(peer));
    }

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            Some(command) = commands.recv() => match command {
                PeerCommand::Add(peer) => {
                    if outbound_tasks.get(&peer).is_none_or(JoinHandle::is_finished) {
                        outbound_tasks.insert(peer.clone(), spawn_outbound(peer));
                    }
                }
                PeerCommand::Remove(peer) => {
                    if let Some(task) = outbound_tasks.remove(&peer) {
                        task.abort();
                    }
                    context.peers.disconnect(&format!("outbound:{peer}"));
                }
            },
            accepted = listener.accept() => {
                let (stream, peer_addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        eprintln!("accepting a P2P connection failed: {error}");
                        tokio::time::sleep(ACCEPT_ERROR_DELAY).await;
                        continue;
                    }
                };
                let Some(permit) = InboundLimiter::acquire(&inbound_limit, peer_addr.ip()) else {
                    continue;
                };
                let receiver = context.outbound.subscribe();
                let session_shutdown = shutdown.clone();
                let session_context = context.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let key = format!("inbound:{peer_addr}");
                    session_context.peers.connect(key.clone(), peer_addr, "inbound");
                    let _ = run_connection(
                        stream,
                        receiver,
                        session_shutdown,
                        session_context.clone(),
                        key.clone(),
                        Some(peer_addr),
                    )
                    .await;
                    session_context.peers.disconnect(&key);
                });
            }
        }
    }

    for task in outbound_tasks.into_values() {
        task.abort();
        let _ = task.await;
    }
}

/// Tries each resolved address in turn: a host with a dead IPv6 route may have a live IPv4 one.
async fn connect_peer(peer: &PeerAddress) -> io::Result<TcpStream> {
    let mut last_error = io::Error::new(io::ErrorKind::NotFound, "the host has no addresses");
    let addresses = tokio::time::timeout(CONNECT_TIMEOUT, lookup_host(peer.as_str()))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    for address in addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = error,
            Err(_) => last_error = io::ErrorKind::TimedOut.into(),
        }
    }
    Err(last_error)
}

async fn run_outbound_peer(
    peer: PeerAddress,
    context: NetworkContext,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let attempt = unix_time_secs();
        match connect_peer(&peer).await {
            Ok(stream) => 'session: {
                let remote = stream.peer_addr().ok();
                // Falls through to the tail below, which drops a discovered duplicate.
                if let Some(holder) =
                    remote.and_then(|remote| context.peers.claim_outbound_remote(remote, &peer))
                {
                    context.peers.update_target(&peer, |target| {
                        target.last_attempt = Some(attempt);
                        target.last_error =
                            Some(format!("already connected to this node as {holder}"));
                    });
                    break 'session;
                }
                let key = format!("outbound:{peer}");
                context.peers.connect(key.clone(), &peer, "outbound");
                context.peers.update_target(&peer, |target| {
                    target.connected = true;
                    target.last_attempt = Some(attempt);
                    target.last_error = None;
                });
                let result = run_connection(
                    stream,
                    context.outbound.subscribe(),
                    shutdown.clone(),
                    context.clone(),
                    key.clone(),
                    None,
                )
                .await;
                context.peers.disconnect(&key);
                if let Some(remote) = remote {
                    context.peers.release_outbound_remote(remote, &peer);
                }
                context.peers.update_target(&peer, |target| {
                    target.connected = false;
                    target.verified = false;
                    target.last_error = Some(match result {
                        Ok(()) => "connection closed".to_owned(),
                        Err(error) => format!("{error:#}"),
                    });
                });
            }
            Err(error) => context.peers.update_target(&peer, |target| {
                target.last_attempt = Some(attempt);
                target.last_error = Some(error.to_string());
            }),
        }
        // A target that turned out to be this node is not dialled again. An added one stays
        // listed with the reason as its last error, so the operator can remove it.
        if context.peers.forget_discovered(&peer) || context.peers.is_own_address(&peer) {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(RECONNECT_DELAY) => {}
        }
    }
}

async fn run_connection(
    stream: TcpStream,
    mut outbound: broadcast::Receiver<Envelope>,
    mut shutdown: watch::Receiver<bool>,
    context: NetworkContext,
    peer_key: String,
    discover_from: Option<SocketAddr>,
) -> Result<()> {
    let NetworkContext {
        node,
        outbound: relay_outbound,
        peers,
        advertisement,
    } = context;
    stream
        .set_nodelay(true)
        .context("enabling P2P TCP_NODELAY")?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let nonce = peers.open_session_nonce();
    let status = node.lock().await.status();
    write_envelope(
        &mut writer,
        &Envelope::Hello(
            Hello::new(status.height, status.tip)
                .with_listen_addr(advertisement.get().map(|address| address.to_string()))
                .with_known_peers(peers.known_peers_for_hello(&peer_key))
                .with_node_nonce(nonce.nonce),
        ),
    )
    .await?;
    let mut session = PeerSession::default();
    // A peer has to prove it speaks the protocol quickly, and a complete line resets this: a peer
    // that dribbles bytes without finishing a line runs out of time too.
    let mut read_deadline = Instant::now() + HELLO_TIMEOUT;
    // Lives outside the select loop: a partial line survives when another branch wins the race.
    let mut partial_line = Vec::new();
    // When the unfinished line in `partial_line` was first seen. The loop wakes at least once per
    // advertisement interval, so a stalled line is noticed within that much of its deadline.
    let mut line_started: Option<Instant> = None;
    let mut peer_advertisement = tokio::time::interval_at(
        Instant::now() + PEER_ADVERTISEMENT_INTERVAL,
        PEER_ADVERTISEMENT_INTERVAL,
    );

    loop {
        let deadline = if partial_line.is_empty() {
            line_started = None;
            read_deadline
        } else {
            read_deadline.min(*line_started.get_or_insert_with(Instant::now) + LINE_TIMEOUT)
        };
        tokio::select! {
            // In this order: a line that arrived while a write held up the loop beats the deadline.
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            message = outbound.recv() => {
                match message {
                    Ok(envelope) => write_envelope(&mut writer, &envelope).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
            _ = peer_advertisement.tick() => {
                let status = node.lock().await.status();
                write_envelope(
                    &mut writer,
                    &Envelope::Hello(
                        Hello::new(status.height, status.tip)
                            .with_listen_addr(advertisement.get().map(|address| address.to_string()))
                            .with_known_peers(peers.known_peers_for_hello(&peer_key))
                            .with_node_nonce(nonce.nonce),
                    ),
                )
                .await?;
            }
            line = read_bounded_line(
                &mut reader,
                &mut partial_line,
                if session.remote_hello().is_some() { MAX_WIRE_LINE_BYTES } else { MAX_HELLO_LINE_BYTES },
            ) => {
                let Some(line) = line? else {
                    return Ok(());
                };
                read_deadline = Instant::now() + IDLE_TIMEOUT;
                let envelope = decode_line(&line)?;
                match &envelope {
                    Envelope::Hello(hello) => {
                        if hello.node_nonce.is_some_and(|nonce| peers.is_own_nonce(nonce)) {
                            peers.mark_own_session(&peer_key);
                            bail!("connected to this node itself");
                        }
                        peers.update_hello(&peer_key, hello);
                        let own = advertisement.get();
                        if hello.validate().is_ok() {
                            if is_own_endpoint(own.as_ref(), hello.listen_addr.as_deref()) {
                                bail!("connected to this node itself");
                            }
                            peers.verify_outbound(&peer_key);
                            peers.discover_hello_peers(hello, own.as_ref());
                        }
                        if hello.validate().is_ok()
                            && let (Some(remote), Some(advertised)) = (discover_from, &hello.listen_addr)
                            && let Ok(advertised) = advertised.parse::<SocketAddr>()
                        {
                                let remote_ip = remote.ip().to_canonical();
                                let advertised_ip = advertised.ip().to_canonical();
                                if advertised.port() > 0
                                    && !remote_ip.is_loopback()
                                    && !remote_ip.is_unspecified()
                                    && (advertised_ip == remote_ip || advertised_ip.is_unspecified())
                                {
                                    peers.discover_target(
                                        SocketAddr::new(remote_ip, advertised.port()).into(),
                                        own.as_ref(),
                                    );
                                }
                        }
                    }
                    Envelope::Blocks { blocks } => {
                        if let Some(block) = blocks.iter().max_by_key(|block| block.header.height) {
                            peers.observe_block(&peer_key, block.header.height, &block.hash());
                        }
                    }
                    Envelope::CompactBlock(compact) => {
                        peers.observe_block(&peer_key, compact.header.height, &compact.header.hash());
                    }
                    // A peer announces its new tips by hash. Blocks it got from us never come back
                    // as blocks, so a hash this node already holds is how its height moves on.
                    Envelope::Inventory { block_hashes } => {
                        let node = node.lock().await;
                        let known = block_hashes
                            .iter()
                            .filter_map(|hash| node.chain().block(hash))
                            .max_by_key(|block| block.header.height)
                            .map(|block| (block.header.height, block.hash()));
                        drop(node);
                        if let Some((height, hash)) = known {
                            peers.observe_block(&peer_key, height, &hash);
                        }
                    }
                    _ => {}
                }
                // Range validation and SQLite persistence are synchronous. Import one block per
                // lock turn so status requests and the screen-data projector can run throughout a
                // fresh sync, and tell Tokio that the work may block its current worker thread.
                // Keep only the final range response: every singleton import asks for the next
                // range while the remote Hello still says this node is behind.
                if let Envelope::Blocks { blocks } = envelope {
                    let (responses, gossip, imported) = import_block_range(
                        &node,
                        &mut session,
                        blocks,
                        &peers,
                        &peer_key,
                    )
                    .await;
                    // Blocks activated before a bad one are announced even though the session
                    // then ends, so peers do not wait for the next Hello to learn the new tip.
                    for envelope in gossip {
                        let _ = relay_outbound.send(envelope);
                    }
                    imported?;
                    for response in responses {
                        write_envelope(&mut writer, &response).await?;
                    }
                    continue;
                }
                let (responses, gossip) = {
                    let mut node = node.lock().await;
                    let before_tip = node.status().tip;
                    let unseen_transactions = match &envelope {
                        Envelope::Transactions { transactions } => transactions
                            .iter()
                            .filter(|transaction| !node.has_pending_transaction(&transaction.id()))
                            .map(|transaction| (transaction.id(), transaction.clone()))
                            .collect::<std::collections::BTreeMap<_, _>>(),
                        _ => std::collections::BTreeMap::new(),
                    };
                    let unseen_tickets = match &envelope {
                        Envelope::Tickets { tickets } => tickets
                            .iter()
                            .filter(|ticket| !node.has_pending_ticket(&ticket.id()))
                            .map(|ticket| (ticket.id(), ticket.clone()))
                            .collect::<std::collections::BTreeMap<_, _>>(),
                        _ => std::collections::BTreeMap::new(),
                    };
                    let unseen_candidates = match &envelope {
                        Envelope::Candidates { candidates } => candidates
                            .iter()
                            .filter(|candidate| {
                                !node.has_pending_candidate(&candidate.header.hash())
                            })
                            .map(|candidate| (candidate.header.hash(), candidate.clone()))
                            .collect::<std::collections::BTreeMap<_, _>>(),
                        _ => std::collections::BTreeMap::new(),
                    };
                    let responses = session.handle(&mut node, envelope, unix_time_secs())?;
                    let mut gossip = Vec::new();
                    let after_tip = node.status().tip;
                    if after_tip != before_tip {
                        gossip.push(Envelope::Inventory {
                            block_hashes: vec![after_tip],
                        });
                    }
                    let transactions = unseen_transactions
                        .into_iter()
                        .filter(|(id, _)| node.has_pending_transaction(id))
                        .map(|(_, transaction)| transaction)
                        .collect::<Vec<_>>();
                    if !transactions.is_empty() {
                        gossip.push(Envelope::Transactions { transactions });
                    }
                    let tickets = unseen_tickets
                        .into_iter()
                        .filter(|(id, _)| node.has_pending_ticket(id))
                        .map(|(_, ticket)| ticket)
                        .collect::<Vec<_>>();
                    if !tickets.is_empty() {
                        gossip.push(Envelope::Tickets { tickets });
                    }
                    let candidates = unseen_candidates
                        .into_iter()
                        .filter(|(hash, _)| node.has_pending_candidate(hash))
                        .map(|(_, candidate)| candidate)
                        .collect::<Vec<_>>();
                    if !candidates.is_empty() {
                        gossip.push(Envelope::Candidates { candidates });
                    }
                    (responses, gossip)
                };
                for response in responses {
                    write_envelope(&mut writer, &response).await?;
                }
                for envelope in gossip {
                    let _ = relay_outbound.send(envelope);
                }
            }
            () = tokio::time::sleep_until(deadline) => bail!("peer timed out"),
        }
    }
}

async fn import_block_range(
    node: &SharedConsensusNode,
    session: &mut PeerSession,
    blocks: Vec<HeartbeatBlock>,
    peers: &PeerRegistry,
    peer_key: &str,
) -> (Vec<Envelope>, Vec<Envelope>, Result<()>) {
    let mut responses = Vec::new();
    let mut first_tip = None;
    let mut final_tip = None;
    let mut imported = Ok(());
    let mut blocks = blocks.into_iter().peekable();

    // An empty range still ends a side-branch walk and must reach PeerSession.
    if blocks.peek().is_none() {
        let mut locked = node.lock().await;
        let tip = locked.status().tip;
        match handle_blocking_peer_envelope(
            session,
            &mut locked,
            Envelope::Blocks { blocks: Vec::new() },
        ) {
            Ok(handled) => responses = handled,
            Err(error) => imported = Err(error),
        }
        first_tip = Some(tip);
        final_tip = Some(locked.status().tip);
    }

    for block in blocks {
        let mut locked = node.lock().await;
        first_tip.get_or_insert_with(|| locked.status().tip);
        let handled = handle_blocking_peer_envelope(
            session,
            &mut locked,
            Envelope::Blocks {
                blocks: vec![block],
            },
        );
        final_tip = Some(locked.status().tip);
        drop(locked);
        match handled {
            Ok(handled) => responses = handled,
            Err(error) => {
                imported = Err(error);
                break;
            }
        }
        // Tokio's mutex is fair; yielding lets already-waiting HTTP and projection tasks take
        // their turn before the next block import reacquires it.
        tokio::task::yield_now().await;
    }

    peers.update_syncing(peer_key, session.is_syncing());
    let gossip = match (first_tip, final_tip) {
        (Some(before), Some(after)) if before != after => vec![Envelope::Inventory {
            block_hashes: vec![after],
        }],
        _ => Vec::new(),
    };
    (responses, gossip, imported)
}

fn handle_blocking_peer_envelope(
    session: &mut PeerSession,
    node: &mut Node<ConsensusChainStore>,
    envelope: Envelope,
) -> Result<Vec<Envelope>> {
    let handle = tokio::runtime::Handle::current();
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
        tokio::task::block_in_place(|| session.handle(node, envelope, unix_time_secs()))
    } else {
        // Some unit tests deliberately use Tokio's current-thread runtime, where block_in_place
        // is unavailable. Production uses the multi-thread runtime from #[tokio::main].
        session.handle(node, envelope, unix_time_secs())
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

async fn write_envelope(writer: &mut (impl AsyncWrite + Unpin), envelope: &Envelope) -> Result<()> {
    let bytes = encode_line(envelope)?;
    for chunk in bytes.chunks(WRITE_CHUNK_BYTES) {
        tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(chunk))
            .await
            .context("peer stopped reading")?
            .context("writing P2P envelope")?;
    }
    Ok(())
}

/// Cancel-safe: consumed bytes move into `line` without an intervening await, so dropping this
/// future and calling it again with the same buffer resumes the line.
async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    loop {
        let buffer = reader.fill_buf().await.context("reading P2P envelope")?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then(|| std::mem::take(line)));
        }
        let consumed = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |position| position + 1);
        if line.len().saturating_add(consumed) > limit {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "P2P line exceeds limit").into(),
            );
        }
        line.extend_from_slice(&buffer[..consumed]);
        let complete = line.last() == Some(&b'\n');
        reader.consume(consumed);
        if complete {
            return Ok(Some(std::mem::take(line)));
        }
    }
}

fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapters::consensus_store::ConsensusChainStore,
        consensus::{CandidateHeader, Chain, GENESIS_TIMESTAMP, PROTOCOL_VERSION, merkle_root},
    };

    #[test]
    fn peer_addresses_accept_host_names_and_reject_the_rest() {
        for valid in [
            "iuna.jhx.app:9444",
            "203.0.113.7:9444",
            "[2001:db8::1]:9444",
            "node-1:9",
        ] {
            assert_eq!(valid.parse::<PeerAddress>().unwrap().as_str(), valid);
        }
        for (spelling, normal) in [
            ("IUNA.jhx.app:9444", "iuna.jhx.app:9444"),
            ("[2001:DB8:0::1]:9444", "[2001:db8::1]:9444"),
        ] {
            assert_eq!(spelling.parse::<PeerAddress>().unwrap().as_str(), normal);
        }
        for invalid in [
            "iuna.jhx.app",
            "iuna.jhx.app:0",
            "iuna.jhx.app:99999",
            ":9444",
            "a b:1",
            "",
        ] {
            assert!(invalid.parse::<PeerAddress>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn one_host_cannot_take_every_inbound_slot() {
        let limiter = Arc::new(StdMutex::new(InboundLimiter::default()));
        let ip = |last: u8| IpAddr::from([203, 0, 113, last]);
        let mut sessions = (0..MAX_INBOUND_SESSIONS_PER_IP)
            .map(|_| InboundLimiter::acquire(&limiter, ip(1)).unwrap())
            .collect::<Vec<_>>();

        assert!(InboundLimiter::acquire(&limiter, ip(1)).is_none());
        assert!(InboundLimiter::acquire(&limiter, ip(2)).is_some());

        sessions.pop();
        assert!(InboundLimiter::acquire(&limiter, ip(1)).is_some());
    }

    #[test]
    fn a_host_reconnecting_in_a_loop_is_rate_limited_and_recovers() {
        let mut limiter = InboundLimiter::default();
        let ip = IpAddr::from([203, 0, 113, 3]);
        let start = Instant::now();
        for _ in 0..MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW {
            assert!(limiter.admit(ip, start));
            limiter.active -= 1;
            limiter.hosts.get_mut(&ip).unwrap().active -= 1;
        }
        assert!(!limiter.admit(ip, start));
        assert!(limiter.admit(ip, start + INBOUND_ACCEPT_WINDOW));
    }

    #[test]
    fn hosts_without_sessions_leave_the_limiter() {
        let mut limiter = InboundLimiter::default();
        let start = Instant::now();
        for index in 0..MAX_INBOUND_TRACKED_HOSTS as u32 {
            assert!(limiter.admit(IpAddr::from(index.to_be_bytes()), start));
            limiter.active -= 1;
        }
        limiter.hosts.values_mut().for_each(|host| host.active -= 1);
        assert!(limiter.admit(
            IpAddr::from([203, 0, 113, 5]),
            start + INBOUND_ACCEPT_WINDOW
        ));
        assert_eq!(limiter.hosts.len(), 1);
    }

    #[test]
    fn an_ipv6_network_counts_as_one_host() {
        let limiter = Arc::new(StdMutex::new(InboundLimiter::default()));
        let address = |low: u16| IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, low]);
        let _sessions = (1..=MAX_INBOUND_SESSIONS_PER_IP as u16)
            .map(|low| InboundLimiter::acquire(&limiter, address(low)).unwrap())
            .collect::<Vec<_>>();

        assert!(InboundLimiter::acquire(&limiter, address(100)).is_none());
        assert_eq!(
            inbound_host("::ffff:203.0.113.9".parse().unwrap()),
            IpAddr::from([203, 0, 113, 9])
        );
    }

    #[test]
    fn bootstrap_peers_are_seeded_once_and_stay_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let peers = ["iuna.jhx.app:9444".parse::<PeerAddress>().unwrap()];

        seed_added_peers(&path, &peers).unwrap();
        assert_eq!(load_added_peers(&path).unwrap(), peers);

        fs::write(&path, "[]").unwrap();
        seed_added_peers(&path, &peers).unwrap();
        assert!(load_added_peers(&path).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_saved_peer_is_skipped_instead_of_stopping_the_node() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        fs::write(&path, r#"["not a peer", "203.0.113.7:9444"]"#).unwrap();

        assert_eq!(
            load_added_peers(&path).unwrap(),
            ["203.0.113.7:9444".parse::<PeerAddress>().unwrap()]
        );
    }

    #[test]
    fn a_saved_bootstrap_peer_follows_a_port_change_and_others_stay() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        fs::write(
            &path,
            r#"["iuna.jhx.app:9444", "203.0.113.7:9444", "iuna.jhx.app:9444"]"#,
        )
        .unwrap();
        let peers = ["iuna.jhx.app:9444".parse::<PeerAddress>().unwrap()];

        seed_added_peers(&path, &peers).unwrap();

        assert_eq!(
            load_added_peers(&path)
                .unwrap()
                .iter()
                .map(PeerAddress::as_str)
                .collect::<Vec<_>>(),
            ["iuna.jhx.app:9444", "203.0.113.7:9444"]
        );
    }

    #[test]
    fn runtime_peers_are_validated_saved_and_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let registry = PeerRegistry::default();
        let address: SocketAddr = "203.0.113.7:9444".parse().unwrap();
        assert!(
            registry.add_target(address).is_err(),
            "needs a running network"
        );

        let (commands, mut received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        *registry.store_path.write().unwrap() = Some(path.clone());
        registry.add_target(address).unwrap();
        assert!(
            registry.add_target(address).is_err(),
            "duplicates are refused"
        );
        assert!(
            matches!(received.try_recv(), Ok(PeerCommand::Add(added)) if added == address.into())
        );
        assert_eq!(
            load_added_peers(&path).unwrap(),
            vec![PeerAddress::from(address)]
        );
        assert_eq!(registry.targets()[0].source, "added");

        registry.remove_target(address).unwrap();
        assert!(
            matches!(received.try_recv(), Ok(PeerCommand::Remove(removed)) if removed == address.into())
        );
        assert!(load_added_peers(&path).unwrap().is_empty());
        assert!(registry.remove_target(address).is_err());

        let broken_path = directory.path().join("missing").join("peers.json");
        *registry.store_path.write().unwrap() = Some(broken_path);
        assert!(registry.add_target(address).is_err());
        assert!(
            registry
                .targets()
                .iter()
                .all(|target| target.address != address.to_string())
        );
        assert!(
            received.try_recv().is_err(),
            "failed add must not reach supervisor"
        );

        *registry.store_path.write().unwrap() = Some(path.clone());
        registry.add_target(address).unwrap();
        assert!(
            matches!(received.try_recv(), Ok(PeerCommand::Add(added)) if added == address.into())
        );
        *registry.store_path.write().unwrap() = Some(directory.path().join("missing-2/peers.json"));
        assert!(registry.remove_target(address).is_err());
        assert!(
            registry
                .targets()
                .iter()
                .any(|target| target.address == address.to_string())
        );
        assert!(
            received.try_recv().is_err(),
            "failed remove must not reach supervisor"
        );

        *registry.store_path.write().unwrap() = Some(path);
        registry.remove_target(address).unwrap();
        assert!(
            matches!(received.try_recv(), Ok(PeerCommand::Remove(removed)) if removed == address.into())
        );

        for port in 0..MAX_DISCOVERED_PEER_TARGETS as u16 {
            registry.discover_target(
                SocketAddr::from(([198, 51, 100, 8], 30_000 + port)).into(),
                None,
            );
        }
        registry.discover_target(SocketAddr::from(([198, 51, 100, 8], 40_000)).into(), None);
        assert_eq!(
            registry
                .targets()
                .iter()
                .filter(|target| target.source == "discovered")
                .count(),
            MAX_DISCOVERED_PEER_TARGETS,
            "Hello discovery has its own bound"
        );

        for port in 0..MAX_PEER_TARGETS as u16 {
            registry
                .add_target(SocketAddr::from(([203, 0, 113, 8], 20_000 + port)))
                .unwrap();
        }
        assert_eq!(
            registry.targets().len(),
            MAX_PEER_TARGETS + MAX_DISCOVERED_PEER_TARGETS,
            "discovered peers do not consume operator slots"
        );
        assert!(
            registry.add_target(address).is_err(),
            "the target list is bounded"
        );
    }

    #[test]
    fn ipv6_prefixes_that_embed_ipv4_follow_the_ipv4_rules() {
        let discoverable = |ip: &str| ip_is_publicly_discoverable(ip.parse().unwrap());
        // 6to4 and NAT64 around 10.0.0.1 and around 87.215.225.115.
        assert!(!discoverable("2002:a00:1::1"));
        assert!(discoverable("2002:57d7:e173::1"));
        assert!(!discoverable("64:ff9b::a00:1"));
        assert!(discoverable("64:ff9b::57d7:e173"));
        assert!(!discoverable("2001:0:4136:e378:8000:63bf:3fff:fdd2"));
        assert!(discoverable("2a01:4f8::1"));
    }

    #[test]
    fn hello_discovers_only_bounded_public_socket_addresses() {
        let registry = PeerRegistry::default();
        let (commands, mut received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        let hello = Hello::new(0, crate::consensus::GENESIS_HASH).with_known_peers(vec![
            "87.215.225.115:9444".to_owned(),
            "127.0.0.1:9444".to_owned(),
            "192.168.1.20:9444".to_owned(),
            "198.18.0.1:9444".to_owned(),
            "[2001:db8::1]:9444".to_owned(),
            "peer.example:9444".to_owned(),
        ]);

        registry.discover_hello_peers(&hello, None);

        assert!(matches!(
            received.try_recv(),
            Ok(PeerCommand::Add(address)) if address.as_str() == "87.215.225.115:9444"
        ));
        assert!(received.try_recv().is_err());
        assert_eq!(registry.targets().len(), 1);
        assert_eq!(registry.targets()[0].source, "discovered");
    }

    #[test]
    fn every_peer_shares_connected_targets_except_the_current_outbound_peer() {
        let registry = PeerRegistry::default();
        let current = "bootstrap.example:9444".parse::<PeerAddress>().unwrap();
        let learned = "87.215.225.115:9444".parse::<PeerAddress>().unwrap();
        let unverified = "87.215.225.116:9444".parse::<PeerAddress>().unwrap();
        {
            let mut targets = registry.targets.write().unwrap();
            targets.insert(current.clone(), new_target(&current, "added"));
            targets.insert(learned.clone(), new_target(&learned, "discovered"));
            targets.get_mut(&current).unwrap().verified = true;
            targets.get_mut(&learned).unwrap().verified = true;
            targets.insert(unverified.clone(), new_target(&unverified, "discovered"));
            targets.get_mut(&unverified).unwrap().connected = true;
        }

        assert_eq!(
            registry.known_peers_for_hello("outbound:bootstrap.example:9444"),
            ["87.215.225.115:9444"],
            "a TCP connection without a valid Hello is not passed on"
        );
    }

    #[test]
    fn only_a_specific_advertised_endpoint_identifies_this_node() {
        let own = |address: &str| address.parse::<PeerAddress>().unwrap();
        let public = own("87.215.225.115:9444");
        assert!(is_own_endpoint(Some(&public), Some("87.215.225.115:9444")));
        assert!(!is_own_endpoint(Some(&public), Some("87.215.225.116:9444")));
        let named = own("node.example:9444");
        assert!(is_own_endpoint(Some(&named), Some("node.example:9444")));
        // Every public node without a configured address advertises this.
        let default = own("0.0.0.0:9444");
        assert!(!is_own_endpoint(Some(&default), Some("0.0.0.0:9444")));
        assert!(!is_own_endpoint(None, Some("0.0.0.0:9444")));
    }

    #[test]
    fn a_node_advertising_the_default_endpoint_stops_learning_its_public_address() {
        let registry = PeerRegistry::default();
        let (commands, _received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        let first = registry.open_session_nonce();
        let second = registry.open_session_nonce();
        assert_ne!(first.nonce, second.nonce);
        assert!(registry.is_own_nonce(first.nonce) && registry.is_own_nonce(second.nonce));
        let closed = first.nonce;
        drop(first);
        assert!(!registry.is_own_nonce(closed));
        assert!(!PeerRegistry::default().is_own_nonce(second.nonce));
        let default = "0.0.0.0:9444".parse::<PeerAddress>().unwrap();
        let public = "87.215.225.115:9444";
        let hello = Hello::new(0, crate::consensus::GENESIS_HASH)
            .with_known_peers(vec![public.to_owned(), "87.215.225.116:9444".to_owned()]);

        // The session that dialled `public` received this node's own nonce.
        registry.mark_own_session(&format!("outbound:{public}"));
        registry.discover_hello_peers(&hello, Some(&default));

        assert_eq!(
            registry
                .targets()
                .iter()
                .map(|target| target.address.as_str())
                .collect::<Vec<_>>(),
            ["87.215.225.116:9444"]
        );
        // Inbound sessions name no dialled address, so nothing is remembered for them.
        registry.mark_own_session("inbound:87.215.225.117:50000");
        assert_eq!(registry.own_addresses.read().unwrap().len(), 1);
        // Added by hand, it is tried again.
        registry.forget_own_address(&public.parse().unwrap());
        assert!(registry.own_addresses.read().unwrap().is_empty());
    }

    #[test]
    fn a_node_reached_by_host_name_is_not_dialled_again_by_address() {
        let registry = PeerRegistry::default();
        let (commands, _received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        let named = "bootstrap.example:9444".parse::<PeerAddress>().unwrap();
        let resolved: SocketAddr = "87.215.225.115:9444".parse().unwrap();
        let by_address = PeerAddress::from(resolved);
        registry
            .targets
            .write()
            .unwrap()
            .insert(named.clone(), new_target(&named, "added"));

        assert_eq!(registry.claim_outbound_remote(resolved, &named), None);
        // A Hello naming the same node by address does not add a second target.
        registry.discover_target(by_address.clone(), None);
        assert_eq!(registry.targets().len(), 1);

        // One discovered before the named session connected is refused when it connects.
        registry
            .targets
            .write()
            .unwrap()
            .insert(by_address.clone(), new_target(&by_address, "discovered"));
        let mapped: SocketAddr = "[::ffff:87.215.225.115]:9444".parse().unwrap();
        assert_eq!(
            registry.claim_outbound_remote(mapped, &by_address),
            Some(named.clone())
        );

        // Releasing someone else's claim changes nothing; the holder's own release frees it.
        registry.release_outbound_remote(resolved, &by_address);
        assert!(registry.has_outbound_session_to(&by_address));
        registry.release_outbound_remote(resolved, &named);
        assert!(!registry.has_outbound_session_to(&by_address));
        assert_eq!(registry.claim_outbound_remote(resolved, &by_address), None);
    }

    #[test]
    fn hello_never_teaches_a_node_its_own_address() {
        let registry = PeerRegistry::default();
        let (commands, _received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        let own = "87.215.225.115:9444".parse::<PeerAddress>().unwrap();
        let hello = Hello::new(0, crate::consensus::GENESIS_HASH)
            .with_known_peers(vec![own.to_string(), "87.215.225.116:9444".to_owned()]);

        registry.discover_hello_peers(&hello, Some(&own));

        assert_eq!(
            registry
                .targets()
                .iter()
                .map(|target| target.address.as_str())
                .collect::<Vec<_>>(),
            ["87.215.225.116:9444"]
        );
    }

    #[test]
    fn a_failed_discovered_peer_frees_its_slot_and_configured_peers_stay() {
        let registry = PeerRegistry::default();
        let (commands, mut received) = mpsc::unbounded_channel();
        *registry.commands.write().unwrap() = Some(commands);
        let discovered = "87.215.225.115:9444".parse::<PeerAddress>().unwrap();
        let configured = "bootstrap.example:9444".parse::<PeerAddress>().unwrap();
        {
            let mut targets = registry.targets.write().unwrap();
            targets.insert(discovered.clone(), new_target(&discovered, "discovered"));
            targets.insert(configured.clone(), new_target(&configured, "added"));
        }

        assert!(registry.forget_discovered(&discovered));
        assert!(matches!(
            received.try_recv(),
            Ok(PeerCommand::Remove(removed)) if removed == discovered
        ));
        assert!(!registry.forget_discovered(&configured));
        assert_eq!(registry.targets().len(), 1);
    }

    #[tokio::test]
    async fn relay_publishes_heartbeats_and_wallet_objects_to_subscribers() {
        let (outbound, mut receiver) = broadcast::channel(4);
        let relay = ConsensusRelay { outbound };
        let genesis = Chain::from_genesis().unwrap().canonical_blocks()[0].clone();
        relay.heartbeat(&genesis);
        let Envelope::CompactBlock(block) = receiver.recv().await.unwrap() else {
            panic!("expected compact Heartbeat relay");
        };
        assert_eq!(block.header.hash(), genesis.hash());

        let transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        let transaction_id = transaction.id();
        relay.transactions(vec![transaction]);
        let Envelope::Transactions { transactions } = receiver.recv().await.unwrap() else {
            panic!("expected transaction relay");
        };
        assert_eq!(transactions[0].id(), transaction_id);

        let ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: Vec::new(),
            budget: 1,
            duration: 1,
            mining_public_key: [1; 32],
            lottery_seed: [2; 32],
            change: None,
        };
        let ticket_id = ticket.id();
        relay.tickets(vec![ticket]);
        let Envelope::Tickets { tickets } = receiver.recv().await.unwrap() else {
            panic!("expected ticket relay");
        };
        assert_eq!(tickets[0].id(), ticket_id);

        let candidate = Sidecar {
            header: CandidateHeader {
                version: PROTOCOL_VERSION,
                lottery_heartbeat: [3; 32],
                lottery_height: 1,
                parent_anchor: [4; 32],
                ticket_id: [5; 32],
                slot: 0,
                lottery_output: [6; 32],
                transactions_root: merkle_root(&[]),
                signature: vec![7; 64],
            },
            transactions: Vec::new(),
        };
        let candidate_hash = candidate.header.hash();
        relay.candidates(vec![candidate]);
        let Envelope::Candidates { candidates } = receiver.recv().await.unwrap() else {
            panic!("expected signed candidate relay");
        };
        assert_eq!(candidates[0].header.hash(), candidate_hash);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn range_import_advances_through_single_block_lock_turns() {
        let directory = tempfile::tempdir().unwrap();
        let mut source =
            Node::open(ConsensusChainStore::open(directory.path().join("source.sqlite3")).unwrap())
                .unwrap();
        for height in 1..=3 {
            source
                .mine_next(
                    GENESIS_TIMESTAMP + height * crate::consensus::HEARTBEAT_TARGET_INTERVAL_SECS,
                    [91; 32],
                )
                .unwrap();
        }
        let remote = source.status();
        let blocks = source
            .chain()
            .canonical_blocks_after(Some(0))
            .into_iter()
            .cloned()
            .collect();
        let target = Arc::new(Mutex::new(
            Node::open(ConsensusChainStore::open(directory.path().join("target.sqlite3")).unwrap())
                .unwrap(),
        ));
        let mut session = PeerSession::default();
        {
            let mut locked = target.lock().await;
            session
                .handle(
                    &mut locked,
                    Envelope::Hello(Hello::new(remote.height, remote.tip)),
                    GENESIS_TIMESTAMP,
                )
                .unwrap();
        }

        let (responses, gossip, imported) = import_block_range(
            &target,
            &mut session,
            blocks,
            &PeerRegistry::default(),
            "test-peer",
        )
        .await;
        imported.unwrap();

        assert!(responses.is_empty(), "the final block completes the range");
        assert!(matches!(
            gossip.as_slice(),
            [Envelope::Inventory { block_hashes }] if block_hashes == &[remote.tip]
        ));
        assert_eq!(target.lock().await.status(), remote);
        assert!(!session.is_syncing());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_range_that_fails_part_way_still_announces_the_blocks_it_activated() {
        let directory = tempfile::tempdir().unwrap();
        let mut source =
            Node::open(ConsensusChainStore::open(directory.path().join("source.sqlite3")).unwrap())
                .unwrap();
        for height in 1..=3 {
            source
                .mine_next(
                    GENESIS_TIMESTAMP + height * crate::consensus::HEARTBEAT_TARGET_INTERVAL_SECS,
                    [91; 32],
                )
                .unwrap();
        }
        let remote = source.status();
        let mut blocks = source
            .chain()
            .canonical_blocks_after(Some(0))
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let second = blocks[1].hash();
        // The last block no longer matches its proof of work.
        blocks[2].header.miner_public_key = [92; 32];
        let target = Arc::new(Mutex::new(
            Node::open(ConsensusChainStore::open(directory.path().join("target.sqlite3")).unwrap())
                .unwrap(),
        ));
        let mut session = PeerSession::default();
        {
            let mut locked = target.lock().await;
            session
                .handle(
                    &mut locked,
                    Envelope::Hello(Hello::new(remote.height, remote.tip)),
                    GENESIS_TIMESTAMP,
                )
                .unwrap();
        }

        let (_, gossip, imported) = import_block_range(
            &target,
            &mut session,
            blocks,
            &PeerRegistry::default(),
            "test-peer",
        )
        .await;

        assert!(imported.is_err());
        assert_eq!(target.lock().await.status().tip, second);
        assert!(matches!(
            gossip.as_slice(),
            [Envelope::Inventory { block_hashes }] if block_hashes == &[second]
        ));
    }

    #[tokio::test]
    async fn bounded_reader_accepts_complete_and_eof_terminated_lines() {
        let mut complete = BufReader::new(&b"hello\nrest"[..]);
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut complete, &mut line, MAX_WIRE_LINE_BYTES)
                .await
                .unwrap(),
            Some(b"hello\n".to_vec())
        );
        assert_eq!(
            read_bounded_line(&mut complete, &mut line, MAX_WIRE_LINE_BYTES)
                .await
                .unwrap(),
            Some(b"rest".to_vec())
        );
        assert_eq!(
            read_bounded_line(&mut complete, &mut line, MAX_WIRE_LINE_BYTES)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn bounded_reader_resumes_a_line_after_cancellation() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        writer.write_all(b"hel").await.unwrap();
        tokio::select! {
            biased;
            _ = read_bounded_line(&mut reader, &mut line, MAX_WIRE_LINE_BYTES) => panic!("the line is incomplete"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        writer.write_all(b"lo\n").await.unwrap();
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, MAX_WIRE_LINE_BYTES)
                .await
                .unwrap(),
            Some(b"hello\n".to_vec())
        );
    }

    #[tokio::test]
    async fn a_peer_without_a_hello_gets_only_a_hello_sized_buffer() {
        let bytes = vec![b'x'; MAX_HELLO_LINE_BYTES + 1];
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(
            read_bounded_line(&mut reader, &mut Vec::new(), MAX_HELLO_LINE_BYTES)
                .await
                .is_err()
        );
        let hello = serde_json::to_vec(&Envelope::Hello(
            Hello::new(u64::MAX, [0xff; 32])
                .with_listen_addr(Some("x".repeat(128)))
                .with_known_peers(vec![
                    "x".repeat(128);
                    crate::adapters::consensus_p2p::MAX_HELLO_PEERS
                ]),
        ))
        .unwrap();
        assert!(
            hello.len() * 2 < MAX_HELLO_LINE_BYTES,
            "{} bytes",
            hello.len()
        );
    }

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_input_before_eof() {
        let bytes = vec![b'x'; MAX_WIRE_LINE_BYTES + 1];
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(
            read_bounded_line(&mut reader, &mut Vec::new(), MAX_WIRE_LINE_BYTES)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[ignore = "requires permission to bind local TCP sockets"]
    async fn two_tcp_nodes_relay_a_compact_heartbeat() {
        let directory = tempfile::tempdir().unwrap();
        let source = Arc::new(Mutex::new(
            Node::open(ConsensusChainStore::open(directory.path().join("source.sqlite3")).unwrap())
                .unwrap(),
        ));
        let target = Arc::new(Mutex::new(
            Node::open(ConsensusChainStore::open(directory.path().join("target.sqlite3")).unwrap())
                .unwrap(),
        ));
        let target_network = ConsensusNetwork::start(
            Arc::clone(&target),
            "127.0.0.1:0".parse().unwrap(),
            Vec::new(),
        )
        .await
        .unwrap();
        let source_network = ConsensusNetwork::start(
            Arc::clone(&source),
            "127.0.0.1:0".parse().unwrap(),
            vec![target_network.local_addr().into()],
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let timestamp = unix_time_secs().max(GENESIS_TIMESTAMP + 1);
        let block = source.lock().await.mine_next(timestamp, [61; 32]).unwrap();
        source_network.broadcast_block(&block);

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if target.lock().await.status().tip == block.hash() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        source_network.shutdown().await;
        target_network.shutdown().await;
    }
}
