//! Tokio TCP transport for the peer state machine.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
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
/// For each chunk, so a slow but moving uplink can finish an 8 MiB block batch.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_CHUNK_BYTES: usize = 64 * 1024;
/// Above this many tracked hosts, stale ones are swept. Below it the sweep is not worth its cost.
const MAX_INBOUND_TRACKED_HOSTS: usize = 1_024;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
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
    fs::write(path, serde_json::to_vec_pretty(&seeded)?).context("writing bootstrap peers")
}

pub fn load_added_peers(path: &Path) -> Result<Vec<PeerAddress>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Vec<String>>(&bytes)
            .context("decoding saved peers")?
            .iter()
            .take(MAX_PEER_TARGETS)
            .map(|address| address.parse().context("parsing saved peer address"))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("reading saved peers"),
    }
}

impl PeerRegistry {
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
        if own == Some(&address) {
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
        drop(targets);
        if let Ok(commands) = self.command_sender() {
            let _ = commands.send(PeerCommand::Remove(address.clone()));
        }
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
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&added)?).context("writing saved peers")?;
        fs::rename(&temporary, &path).context("replacing saved peers")?;
        Ok(())
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
                || (segments[0] == 0x2001 && segments[1] == 0x0db8))
        }
    }
}

fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(ip) = ip.to_ipv4_mapped() {
        return Some(ip);
    }
    let segments = ip.segments();
    if segments[..6] != [0; 6] {
        return None;
    }
    let [a, b] = segments[6].to_be_bytes();
    let [c, d] = segments[7].to_be_bytes();
    Some(Ipv4Addr::new(a, b, c, d))
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
                let Ok((stream, peer_addr)) = accepted else {
                    continue;
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
            Ok(stream) => {
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
        if context.peers.forget_discovered(&peer) {
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
    let status = node.lock().await.status();
    write_envelope(
        &mut writer,
        &Envelope::Hello(
            Hello::new(status.height, status.tip)
                .with_listen_addr(advertisement.get().map(|address| address.to_string()))
                .with_known_peers(peers.known_peers_for_hello(&peer_key)),
        ),
    )
    .await?;
    let mut session = PeerSession::default();
    // A peer has to prove it speaks the protocol quickly, and a complete line resets this: a peer
    // that dribbles bytes without finishing a line runs out of time too.
    let mut read_deadline = Instant::now() + HELLO_TIMEOUT;
    // Lives outside the select loop: a partial line survives when another branch wins the race.
    let mut partial_line = Vec::new();
    let mut peer_advertisement = tokio::time::interval_at(
        Instant::now() + PEER_ADVERTISEMENT_INTERVAL,
        PEER_ADVERTISEMENT_INTERVAL,
    );

    loop {
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
                            .with_known_peers(peers.known_peers_for_hello(&peer_key)),
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
                        peers.update_hello(&peer_key, hello);
                        let own = advertisement.get();
                        if hello.validate().is_ok() {
                            if own.is_some()
                                && hello.listen_addr.as_deref() == own.as_ref().map(PeerAddress::as_str)
                            {
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
                    let (responses, gossip) = import_block_range(
                        &node,
                        &mut session,
                        blocks,
                        &peers,
                        &peer_key,
                    )
                    .await?;
                    for response in responses {
                        write_envelope(&mut writer, &response).await?;
                    }
                    for envelope in gossip {
                        let _ = relay_outbound.send(envelope);
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
            () = tokio::time::sleep_until(read_deadline) => bail!("peer timed out"),
        }
    }
}

async fn import_block_range(
    node: &SharedConsensusNode,
    session: &mut PeerSession,
    blocks: Vec<HeartbeatBlock>,
    peers: &PeerRegistry,
    peer_key: &str,
) -> Result<(Vec<Envelope>, Vec<Envelope>)> {
    let mut responses = Vec::new();
    let mut first_tip = None;
    let mut final_tip = None;
    let mut blocks = blocks.into_iter().peekable();

    // An empty range still ends a side-branch walk and must reach PeerSession.
    if blocks.peek().is_none() {
        let mut locked = node.lock().await;
        let tip = locked.status().tip;
        responses = handle_blocking_peer_envelope(
            session,
            &mut locked,
            Envelope::Blocks { blocks: Vec::new() },
        )?;
        first_tip = Some(tip);
        final_tip = Some(locked.status().tip);
    }

    for block in blocks {
        let mut locked = node.lock().await;
        first_tip.get_or_insert_with(|| locked.status().tip);
        responses = handle_blocking_peer_envelope(
            session,
            &mut locked,
            Envelope::Blocks {
                blocks: vec![block],
            },
        )?;
        final_tip = Some(locked.status().tip);
        drop(locked);
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
    Ok((responses, gossip))
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

        let (responses, gossip) = import_block_range(
            &target,
            &mut session,
            blocks,
            &PeerRegistry::default(),
            "test-peer",
        )
        .await
        .unwrap();

        assert!(responses.is_empty(), "the final block completes the range");
        assert!(matches!(
            gossip.as_slice(),
            [Envelope::Inventory { block_hashes }] if block_hashes == &[remote.tip]
        ));
        assert_eq!(target.lock().await.status(), remote);
        assert!(!session.is_syncing());
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
