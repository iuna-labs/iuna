use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex as StdMutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::{Instant, interval, interval_at, sleep, timeout},
};

use crate::{
    app::{
        BlockInventory, GossipEnvelope, MEMPOOL_STATUS_LIMIT, NETWORK_ID, NodeCore,
        PROTOCOL_VERSION, PeerDirection, ProtocolHello, SharedNode, SharedPeerBook,
        TRANSACTION_BATCH_LIMIT, TransactionRejection, debug_logging_enabled, now_ms,
    },
    domain::{Block, ChainSnapshot, Ledger, Transaction, TransactionSubmitOutcome, verify_vdf},
};

const MAX_BLOCK_BATCH: usize = 128;
const MAX_OBJECT_REQUESTS: usize = 128;
const MAX_INVENTORY_ITEMS: usize = 512;
const MAX_PEER_LIST: usize = 128;
const MAX_SNAPSHOT_BLOCKS: usize = 10_000;
const MAX_GOSSIP_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INBOUND_SESSIONS: usize = 64;
const MAX_INBOUND_SESSIONS_PER_IP: usize = 8;
const MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW: usize = 24;
const INBOUND_ACCEPT_RATE_WINDOW_MS: u64 = 10_000;
const PEER_QUEUE_SIZE: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const TRANSACTION_ACK_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const JOIN_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JOIN_RESPONSE_ENVELOPES: usize = 16;
const MAX_PEER_VERIFICATION_ENVELOPES: usize = 8;
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
static NODE_SIGNING_KEYS: OnceLock<StdMutex<BTreeMap<String, SigningKey>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
struct PeerStatus {
    height: u64,
    tip_hash: String,
    time_ms: u64,
    mempool_count: usize,
    mempool_root: String,
    mempool_txs: Vec<String>,
    request_snapshot: bool,
    push_snapshot: bool,
}

impl PeerStatus {
    fn new(height: u64, tip_hash: String) -> Self {
        Self::with_time(height, tip_hash, now_ms())
    }

    fn with_time(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            mempool_count: 0,
            mempool_root: String::new(),
            mempool_txs: Vec::new(),
            request_snapshot: false,
            push_snapshot: false,
        }
    }

    fn from_envelope(
        height: u64,
        tip_hash: String,
        mempool_count: usize,
        mempool_root: String,
        mempool_txs: Vec<String>,
        time_ms: u64,
    ) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            mempool_count,
            mempool_root,
            mempool_txs,
            request_snapshot: false,
            push_snapshot: false,
        }
    }

    fn with_snapshot_request(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            mempool_count: 0,
            mempool_root: String::new(),
            mempool_txs: Vec::new(),
            request_snapshot: true,
            push_snapshot: false,
        }
    }

    fn with_snapshot_push(height: u64, tip_hash: String, time_ms: u64) -> Self {
        Self {
            height,
            tip_hash,
            time_ms,
            mempool_count: 0,
            mempool_root: String::new(),
            mempool_txs: Vec::new(),
            request_snapshot: false,
            push_snapshot: true,
        }
    }
}

type OutboundBatch = Vec<GossipEnvelope>;

#[derive(Clone)]
pub struct GossipNetwork {
    inner: Arc<GossipNetworkInner>,
}

struct GossipNetworkInner {
    node: SharedNode,
    peers: SharedPeerBook,
    listen_addr: SocketAddr,
    p2p_announce_addr: Mutex<Option<SocketAddr>>,
    node_id: String,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    sessions: Mutex<BTreeMap<String, mpsc::Sender<OutboundBatch>>>,
    tx_delivery: Mutex<BTreeMap<String, PeerTransactionDelivery>>,
    inbound_limiter: Arc<StdMutex<InboundConnectionLimiter>>,
    metrics: P2pMetricsCounters,
}

#[derive(Default)]
struct PeerTransactionDelivery {
    accepted: BTreeSet<String>,
    rejected: BTreeSet<String>,
    sent: BTreeMap<String, Instant>,
}

#[derive(Default)]
struct InboundConnectionLimiter {
    active: usize,
    peers: BTreeMap<IpAddr, InboundPeerLimit>,
}

#[derive(Default)]
struct InboundPeerLimit {
    active: usize,
    accepted_at_ms: VecDeque<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InboundSessionRejection {
    GlobalActive,
    PeerActive,
    PeerRate,
}

impl InboundSessionRejection {
    fn label(self) -> &'static str {
        match self {
            Self::GlobalActive => "global active inbound session limit",
            Self::PeerActive => "per-IP active inbound session limit",
            Self::PeerRate => "per-IP inbound accept rate limit",
        }
    }
}

struct InboundSessionPermit {
    limiter: Arc<StdMutex<InboundConnectionLimiter>>,
    ip: IpAddr,
}

struct PeerVerificationSession<'a> {
    writer: &'a mut OwnedWriteHalf,
    reader: &'a mut LimitedLineReader<OwnedReadHalf>,
    connection_label: &'a str,
}

impl Drop for InboundSessionPermit {
    fn drop(&mut self) {
        if let Ok(mut limiter) = self.limiter.lock() {
            limiter.release(self.ip);
        }
    }
}

impl InboundConnectionLimiter {
    fn try_acquire(
        &mut self,
        ip: IpAddr,
        now_ms: u64,
    ) -> std::result::Result<(), InboundSessionRejection> {
        self.prune_stale_accepts(now_ms);
        if self.active >= MAX_INBOUND_SESSIONS {
            return Err(InboundSessionRejection::GlobalActive);
        }

        let peer = self.peers.entry(ip).or_default();
        prune_peer_accepts(peer, now_ms);
        if peer.active >= MAX_INBOUND_SESSIONS_PER_IP {
            return Err(InboundSessionRejection::PeerActive);
        }
        if peer.accepted_at_ms.len() >= MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW {
            return Err(InboundSessionRejection::PeerRate);
        }

        peer.active += 1;
        peer.accepted_at_ms.push_back(now_ms);
        self.active += 1;
        Ok(())
    }

    fn release(&mut self, ip: IpAddr) {
        if self.active > 0 {
            self.active -= 1;
        }
        if let Some(peer) = self.peers.get_mut(&ip) {
            if peer.active > 0 {
                peer.active -= 1;
            }
        }
    }

    fn prune_stale_accepts(&mut self, now_ms: u64) {
        self.peers.retain(|_, peer| {
            prune_peer_accepts(peer, now_ms);
            peer.active > 0 || !peer.accepted_at_ms.is_empty()
        });
    }
}

fn prune_peer_accepts(peer: &mut InboundPeerLimit, now_ms: u64) {
    while peer.accepted_at_ms.front().is_some_and(|accepted_ms| {
        now_ms.saturating_sub(*accepted_ms) >= INBOUND_ACCEPT_RATE_WINDOW_MS
    }) {
        peer.accepted_at_ms.pop_front();
    }
}

#[derive(Default)]
struct P2pMetricsCounters {
    inbound_sessions_started: AtomicU64,
    inbound_sessions_rejected: AtomicU64,
    outbound_connect_attempts: AtomicU64,
    outbound_connect_successes: AtomicU64,
    outbound_connect_failures: AtomicU64,
    outbound_sessions_started: AtomicU64,
    sessions_closed: AtomicU64,
    session_failures: AtomicU64,
    quiet_disconnects: AtomicU64,
    envelopes_received: AtomicU64,
    hello_envelopes_received: AtomicU64,
    peer_status_envelopes_received: AtomicU64,
    inventory_envelopes_received: AtomicU64,
    data_envelopes_received: AtomicU64,
    control_envelopes_received: AtomicU64,
    bytes_received: AtomicU64,
    parse_errors: AtomicU64,
    empty_frames: AtomicU64,
    self_peer_rejections: AtomicU64,
    self_peer_skips: AtomicU64,
    outbound_queue_full: AtomicU64,
    outbound_queue_closed: AtomicU64,
    transaction_ack_envelopes_sent: AtomicU64,
    transaction_ack_envelopes_received: AtomicU64,
    transactions_accepted_sent: AtomicU64,
    transactions_accepted_received: AtomicU64,
    transactions_rejected_sent: AtomicU64,
    transactions_rejected_received: AtomicU64,
    transaction_retries_sent: AtomicU64,
    mempool_statuses_received: AtomicU64,
    mempool_status_transactions_received: AtomicU64,
    mempool_status_mismatches: AtomicU64,
    mempool_transaction_requests_sent: AtomicU64,
    mempool_transaction_request_signatures_sent: AtomicU64,
    last_session_failure: StdMutex<Option<String>>,
    last_empty_frame_remote: StdMutex<Option<String>>,
    last_parse_error: StdMutex<Option<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct P2pMetrics {
    pub inbound_sessions_started: u64,
    pub inbound_sessions_rejected: u64,
    pub outbound_connect_attempts: u64,
    pub outbound_connect_successes: u64,
    pub outbound_connect_failures: u64,
    pub outbound_sessions_started: u64,
    pub sessions_closed: u64,
    pub session_failures: u64,
    pub quiet_disconnects: u64,
    pub envelopes_received: u64,
    pub hello_envelopes_received: u64,
    pub peer_status_envelopes_received: u64,
    pub inventory_envelopes_received: u64,
    pub data_envelopes_received: u64,
    pub control_envelopes_received: u64,
    pub bytes_received: u64,
    pub parse_errors: u64,
    pub empty_frames: u64,
    pub self_peer_rejections: u64,
    pub self_peer_skips: u64,
    pub outbound_queue_full: u64,
    pub outbound_queue_closed: u64,
    pub transaction_ack_envelopes_sent: u64,
    pub transaction_ack_envelopes_received: u64,
    pub transactions_accepted_sent: u64,
    pub transactions_accepted_received: u64,
    pub transactions_rejected_sent: u64,
    pub transactions_rejected_received: u64,
    pub transaction_retries_sent: u64,
    pub mempool_statuses_received: u64,
    pub mempool_status_transactions_received: u64,
    pub mempool_status_mismatches: u64,
    pub mempool_transaction_requests_sent: u64,
    pub mempool_transaction_request_signatures_sent: u64,
    pub transaction_ack_pending: u64,
    pub last_session_failure: Option<String>,
    pub last_empty_frame_remote: Option<String>,
    pub last_parse_error: Option<String>,
}

impl P2pMetricsCounters {
    fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn add(counter: &AtomicU64, amount: u64) {
        counter.fetch_add(amount, Ordering::Relaxed);
    }

    fn set_last(target: &StdMutex<Option<String>>, value: impl Into<String>) {
        if let Ok(mut last) = target.lock() {
            *last = Some(value.into());
        }
    }

    fn snapshot(&self) -> P2pMetrics {
        P2pMetrics {
            inbound_sessions_started: self.inbound_sessions_started.load(Ordering::Relaxed),
            inbound_sessions_rejected: self.inbound_sessions_rejected.load(Ordering::Relaxed),
            outbound_connect_attempts: self.outbound_connect_attempts.load(Ordering::Relaxed),
            outbound_connect_successes: self.outbound_connect_successes.load(Ordering::Relaxed),
            outbound_connect_failures: self.outbound_connect_failures.load(Ordering::Relaxed),
            outbound_sessions_started: self.outbound_sessions_started.load(Ordering::Relaxed),
            sessions_closed: self.sessions_closed.load(Ordering::Relaxed),
            session_failures: self.session_failures.load(Ordering::Relaxed),
            quiet_disconnects: self.quiet_disconnects.load(Ordering::Relaxed),
            envelopes_received: self.envelopes_received.load(Ordering::Relaxed),
            hello_envelopes_received: self.hello_envelopes_received.load(Ordering::Relaxed),
            peer_status_envelopes_received: self
                .peer_status_envelopes_received
                .load(Ordering::Relaxed),
            inventory_envelopes_received: self.inventory_envelopes_received.load(Ordering::Relaxed),
            data_envelopes_received: self.data_envelopes_received.load(Ordering::Relaxed),
            control_envelopes_received: self.control_envelopes_received.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            empty_frames: self.empty_frames.load(Ordering::Relaxed),
            self_peer_rejections: self.self_peer_rejections.load(Ordering::Relaxed),
            self_peer_skips: self.self_peer_skips.load(Ordering::Relaxed),
            outbound_queue_full: self.outbound_queue_full.load(Ordering::Relaxed),
            outbound_queue_closed: self.outbound_queue_closed.load(Ordering::Relaxed),
            transaction_ack_envelopes_sent: self
                .transaction_ack_envelopes_sent
                .load(Ordering::Relaxed),
            transaction_ack_envelopes_received: self
                .transaction_ack_envelopes_received
                .load(Ordering::Relaxed),
            transactions_accepted_sent: self.transactions_accepted_sent.load(Ordering::Relaxed),
            transactions_accepted_received: self
                .transactions_accepted_received
                .load(Ordering::Relaxed),
            transactions_rejected_sent: self.transactions_rejected_sent.load(Ordering::Relaxed),
            transactions_rejected_received: self
                .transactions_rejected_received
                .load(Ordering::Relaxed),
            transaction_retries_sent: self.transaction_retries_sent.load(Ordering::Relaxed),
            mempool_statuses_received: self.mempool_statuses_received.load(Ordering::Relaxed),
            mempool_status_transactions_received: self
                .mempool_status_transactions_received
                .load(Ordering::Relaxed),
            mempool_status_mismatches: self.mempool_status_mismatches.load(Ordering::Relaxed),
            mempool_transaction_requests_sent: self
                .mempool_transaction_requests_sent
                .load(Ordering::Relaxed),
            mempool_transaction_request_signatures_sent: self
                .mempool_transaction_request_signatures_sent
                .load(Ordering::Relaxed),
            transaction_ack_pending: 0,
            last_session_failure: self
                .last_session_failure
                .lock()
                .ok()
                .and_then(|last| last.clone()),
            last_empty_frame_remote: self
                .last_empty_frame_remote
                .lock()
                .ok()
                .and_then(|last| last.clone()),
            last_parse_error: self
                .last_parse_error
                .lock()
                .ok()
                .and_then(|last| last.clone()),
        }
    }
}

impl GossipNetwork {
    #[cfg(test)]
    pub(crate) fn new_for_tests(node: SharedNode, peers: SharedPeerBook) -> Self {
        Self {
            inner: Arc::new(GossipNetworkInner {
                node,
                peers,
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                p2p_announce_addr: Mutex::new(None),
                node_id: new_node_id(),
                accept_task: Mutex::new(None),
                sessions: Mutex::new(BTreeMap::new()),
                tx_delivery: Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(StdMutex::new(InboundConnectionLimiter::default())),
                metrics: P2pMetricsCounters::default(),
            }),
        }
    }

    pub async fn start(
        node: SharedNode,
        peers: SharedPeerBook,
        addr: SocketAddr,
        p2p_announce_addr: Option<SocketAddr>,
        accept_inbound: bool,
    ) -> Result<Self> {
        let network = Self {
            inner: Arc::new(GossipNetworkInner {
                node,
                peers,
                listen_addr: addr,
                p2p_announce_addr: Mutex::new(p2p_announce_addr),
                node_id: new_node_id(),
                accept_task: Mutex::new(None),
                sessions: Mutex::new(BTreeMap::new()),
                tx_delivery: Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(StdMutex::new(InboundConnectionLimiter::default())),
                metrics: P2pMetricsCounters::default(),
            }),
        };

        if accept_inbound {
            network.set_accept_inbound(true).await?;
        }
        tokio::spawn(outbound_supervisor(network.clone()));
        network.ensure_outbound_sessions().await;
        Ok(network)
    }

    pub async fn set_accept_inbound(&self, enabled: bool) -> Result<()> {
        let mut accept_task = self.inner.accept_task.lock().await;
        if enabled {
            if accept_task.is_some() {
                return Ok(());
            }
            let listener = TcpListener::bind(self.inner.listen_addr)
                .await
                .with_context(|| format!("binding p2p listener on {}", self.inner.listen_addr))?;
            *accept_task = Some(tokio::spawn(accept_loop(self.clone(), listener)));
        } else if let Some(task) = accept_task.take() {
            task.abort();
        }
        Ok(())
    }

    pub async fn accepts_inbound(&self) -> bool {
        self.inner.accept_task.lock().await.is_some()
    }

    pub async fn set_p2p_announce_addr(&self, addr: Option<SocketAddr>) {
        *self.inner.p2p_announce_addr.lock().await = addr;
    }

    async fn advertised_addr(&self) -> Option<SocketAddr> {
        if !self.accepts_inbound().await {
            return None;
        }
        Some((*self.inner.p2p_announce_addr.lock().await).unwrap_or(self.inner.listen_addr))
    }

    async fn self_filter_addr(&self) -> Option<SocketAddr> {
        if let Some(addr) = *self.inner.p2p_announce_addr.lock().await {
            return Some(addr);
        }
        self.accepts_inbound()
            .await
            .then_some(self.inner.listen_addr)
    }

    async fn is_self_peer(&self, address: &str) -> bool {
        is_self_peer_address_for(
            address,
            self.inner.listen_addr,
            self.self_filter_addr().await,
        )
    }

    pub fn metrics(&self) -> P2pMetrics {
        let mut metrics = self.inner.metrics.snapshot();
        metrics.transaction_ack_pending = self
            .inner
            .tx_delivery
            .try_lock()
            .map(|delivery| {
                delivery
                    .values()
                    .map(|peer_delivery| peer_delivery.sent.len() as u64)
                    .sum()
            })
            .unwrap_or(0);
        metrics
    }

    fn try_acquire_inbound_session(
        &self,
        ip: IpAddr,
    ) -> std::result::Result<InboundSessionPermit, InboundSessionRejection> {
        self.inner
            .inbound_limiter
            .lock()
            .expect("inbound limiter mutex poisoned")
            .try_acquire(ip, now_ms())?;
        Ok(InboundSessionPermit {
            limiter: Arc::clone(&self.inner.inbound_limiter),
            ip,
        })
    }

    pub async fn broadcast(&self, envelopes: Vec<GossipEnvelope>) -> Result<()> {
        let envelopes = self.prepare_gossip(envelopes).await;
        if envelopes.is_empty() {
            return Ok(());
        }

        let sessions = self.inner.sessions.lock().await.clone();
        for (peer, sender) in sessions {
            if self.inner.peers.lock().await.is_banned(&peer) {
                continue;
            }
            match sender.try_send(envelopes.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    P2pMetricsCounters::inc(&self.inner.metrics.outbound_queue_full);
                    self.inner
                        .peers
                        .lock()
                        .await
                        .record_error(&peer, "outbound gossip queue is full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    P2pMetricsCounters::inc(&self.inner.metrics.outbound_queue_closed);
                }
            }
        }
        Ok(())
    }

    async fn record_sent_transactions(&self, peer: &str, envelopes: &[GossipEnvelope]) {
        let now = Instant::now();
        let mut delivery = self.inner.tx_delivery.lock().await;
        let peer_delivery = delivery.entry(peer.to_string()).or_default();
        for (signature, _) in transactions_in_envelopes(envelopes) {
            if peer_delivery.accepted.contains(&signature)
                || peer_delivery.rejected.contains(&signature)
            {
                continue;
            }
            peer_delivery.sent.insert(signature, now);
        }
    }

    async fn record_transaction_ack(
        &self,
        peer: &str,
        accepted: &[String],
        rejected: &[TransactionRejection],
    ) {
        if !accepted.is_empty() || !rejected.is_empty() {
            P2pMetricsCounters::inc(&self.inner.metrics.transaction_ack_envelopes_received);
            P2pMetricsCounters::add(
                &self.inner.metrics.transactions_accepted_received,
                accepted.len() as u64,
            );
            P2pMetricsCounters::add(
                &self.inner.metrics.transactions_rejected_received,
                rejected.len() as u64,
            );
        }
        let mut delivery = self.inner.tx_delivery.lock().await;
        let peer_delivery = delivery.entry(peer.to_string()).or_default();
        for signature in accepted {
            peer_delivery.accepted.insert(signature.clone());
            peer_delivery.rejected.remove(signature);
            peer_delivery.sent.remove(signature);
        }
        for rejection in rejected {
            peer_delivery.rejected.insert(rejection.signature.clone());
            peer_delivery.accepted.remove(&rejection.signature);
            peer_delivery.sent.remove(&rejection.signature);
        }
        let last_rejection = rejected.last().map(|rejection| {
            format!(
                "peer rejected transaction {}: {}",
                short_signature(&rejection.signature),
                rejection.reason
            )
        });
        drop(delivery);
        if let Some(reason) = last_rejection {
            self.inner
                .peers
                .lock()
                .await
                .record_transaction_rejection(peer, reason);
        }
    }

    async fn pending_transactions_for_retry(&self, peer: &str) -> Vec<Transaction> {
        let pending = self.inner.node.lock().await.pending_transactions();
        let pending_signatures = pending
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let now = Instant::now();
        let mut delivery = self.inner.tx_delivery.lock().await;
        let peer_delivery = delivery.entry(peer.to_string()).or_default();
        peer_delivery
            .sent
            .retain(|signature, _| pending_signatures.contains(signature));

        let retry = pending
            .into_iter()
            .filter(|tx| {
                let signature = tx.signature();
                if peer_delivery.accepted.contains(signature)
                    || peer_delivery.rejected.contains(signature)
                {
                    return false;
                }
                peer_delivery.sent.get(signature).is_none_or(|last_sent| {
                    now.duration_since(*last_sent) >= TRANSACTION_ACK_RETRY_INTERVAL
                })
            })
            .collect::<Vec<_>>();

        for tx in &retry {
            peer_delivery.sent.insert(tx.signature().to_string(), now);
        }
        retry
    }

    async fn prepare_gossip(&self, envelopes: Vec<GossipEnvelope>) -> Vec<GossipEnvelope> {
        let mut full_transactions = Vec::new();
        let mut txs = Vec::new();
        let mut blocks = Vec::new();
        let mut passthrough = Vec::new();

        for envelope in envelopes {
            match envelope {
                GossipEnvelope::Transaction(tx) => {
                    txs.push(tx.signature().to_string());
                    full_transactions.push(tx);
                }
                GossipEnvelope::Transactions { transactions } => {
                    txs.extend(
                        transactions
                            .iter()
                            .map(|tx| tx.signature().to_string())
                            .collect::<Vec<_>>(),
                    );
                    passthrough.extend(transaction_batch_envelopes(transactions));
                }
                GossipEnvelope::Block(block) => blocks.push(BlockInventory {
                    height: block.height,
                    hash: block.hash,
                }),
                GossipEnvelope::Blocks { blocks: batch } => {
                    blocks.extend(batch.into_iter().map(|block| BlockInventory {
                        height: block.height,
                        hash: block.hash,
                    }));
                }
                GossipEnvelope::Inventory {
                    txs: inv_txs,
                    blocks: inv_blocks,
                } => {
                    txs.extend(inv_txs);
                    blocks.extend(inv_blocks);
                }
                other => passthrough.push(other),
            }
        }

        if !full_transactions.is_empty() {
            passthrough.extend(transaction_batch_envelopes(full_transactions));
        }

        txs.sort();
        txs.dedup();
        blocks.sort_by(|left, right| {
            left.height
                .cmp(&right.height)
                .then_with(|| left.hash.cmp(&right.hash))
        });
        blocks.dedup_by(|left, right| left.hash == right.hash);

        if !txs.is_empty() || !blocks.is_empty() {
            passthrough.push(GossipEnvelope::Inventory { txs, blocks });
        }
        passthrough
    }

    pub async fn peer_exchange(&self) -> GossipEnvelope {
        let advertised_addr = self.advertised_addr().await;
        let self_filter_addr = self.self_filter_addr().await;
        let self_addr = advertised_addr.map(|addr| addr.to_string());
        let peers = self
            .inner
            .peers
            .lock()
            .await
            .addresses_except(self_addr.as_deref().unwrap_or(""))
            .into_iter()
            .filter(|peer| peer.parse::<SocketAddr>().is_ok())
            .filter(|peer| {
                !is_self_peer_address_for(peer, self.inner.listen_addr, self_filter_addr)
            })
            .collect::<Vec<_>>();
        GossipEnvelope::PeerList {
            peers: self_addr.into_iter().chain(peers.into_iter()).collect(),
        }
    }

    async fn ensure_outbound_sessions(&self) {
        let addresses = self
            .inner
            .peers
            .lock()
            .await
            .connectable_addresses_at(crate::app::now_ms());
        let address_set = addresses.iter().cloned().collect::<BTreeSet<_>>();
        let self_filter_addr = self.self_filter_addr().await;
        let mut sessions = self.inner.sessions.lock().await;
        sessions.retain(|peer, _| {
            let keep = address_set.contains(peer)
                && !is_self_peer_address_for(peer, self.inner.listen_addr, self_filter_addr);
            if !keep {
                P2pMetricsCounters::inc(&self.inner.metrics.self_peer_skips);
            }
            keep
        });
        for peer in addresses {
            if is_self_peer_address_for(&peer, self.inner.listen_addr, self_filter_addr) {
                P2pMetricsCounters::inc(&self.inner.metrics.self_peer_skips);
                continue;
            }
            if sessions.contains_key(&peer) {
                continue;
            }

            let (sender, receiver) = mpsc::channel(PEER_QUEUE_SIZE);
            sessions.insert(peer.clone(), sender);
            tokio::spawn(outbound_session(self.clone(), peer, receiver));
        }
    }

    async fn forward_outbox(&self) {
        let outbox = self.inner.node.lock().await.drain_outbox();
        if let Err(error) = self.broadcast(outbox).await {
            if debug_logging_enabled() {
                eprintln!("p2p rebroadcast failed: {error:#}");
            }
        }
    }
}

async fn accept_loop(network: GossipNetwork, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, remote_addr)) => {
                let network = network.clone();
                let permit = match network.try_acquire_inbound_session(remote_addr.ip()) {
                    Ok(permit) => permit,
                    Err(rejection) => {
                        P2pMetricsCounters::inc(&network.inner.metrics.inbound_sessions_rejected);
                        P2pMetricsCounters::set_last(
                            &network.inner.metrics.last_session_failure,
                            format!("{remote_addr}: {}", rejection.label()),
                        );
                        if debug_logging_enabled() {
                            eprintln!(
                                "p2p inbound connection from {remote_addr} rejected: {}",
                                rejection.label()
                            );
                        }
                        drop(stream);
                        continue;
                    }
                };
                P2pMetricsCounters::inc(&network.inner.metrics.inbound_sessions_started);
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = session_loop(
                        network.clone(),
                        stream,
                        remote_addr,
                        None,
                        mpsc::channel(1).1,
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            P2pMetricsCounters::inc(&network.inner.metrics.sessions_closed);
                        }
                        Err(error) if is_quiet_disconnect(&error) => {
                            P2pMetricsCounters::inc(&network.inner.metrics.quiet_disconnects);
                        }
                        Err(error) => {
                            P2pMetricsCounters::inc(&network.inner.metrics.session_failures);
                            P2pMetricsCounters::set_last(
                                &network.inner.metrics.last_session_failure,
                                format!("{remote_addr}: {error:#}"),
                            );
                            if debug_logging_enabled() {
                                eprintln!(
                                    "p2p inbound connection from {remote_addr} failed: {error:#}"
                                );
                            }
                        }
                    }
                });
            }
            Err(error) if debug_logging_enabled() => eprintln!("p2p accept failed: {error:#}"),
            Err(_) => {}
        }
    }
}

async fn outbound_supervisor(network: GossipNetwork) {
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        network.ensure_outbound_sessions().await;
    }
}

async fn outbound_session(
    network: GossipNetwork,
    peer: String,
    mut receiver: mpsc::Receiver<OutboundBatch>,
) {
    let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
    loop {
        let self_filter_addr = network.self_filter_addr().await;
        if !peer_is_configured_outbound(&network, &peer).await
            || is_self_peer_address_for(&peer, network.inner.listen_addr, self_filter_addr)
        {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        if network.inner.peers.lock().await.is_banned(&peer) {
            sleep(MAX_RECONNECT_DELAY).await;
            continue;
        }
        P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_attempts);
        let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(&peer)).await {
            Ok(Ok(stream)) => {
                P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_successes);
                stream
            }
            Ok(Err(error)) => {
                P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_failures);
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, format!("connecting to peer {peer}: {error}"));
                sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
            Err(_) => {
                P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_failures);
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, format!("connecting to peer {peer}: timeout"));
                sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
        };

        reconnect_delay = INITIAL_RECONNECT_DELAY;
        let remote_addr = stream.peer_addr().unwrap_or_else(|_| {
            peer.parse()
                .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)))
        });
        P2pMetricsCounters::inc(&network.inner.metrics.outbound_sessions_started);
        let result = session_loop(
            network.clone(),
            stream,
            remote_addr,
            Some(peer.clone()),
            receiver,
        )
        .await;
        match result {
            Ok(()) => {
                P2pMetricsCounters::inc(&network.inner.metrics.sessions_closed);
            }
            Err(error) if is_quiet_disconnect(&error) => {
                P2pMetricsCounters::inc(&network.inner.metrics.quiet_disconnects);
            }
            Err(error) => {
                P2pMetricsCounters::inc(&network.inner.metrics.session_failures);
                let message = format!("{error:#}");
                P2pMetricsCounters::set_last(
                    &network.inner.metrics.last_session_failure,
                    format!("{peer}: {message}"),
                );
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, message.clone());
                if debug_logging_enabled() {
                    eprintln!("p2p session with {peer} failed: {message}");
                }
            }
        }

        let (sender, next_receiver) = mpsc::channel(PEER_QUEUE_SIZE);
        receiver = next_receiver;
        if !peer_is_configured_outbound(&network, &peer).await {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        network
            .inner
            .sessions
            .lock()
            .await
            .insert(peer.clone(), sender);
        sleep(reconnect_delay).await;
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }
}

async fn session_loop(
    network: GossipNetwork,
    stream: TcpStream,
    remote_addr: SocketAddr,
    stable_peer: Option<String>,
    mut outbound: mpsc::Receiver<OutboundBatch>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let connection_label = stable_peer
        .as_ref()
        .map(|peer| format!("outbound {peer}"))
        .unwrap_or_else(|| format!("inbound {remote_addr}"));
    let advertised_addr = network.advertised_addr().await;
    let hello = network.inner.node.lock().await.hello(
        advertised_addr.map(|addr| addr.to_string()),
        Some(network.inner.node_id.clone()),
    );
    write_envelope(&mut writer, &hello).await?;
    let mut reader = LimitedLineReader::new(reader);
    let mut sync_tick = interval_at(
        Instant::now() + SESSION_SYNC_INTERVAL,
        SESSION_SYNC_INTERVAL,
    );
    let mut outbound_closed = false;
    let mut peer_status: Option<PeerStatus> = None;
    let is_outbound_session = stable_peer.is_some();
    let mut known_peer = stable_peer;

    if known_peer.is_some() {
        if let Ok(Ok(Some(envelope))) = timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(&network, &connection_label, &mut reader),
        )
        .await
        {
            if let GossipEnvelope::Hello(hello) = envelope {
                peer_status = Some(
                    process_hello_with_verification(
                        &network,
                        &mut writer,
                        &mut reader,
                        &connection_label,
                        remote_addr,
                        &mut known_peer,
                        hello,
                    )
                    .await?,
                );
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
                maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
            } else if let GossipEnvelope::PeerStatus {
                height,
                tip_hash,
                time_ms,
                mempool_count,
                mempool_root,
                mempool_txs,
            } = envelope
            {
                let status = PeerStatus::from_envelope(
                    height,
                    tip_hash,
                    mempool_count,
                    mempool_root,
                    mempool_txs,
                    time_ms,
                );
                record_peer_status(&network, &known_peer, remote_addr, &status).await;
                maybe_request_mempool_catchup(
                    &network,
                    &mut writer,
                    &known_peer,
                    remote_addr,
                    &status,
                )
                .await?;
                peer_status = Some(status);
                maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
            } else if respond_to_peer_verification_challenge(&network, &mut writer, &envelope)
                .await?
            {
                if known_peer.is_none() {
                    return Ok(());
                }
            } else {
                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                )
                .await?;
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }

    loop {
        tokio::select! {
            maybe_batch = outbound.recv(), if !outbound_closed => {
                match maybe_batch {
                    Some(batch) => {
                        let payload = envelopes_for_peer(
                            Some(&network.inner.node),
                            peer_status.clone(),
                            &batch,
                        ).await;
                        write_payload(&mut writer, &payload).await?;
                        if let Some(peer) = &known_peer {
                            network.record_sent_transactions(peer, &payload).await;
                            network.inner.peers.lock().await.record_sent(peer, payload.len() as u64);
                        }
                    }
                    None => outbound_closed = true,
                }
            }
            _ = sync_tick.tick() => {
                let status = network.inner.node.lock().await.peer_status();
                write_envelope(&mut writer, &status).await?;
                if let Some(status) = peer_status.as_mut() {
                    if let Some(updated_status) = push_catchup_to_peer(&network, &mut writer, status).await? {
                        *status = updated_status;
                    }
                }
                if let Some(peer) = &known_peer {
                    let transactions = network.pending_transactions_for_retry(peer).await;
                    if !transactions.is_empty() {
                        let retry_envelopes = transaction_batch_envelopes(transactions);
                        for retry in &retry_envelopes {
                            write_envelope(&mut writer, retry).await?;
                        }
                        P2pMetricsCounters::inc(&network.inner.metrics.transaction_retries_sent);
                        network
                            .inner
                            .peers
                            .lock()
                            .await
                            .record_sent(peer, retry_envelopes.len() as u64);
                    }
                }
            }
            envelope = read_session_envelope(&network, &connection_label, &mut reader) => {
                let Some(envelope) = envelope? else {
                    return Ok(());
                };
                if let GossipEnvelope::Hello(hello) = envelope {
                    peer_status = Some(
                        process_hello_with_verification(
                            &network,
                            &mut writer,
                            &mut reader,
                            &connection_label,
                            remote_addr,
                            &mut known_peer,
                            hello,
                        )
                        .await?,
                    );
                    if is_outbound_session && known_peer.is_none() {
                        return Ok(());
                    }
                    maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
                    continue;
                }
                if let GossipEnvelope::PeerStatus {
                    height,
                    tip_hash,
                    time_ms,
                    mempool_count,
                    mempool_root,
                    mempool_txs,
                } = &envelope
                {
                    let status = PeerStatus::from_envelope(
                        *height,
                        tip_hash.clone(),
                        *mempool_count,
                        mempool_root.clone(),
                        mempool_txs.clone(),
                        *time_ms,
                    );
                    record_peer_status(&network, &known_peer, remote_addr, &status).await;
                    maybe_request_mempool_catchup(&network, &mut writer, &known_peer, remote_addr, &status)
                        .await?;
                    peer_status = Some(status);
                    maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
                    continue;
                }

                if respond_to_peer_verification_challenge(&network, &mut writer, &envelope).await? {
                    if known_peer.is_none() {
                        return Ok(());
                    }
                    continue;
                }
                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                ).await?;
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

async fn respond_to_peer_verification_challenge(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    envelope: &GossipEnvelope,
) -> Result<bool> {
    let GossipEnvelope::PeerVerificationChallenge { address, nonce } = envelope else {
        return Ok(false);
    };
    if let Some(response) = peer_verification_response(network, address, nonce) {
        write_envelope(writer, &response).await?;
    }
    Ok(true)
}

async fn peer_is_configured_outbound(network: &GossipNetwork, peer: &str) -> bool {
    network
        .inner
        .peers
        .lock()
        .await
        .is_configured_outbound(peer)
}

async fn process_envelope(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    envelope: GossipEnvelope,
) -> Result<()> {
    match envelope {
        GossipEnvelope::Hello(hello) => {
            let _ = process_hello(network, remote_addr, known_peer, hello).await?;
        }
        GossipEnvelope::ChainSnapshotRequest => {
            let snapshot = network.inner.node.lock().await.chain_snapshot();
            write_envelope(writer, &GossipEnvelope::ChainSnapshot(snapshot)).await?;
        }
        GossipEnvelope::BlockRangeRequest { from_height, limit } => {
            let blocks = network
                .inner
                .node
                .lock()
                .await
                .blocks_from(from_height, limit.min(MAX_BLOCK_BATCH));
            write_envelope(writer, &GossipEnvelope::Blocks { blocks }).await?;
        }
        GossipEnvelope::TransactionRequest { signatures } => {
            let transactions = network
                .inner
                .node
                .lock()
                .await
                .transactions_by_signature(&signatures);
            for envelope in transaction_batch_envelopes(transactions) {
                write_envelope(writer, &envelope).await?;
            }
        }
        GossipEnvelope::BlockRequest { hashes } => {
            let blocks = network.inner.node.lock().await.blocks_by_hash(&hashes);
            if !blocks.is_empty() {
                write_envelope(writer, &GossipEnvelope::Blocks { blocks }).await?;
            }
        }
        GossipEnvelope::Inventory { txs, blocks } => {
            let requests = network
                .inner
                .node
                .lock()
                .await
                .missing_inventory_requests(&txs, &blocks);
            write_payload(writer, &requests).await?;
        }
        GossipEnvelope::PeerAnnouncement { address, node_id } => {
            let peer = normalize_advertised_peer(&address, remote_addr)?;
            if network.is_self_peer(&peer).await {
                P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
                forget_stale_self_peer(network, known_peer).await;
            } else if node_id.is_some() && debug_logging_enabled() {
                eprintln!("p2p peer announcement for {peer} ignored until hello verification");
            }
            let snapshot = network.inner.node.lock().await.chain_snapshot();
            write_envelope(writer, &GossipEnvelope::ChainSnapshot(snapshot)).await?;
        }
        GossipEnvelope::PeerVerificationChallenge { address, nonce } => {
            if let Some(response) = peer_verification_response(network, &address, &nonce) {
                write_envelope(writer, &response).await?;
            }
        }
        GossipEnvelope::PeerVerificationResponse { .. } => {}
        GossipEnvelope::PeerList { peers } => {
            apply_peer_list(network, remote_addr, peers).await?;
        }
        GossipEnvelope::Transaction(tx) => {
            process_transactions(network, writer, remote_addr, known_peer, vec![tx]).await?;
        }
        GossipEnvelope::Transactions { transactions } => {
            process_transactions(network, writer, remote_addr, known_peer, transactions).await?;
        }
        GossipEnvelope::TransactionAck { accepted, rejected } => {
            let peer = known_peer
                .clone()
                .unwrap_or_else(|| remote_addr.to_string());
            network
                .record_transaction_ack(&peer, &accepted, &rejected)
                .await;
            record_inbound_result(network, known_peer, remote_addr, Ok(())).await;
        }
        GossipEnvelope::Block(block) => {
            let adjusted_time_ms = network_adjusted_time_ms(network).await;
            let needs_vdf = {
                let node = network.inner.node.lock().await;
                node.block_requires_vdf_verification_at(&block, adjusted_time_ms)
            };
            let result = match needs_vdf {
                Ok(false) => Ok(()),
                Ok(true) => match verify_block_vdf(block).await {
                    Ok(block) => network
                        .inner
                        .node
                        .lock()
                        .await
                        .receive_preverified_block_at(block, adjusted_time_ms),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
        GossipEnvelope::Blocks { blocks } => {
            let adjusted_time_ms = network_adjusted_time_ms(network).await;
            let local_ledger = network.inner.node.lock().await.clone_ledger();
            let result =
                match validate_blocks_extension(local_ledger, blocks, adjusted_time_ms).await {
                    Ok(ledger) => network
                        .inner
                        .node
                        .lock()
                        .await
                        .import_verified_ledger(ledger)
                        .map(|_| ()),
                    Err(error) => Err(error),
                };
            let request_snapshot = result.as_ref().err().is_some_and(is_possible_fork_error);
            record_inbound_result(network, known_peer, remote_addr, result).await;
            if request_snapshot {
                write_envelope(writer, &GossipEnvelope::ChainSnapshotRequest).await?;
            }
            network.forward_outbox().await;
        }
        GossipEnvelope::ChainSnapshot(snapshot) => {
            let adjusted_time_ms = network_adjusted_time_ms(network).await;
            let local_ledger = network.inner.node.lock().await.clone_ledger();
            let result =
                match validate_snapshot_extension(local_ledger, snapshot, adjusted_time_ms).await {
                    Ok(ledger) => network
                        .inner
                        .node
                        .lock()
                        .await
                        .import_verified_ledger(ledger)
                        .map(|_| ()),
                    Err(error) => Err(error),
                };
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
        other => {
            let result = network.inner.node.lock().await.receive(other);
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
    }
    Ok(())
}

async fn process_transactions(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    remote_addr: SocketAddr,
    known_peer: &Option<String>,
    transactions: Vec<Transaction>,
) -> Result<()> {
    let (accepted, rejected) = {
        let mut node = network.inner.node.lock().await;
        receive_transactions_for_ack(&mut node, transactions)
    };

    if !accepted.is_empty() || !rejected.is_empty() {
        let ack = GossipEnvelope::TransactionAck {
            accepted: accepted.clone(),
            rejected: rejected.clone(),
        };
        write_envelope(writer, &ack).await?;
        P2pMetricsCounters::inc(&network.inner.metrics.transaction_ack_envelopes_sent);
        P2pMetricsCounters::add(
            &network.inner.metrics.transactions_accepted_sent,
            accepted.len() as u64,
        );
        P2pMetricsCounters::add(
            &network.inner.metrics.transactions_rejected_sent,
            rejected.len() as u64,
        );
    }

    for rejection in &rejected {
        let mut peers = network.inner.peers.lock().await;
        match known_peer.as_deref() {
            Some(peer) if transaction_rejection_counts_as_misbehavior(&rejection.reason) => {
                peers.record_misbehavior(peer, rejection.reason.clone());
            }
            Some(peer) => {
                peers.record_inbound_transaction_rejection(peer, rejection.reason.clone());
            }
            None if transaction_rejection_counts_as_misbehavior(&rejection.reason) => {
                peers
                    .record_inbound_misbehavior(&remote_addr.to_string(), rejection.reason.clone());
            }
            None => {
                peers.record_inbound_transaction_rejection(
                    &remote_addr.to_string(),
                    rejection.reason.clone(),
                );
            }
        }
    }
    network.forward_outbox().await;
    if rejected.is_empty() {
        record_inbound_result(network, known_peer, remote_addr, Ok(())).await;
    }
    Ok(())
}

fn receive_transactions_for_ack(
    node: &mut NodeCore,
    transactions: Vec<Transaction>,
) -> (Vec<String>, Vec<TransactionRejection>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for tx in transactions {
        let signature = tx.signature().to_string();
        match node.receive_transaction(tx) {
            Ok(TransactionSubmitOutcome::Added | TransactionSubmitOutcome::AlreadyKnown) => {
                accepted.push(signature);
            }
            Ok(TransactionSubmitOutcome::ConflictsWithPending) => {
                rejected.push(TransactionRejection {
                    signature,
                    reason: "transaction conflicts with pending mempool inputs".to_string(),
                });
            }
            Err(error) => rejected.push(TransactionRejection {
                signature,
                reason: format!("{error:#}"),
            }),
        }
    }
    (accepted, rejected)
}

async fn maybe_request_catchup(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    peer_status: &PeerStatus,
) -> Result<()> {
    let (local_height, local_tip_hash) = {
        let node = network.inner.node.lock().await;
        let status = node.ledger().status();
        (status.height, status.tip_hash)
    };
    if peer_status.request_snapshot {
        write_envelope(writer, &GossipEnvelope::ChainSnapshotRequest).await?;
    } else if peer_status.height > local_height {
        write_envelope(
            writer,
            &GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: MAX_BLOCK_BATCH,
            },
        )
        .await?;
    } else if peer_status.height == local_height && peer_status.tip_hash != local_tip_hash {
        write_envelope(writer, &GossipEnvelope::ChainSnapshotRequest).await?;
    }
    Ok(())
}

async fn maybe_request_mempool_catchup(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    peer_status: &PeerStatus,
) -> Result<()> {
    P2pMetricsCounters::inc(&network.inner.metrics.mempool_statuses_received);
    P2pMetricsCounters::add(
        &network.inner.metrics.mempool_status_transactions_received,
        peer_status.mempool_txs.len() as u64,
    );

    let (local_root, local_inventory, requests) = {
        let node = network.inner.node.lock().await;
        (
            node.mempool_root(),
            node.mempool_inventory(MEMPOOL_STATUS_LIMIT),
            node.missing_inventory_requests(&peer_status.mempool_txs, &[]),
        )
    };
    let local_txs = local_inventory.into_iter().collect::<BTreeSet<_>>();
    let shared = peer_status
        .mempool_txs
        .iter()
        .filter(|signature| local_txs.contains(*signature))
        .count();
    let missing = peer_status.mempool_txs.len().saturating_sub(shared);

    if peer_status.mempool_root != local_root {
        P2pMetricsCounters::inc(&network.inner.metrics.mempool_status_mismatches);
    }
    record_peer_mempool_status(
        network,
        known_peer,
        remote_addr,
        peer_status,
        shared,
        missing,
    )
    .await;

    let requested_signatures = requests
        .iter()
        .map(|envelope| match envelope {
            GossipEnvelope::TransactionRequest { signatures } => signatures.len(),
            _ => 0,
        })
        .sum::<usize>();
    if requested_signatures > 0 {
        write_payload(writer, &requests).await?;
        P2pMetricsCounters::inc(&network.inner.metrics.mempool_transaction_requests_sent);
        P2pMetricsCounters::add(
            &network
                .inner
                .metrics
                .mempool_transaction_request_signatures_sent,
            requested_signatures as u64,
        );
        if let Some(peer) = known_peer {
            network
                .inner
                .peers
                .lock()
                .await
                .record_sent(peer, requests.len() as u64);
        }
    }
    Ok(())
}

async fn push_catchup_to_peer(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    peer_status: &PeerStatus,
) -> Result<Option<PeerStatus>> {
    let payload = catchup_payload_for_peer(&network.inner.node, peer_status).await;
    if payload.is_empty() {
        return Ok(None);
    }

    let updated_status = payload.iter().find_map(|envelope| match envelope {
        GossipEnvelope::Blocks { blocks } => blocks
            .last()
            .map(|block| PeerStatus::new(block.height, block.hash.clone())),
        GossipEnvelope::ChainSnapshot(snapshot) => snapshot
            .blocks
            .last()
            .map(|block| PeerStatus::new(block.height, block.hash.clone())),
        _ => None,
    });
    write_payload(writer, &payload).await?;
    Ok(updated_status)
}

async fn catchup_payload_for_peer(
    node: &SharedNode,
    peer_status: &PeerStatus,
) -> Vec<GossipEnvelope> {
    let node = node.lock().await;
    let local_status = node.ledger().status();
    if node.ledger().is_setup_placeholder() {
        return Vec::new();
    }
    if peer_status.push_snapshot {
        return vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
    }
    if peer_status.height < local_status.height {
        let blocks = node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH);
        if blocks.is_empty() {
            Vec::new()
        } else {
            vec![GossipEnvelope::Blocks { blocks }]
        }
    } else if peer_status.height == local_status.height
        && peer_status.tip_hash != local_status.tip_hash
    {
        vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())]
    } else {
        Vec::new()
    }
}

async fn apply_peer_list(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    peers: Vec<String>,
) -> Result<()> {
    let self_filter_addr = network.self_filter_addr().await;
    let mut peerbook = network.inner.peers.lock().await;
    for address in peers {
        let peer = match normalize_advertised_peer(&address, remote_addr) {
            Ok(peer) => peer,
            Err(error) => {
                if debug_logging_enabled() {
                    eprintln!("p2p peer-list address {address} ignored: {error:#}");
                }
                continue;
            }
        };
        if is_self_peer_address_for(&peer, network.inner.listen_addr, self_filter_addr) {
            P2pMetricsCounters::inc(&network.inner.metrics.self_peer_skips);
        } else if peer_list_address_is_discoverable(&peer, remote_addr)? {
            peerbook.add_peer(peer);
        } else {
            P2pMetricsCounters::inc(&network.inner.metrics.self_peer_skips);
        }
    }
    Ok(())
}

async fn write_payload(writer: &mut OwnedWriteHalf, payload: &[GossipEnvelope]) -> Result<()> {
    for envelope in payload {
        write_envelope(writer, envelope).await?;
    }
    Ok(())
}

fn transactions_in_envelopes(envelopes: &[GossipEnvelope]) -> Vec<(String, Transaction)> {
    let mut transactions = Vec::new();
    for envelope in envelopes {
        match envelope {
            GossipEnvelope::Transaction(tx) => {
                transactions.push((tx.signature().to_string(), tx.clone()));
            }
            GossipEnvelope::Transactions { transactions: txs } => {
                transactions.extend(
                    txs.iter()
                        .map(|tx| (tx.signature().to_string(), tx.clone())),
                );
            }
            _ => {}
        }
    }
    transactions
}

fn short_signature(signature: &str) -> String {
    signature.chars().take(12).collect()
}

fn transaction_rejection_counts_as_misbehavior(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    if transaction_rejection_is_state_dependent(&reason) {
        return false;
    }
    transaction_rejection_is_structurally_invalid(&reason)
}

fn transaction_rejection_is_state_dependent(reason: &str) -> bool {
    [
        "mempool is full",
        "anchor is not on this chain",
        "anchor is too old",
        "conflict",
        "missing output",
        "not spendable",
        "insufficient funds",
        "does not cover",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
}

fn transaction_rejection_is_structurally_invalid(reason: &str) -> bool {
    [
        "signature",
        "proof header is invalid",
        "proof hash is invalid",
        "proof does not meet difficulty",
        "reward is invalid",
        "required burn amount",
        "difficulty is invalid",
        "inputs do not balance",
        "duplicate input",
        "input owner does not match",
        "has no inputs",
        "inputs must have one owner",
        "overflow",
        "invalid public key",
        "invalid transaction public key",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
}

async fn write_envelope(writer: &mut OwnedWriteHalf, envelope: &GossipEnvelope) -> Result<()> {
    let line = serde_json::to_string(envelope)?;
    if line.len() > MAX_GOSSIP_LINE_BYTES {
        anyhow::bail!(
            "p2p message is {} bytes, exceeding {} byte limit",
            line.len(),
            MAX_GOSSIP_LINE_BYTES
        );
    }
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

struct LimitedLineReader<R> {
    reader: BufReader<R>,
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LimitedLineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            pending: Vec::new(),
        }
    }

    async fn read_line(&mut self) -> Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("peer closed before completing a gossip message");
            }

            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if self.pending.len() + newline > MAX_GOSSIP_LINE_BYTES {
                    anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
                }
                self.pending.extend_from_slice(&available[..newline]);
                self.reader.consume(newline + 1);
                if self.pending.ends_with(b"\r") {
                    self.pending.pop();
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes)
                    .context("p2p message is not valid UTF-8")
                    .map(Some);
            }

            if self.pending.len() + available.len() > MAX_GOSSIP_LINE_BYTES {
                anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
            }
            let consumed = available.len();
            self.pending.extend_from_slice(available);
            self.reader.consume(consumed);
        }
    }
}

async fn read_session_envelope(
    network: &GossipNetwork,
    connection_label: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
) -> Result<Option<GossipEnvelope>> {
    let Some(line) = reader.read_line().await? else {
        return Ok(None);
    };
    P2pMetricsCounters::add(&network.inner.metrics.bytes_received, line.len() as u64 + 1);
    if line.trim().is_empty() {
        P2pMetricsCounters::inc(&network.inner.metrics.empty_frames);
        P2pMetricsCounters::set_last(
            &network.inner.metrics.last_empty_frame_remote,
            connection_label.to_string(),
        );
        anyhow::bail!("empty p2p envelope");
    }

    match parse_envelope(&line) {
        Ok(envelope) => {
            P2pMetricsCounters::inc(&network.inner.metrics.envelopes_received);
            record_received_envelope_kind(&network.inner.metrics, &envelope);
            Ok(Some(envelope))
        }
        Err(error) => {
            P2pMetricsCounters::inc(&network.inner.metrics.parse_errors);
            P2pMetricsCounters::set_last(
                &network.inner.metrics.last_parse_error,
                format!("{connection_label}: {error:#}"),
            );
            Err(error)
        }
    }
}

fn record_received_envelope_kind(metrics: &P2pMetricsCounters, envelope: &GossipEnvelope) {
    match envelope {
        GossipEnvelope::Hello(_) => {
            P2pMetricsCounters::inc(&metrics.hello_envelopes_received);
        }
        GossipEnvelope::PeerStatus { .. } => {
            P2pMetricsCounters::inc(&metrics.peer_status_envelopes_received);
        }
        GossipEnvelope::Inventory { .. } => {
            P2pMetricsCounters::inc(&metrics.inventory_envelopes_received);
        }
        GossipEnvelope::Transaction(_)
        | GossipEnvelope::Transactions { .. }
        | GossipEnvelope::Block(_)
        | GossipEnvelope::Blocks { .. }
        | GossipEnvelope::ChainSnapshot(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
        }
        GossipEnvelope::ChainSnapshotRequest
        | GossipEnvelope::BlockRangeRequest { .. }
        | GossipEnvelope::TransactionRequest { .. }
        | GossipEnvelope::BlockRequest { .. }
        | GossipEnvelope::TransactionAck { .. }
        | GossipEnvelope::PeerAnnouncement { .. }
        | GossipEnvelope::PeerVerificationChallenge { .. }
        | GossipEnvelope::PeerVerificationResponse { .. }
        | GossipEnvelope::PeerList { .. } => {
            P2pMetricsCounters::inc(&metrics.control_envelopes_received);
        }
    }
}

fn parse_envelope(line: &str) -> Result<GossipEnvelope> {
    if line.trim().is_empty() {
        anyhow::bail!("empty p2p envelope");
    }
    let envelope = serde_json::from_str(line).context("invalid p2p envelope JSON")?;
    validate_envelope_limits(&envelope)?;
    Ok(envelope)
}

fn validate_envelope_limits(envelope: &GossipEnvelope) -> Result<()> {
    match envelope {
        GossipEnvelope::BlockRangeRequest { limit, .. } => {
            ensure_len("block range request", *limit, MAX_BLOCK_BATCH)?;
        }
        GossipEnvelope::TransactionRequest { signatures } => {
            ensure_len("transaction request", signatures.len(), MAX_OBJECT_REQUESTS)?;
        }
        GossipEnvelope::BlockRequest { hashes } => {
            ensure_len("block request", hashes.len(), MAX_OBJECT_REQUESTS)?;
        }
        GossipEnvelope::Inventory { txs, blocks } => {
            ensure_len("transaction inventory", txs.len(), MAX_INVENTORY_ITEMS)?;
            ensure_len("block inventory", blocks.len(), MAX_INVENTORY_ITEMS)?;
        }
        GossipEnvelope::TransactionAck { accepted, rejected } => {
            ensure_len(
                "transaction ack accepted",
                accepted.len(),
                MAX_OBJECT_REQUESTS,
            )?;
            ensure_len(
                "transaction ack rejected",
                rejected.len(),
                MAX_OBJECT_REQUESTS,
            )?;
        }
        GossipEnvelope::Transactions { transactions } => {
            ensure_len(
                "transaction batch",
                transactions.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
        }
        GossipEnvelope::Blocks { blocks } => {
            ensure_len("block batch", blocks.len(), MAX_BLOCK_BATCH)?;
        }
        GossipEnvelope::ChainSnapshot(snapshot) => {
            ensure_len("chain snapshot", snapshot.blocks.len(), MAX_SNAPSHOT_BLOCKS)?;
        }
        GossipEnvelope::PeerList { peers } => {
            ensure_len("peer list", peers.len(), MAX_PEER_LIST)?;
        }
        GossipEnvelope::PeerStatus { mempool_txs, .. } => {
            ensure_len("mempool status", mempool_txs.len(), MEMPOOL_STATUS_LIMIT)?;
        }
        GossipEnvelope::Hello(_)
        | GossipEnvelope::ChainSnapshotRequest
        | GossipEnvelope::Transaction(_)
        | GossipEnvelope::Block(_)
        | GossipEnvelope::PeerAnnouncement { .. }
        | GossipEnvelope::PeerVerificationChallenge { .. }
        | GossipEnvelope::PeerVerificationResponse { .. } => {}
    }
    Ok(())
}

fn transaction_batch_envelopes(transactions: Vec<Transaction>) -> Vec<GossipEnvelope> {
    transactions
        .chunks(TRANSACTION_BATCH_LIMIT)
        .map(|chunk| GossipEnvelope::Transactions {
            transactions: chunk.to_vec(),
        })
        .collect()
}

fn ensure_len(label: &str, len: usize, max: usize) -> Result<()> {
    if len > max {
        anyhow::bail!("{label} has {len} items, exceeding limit {max}");
    }
    Ok(())
}

async fn envelopes_for_peer(
    node: Option<&SharedNode>,
    peer_status: Option<PeerStatus>,
    envelopes: &[GossipEnvelope],
) -> Vec<GossipEnvelope> {
    let Some(node) = node else {
        return envelopes.to_vec();
    };
    let Some(peer_status) = peer_status else {
        return envelopes.to_vec();
    };

    let node = node.lock().await;
    let local_status = node.ledger().status();
    if node.ledger().is_setup_placeholder() {
        return envelopes
            .iter()
            .filter(|envelope| !matches!(envelope, GossipEnvelope::Block(_)))
            .cloned()
            .collect();
    }
    if peer_status.height < local_status.height {
        let mut payload = vec![GossipEnvelope::Blocks {
            blocks: node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH),
        }];
        payload.extend(
            envelopes
                .iter()
                .filter(|envelope| !matches!(envelope, GossipEnvelope::Block(_)))
                .cloned(),
        );
        return payload;
    }

    if peer_status.height == local_status.height && peer_status.tip_hash != local_status.tip_hash {
        return vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
    }

    if peer_needs_snapshot(peer_status.height, envelopes) {
        return vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
    }

    envelopes
        .iter()
        .filter(|envelope| match envelope {
            GossipEnvelope::Block(block) => block.height > peer_status.height,
            GossipEnvelope::Inventory { blocks, .. } => {
                blocks.iter().any(|block| block.height > peer_status.height)
            }
            _ => true,
        })
        .map(|envelope| match envelope {
            GossipEnvelope::Inventory { txs, blocks } => GossipEnvelope::Inventory {
                txs: txs.clone(),
                blocks: blocks
                    .iter()
                    .filter(|block| block.height > peer_status.height)
                    .cloned()
                    .collect(),
            },
            other => other.clone(),
        })
        .filter(|envelope| match envelope {
            GossipEnvelope::Inventory { txs, blocks } => !txs.is_empty() || !blocks.is_empty(),
            _ => true,
        })
        .collect()
}

pub async fn fetch_snapshot(peer: &str) -> Result<ChainSnapshot> {
    fetch_snapshot_with_announcement(peer, None).await
}

pub async fn fetch_peer_height(peer: &str) -> Result<u64> {
    fetch_peer_status(peer).await.map(|status| status.height)
}

async fn fetch_peer_status(peer: &str) -> Result<PeerStatus> {
    let stream = TcpStream::connect(peer)
        .await
        .with_context(|| format!("connecting to peer {peer}"))?;
    let (reader, _writer) = stream.into_split();
    let mut reader = LimitedLineReader::new(reader);
    let line = reader
        .read_line()
        .await?
        .with_context(|| format!("peer {peer} closed before sending its peer status"))?;
    match parse_envelope(&line)? {
        GossipEnvelope::Hello(hello) => {
            if hello.protocol_version != PROTOCOL_VERSION {
                anyhow::bail!(
                    "unsupported protocol version {}; expected {}",
                    hello.protocol_version,
                    PROTOCOL_VERSION
                );
            }
            if hello.network_id != NETWORK_ID {
                anyhow::bail!(
                    "wrong network {}; expected {}",
                    hello.network_id,
                    NETWORK_ID
                );
            }
            Ok(PeerStatus::with_time(
                hello.height,
                hello.tip_hash,
                hello.time_ms,
            ))
        }
        GossipEnvelope::PeerStatus {
            height,
            tip_hash,
            time_ms,
            mempool_count,
            mempool_root,
            mempool_txs,
        } => Ok(PeerStatus::from_envelope(
            height,
            tip_hash,
            mempool_count,
            mempool_root,
            mempool_txs,
            time_ms,
        )),
        other => anyhow::bail!("peer {peer} sent {other:?} instead of peer status"),
    }
}

pub async fn fetch_snapshot_with_announcement(
    peer: &str,
    _advertised_addr: Option<SocketAddr>,
) -> Result<ChainSnapshot> {
    let stream = TcpStream::connect(peer)
        .await
        .with_context(|| format!("connecting to join peer {peer}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = LimitedLineReader::new(reader);
    let line = reader
        .read_line()
        .await?
        .with_context(|| format!("join peer {peer} closed before sending its peer status"))?;
    match parse_envelope(&line)? {
        GossipEnvelope::Hello(hello) => {
            if hello.protocol_version != PROTOCOL_VERSION {
                anyhow::bail!(
                    "unsupported protocol version {}; expected {}",
                    hello.protocol_version,
                    PROTOCOL_VERSION
                );
            }
            if hello.network_id != NETWORK_ID {
                anyhow::bail!(
                    "wrong network {}; expected {}",
                    hello.network_id,
                    NETWORK_ID
                );
            }
        }
        GossipEnvelope::PeerStatus { .. } => {}
        other => anyhow::bail!("join peer {peer} sent {other:?} instead of peer status"),
    }

    let line = serde_json::to_string(&GossipEnvelope::ChainSnapshotRequest)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    let snapshot = read_join_snapshot_response(peer, &mut reader).await?;

    Ok(snapshot)
}

async fn read_join_snapshot_response(
    peer: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
) -> Result<ChainSnapshot> {
    for _ in 0..MAX_JOIN_RESPONSE_ENVELOPES {
        let line = timeout(JOIN_RESPONSE_TIMEOUT, reader.read_line())
            .await
            .with_context(|| format!("join peer {peer} timed out waiting for a chain snapshot"))??
            .with_context(|| format!("join peer {peer} closed before sending a chain snapshot"))?;
        match join_snapshot_response(peer, parse_envelope(&line)?)? {
            Some(snapshot) => return Ok(snapshot),
            None => continue,
        }
    }

    anyhow::bail!("join peer {peer} sent too many non-snapshot envelopes while joining")
}

fn join_snapshot_response(peer: &str, envelope: GossipEnvelope) -> Result<Option<ChainSnapshot>> {
    match envelope {
        GossipEnvelope::ChainSnapshot(snapshot) => Ok(Some(snapshot)),
        GossipEnvelope::Hello(_)
        | GossipEnvelope::PeerStatus { .. }
        | GossipEnvelope::PeerList { .. }
        | GossipEnvelope::PeerVerificationChallenge { .. }
        | GossipEnvelope::PeerVerificationResponse { .. }
        | GossipEnvelope::Inventory { .. } => Ok(None),
        other => anyhow::bail!("join peer {peer} sent {other:?} instead of a chain snapshot"),
    }
}

async fn validate_snapshot_extension(
    mut ledger: Ledger,
    snapshot: ChainSnapshot,
    now_ms: u64,
) -> Result<Ledger> {
    if ledger.is_setup_placeholder() {
        return tokio::task::spawn_blocking(move || Ledger::from_snapshot_at(snapshot, now_ms))
            .await
            .context("chain snapshot adoption worker failed")?;
    }
    let missing_blocks = ledger.missing_snapshot_blocks(&snapshot)?;
    verify_blocks_vdf(missing_blocks).await?;

    tokio::task::spawn_blocking(move || {
        ledger.extend_from_preverified_snapshot_at(snapshot, now_ms)?;
        Ok(ledger)
    })
    .await
    .context("chain snapshot extension worker failed")?
}

async fn validate_blocks_extension(
    mut ledger: Ledger,
    blocks: Vec<Block>,
    now_ms: u64,
) -> Result<Ledger> {
    if blocks.is_empty() {
        return Ok(ledger);
    }
    verify_blocks_vdf(blocks.clone()).await?;

    tokio::task::spawn_blocking(move || {
        for block in blocks {
            ledger.apply_preverified_block_at(block, now_ms)?;
        }
        Ok(ledger)
    })
    .await
    .context("block batch extension worker failed")?
}

async fn network_adjusted_time_ms(network: &GossipNetwork) -> u64 {
    let local_time_ms = now_ms();
    network
        .inner
        .peers
        .lock()
        .await
        .adjusted_time_ms_at(local_time_ms)
}

async fn verify_block_vdf(block: Block) -> Result<Block> {
    let seed = block.vdf_seed();
    let rounds = block.vdf_rounds;
    let solution = block.vdf_output.clone();
    let valid = tokio::task::spawn_blocking(move || verify_vdf(&seed, rounds, &solution))
        .await
        .context("VDF verification worker failed")?;
    if !valid {
        anyhow::bail!("block VDF output is invalid");
    }

    Ok(block)
}

async fn verify_blocks_vdf(blocks: Vec<Block>) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    for block in blocks {
        tasks.spawn_blocking(move || {
            if !verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output) {
                anyhow::bail!("block {} VDF output is invalid", block.height);
            }
            Ok::<(), anyhow::Error>(())
        });
    }

    while let Some(result) = tasks.join_next().await {
        result.context("VDF verification worker failed")??;
    }

    Ok(())
}

async fn record_peer_status(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    peer_status: &PeerStatus,
) {
    let local_receive_time_ms = now_ms();
    if let Some(peer) = known_peer {
        let mut peers = network.inner.peers.lock().await;
        peers.record_status(peer, peer_status.height, peer_status.tip_hash.clone());
        peers.record_clock_observation(
            peer,
            PeerDirection::Outbound,
            peer_status.time_ms,
            local_receive_time_ms,
        );
    } else {
        let peer = remote_addr.to_string();
        let mut peers = network.inner.peers.lock().await;
        peers.record_clock_observation(
            &peer,
            PeerDirection::Inbound,
            peer_status.time_ms,
            local_receive_time_ms,
        );
        peers.record_received(&peer, 1);
    }
}

async fn record_peer_mempool_status(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    peer_status: &PeerStatus,
    shared: usize,
    missing: usize,
) {
    let mut peers = network.inner.peers.lock().await;
    if let Some(peer) = known_peer {
        peers.record_mempool_status(
            peer,
            peer_status.mempool_count,
            peer_status.mempool_root.clone(),
            shared,
            missing,
        );
    } else {
        peers.record_inbound_mempool_status(
            &remote_addr.to_string(),
            peer_status.mempool_count,
            peer_status.mempool_root.clone(),
            shared,
            missing,
        );
    }
}

async fn process_hello(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    process_hello_inner(network, None, remote_addr, known_peer, hello).await
}

async fn process_hello_with_verification(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
    connection_label: &str,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    let mut verification_session = PeerVerificationSession {
        writer,
        reader,
        connection_label,
    };
    process_hello_inner(
        network,
        Some(&mut verification_session),
        remote_addr,
        known_peer,
        hello,
    )
    .await
}

async fn process_hello_inner(
    network: &GossipNetwork,
    mut verification_session: Option<&mut PeerVerificationSession<'_>>,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    hello: ProtocolHello,
) -> Result<PeerStatus> {
    if hello.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "unsupported protocol version {}; expected {}",
            hello.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if hello.network_id != NETWORK_ID {
        anyhow::bail!(
            "wrong network {}; expected {}",
            hello.network_id,
            NETWORK_ID
        );
    }
    if hello
        .node_id
        .as_deref()
        .is_some_and(|node_id| node_id == network.inner.node_id)
    {
        P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
        forget_stale_self_peer(network, known_peer).await;
        return Ok(PeerStatus::with_time(
            hello.height,
            hello.tip_hash,
            hello.time_ms,
        ));
    }
    let (local_genesis, local_accepts_remote_genesis) = {
        let node = network.inner.node.lock().await;
        (
            node.ledger().genesis_hash().to_string(),
            node.ledger().is_setup_placeholder(),
        )
    };
    let genesis_mismatch = hello.genesis_hash != local_genesis;
    let remote_is_setup_placeholder =
        hello.height == 0 && hello.genesis_hash == setup_placeholder_genesis_hash();
    let request_snapshot = genesis_mismatch && local_accepts_remote_genesis;
    let push_snapshot = genesis_mismatch && remote_is_setup_placeholder;
    if genesis_mismatch && !local_accepts_remote_genesis && !remote_is_setup_placeholder {
        anyhow::bail!(
            "wrong genesis {}; expected {local_genesis}",
            hello.genesis_hash
        );
    }

    let remote_node_id = hello.node_id.clone();
    if let Some(listen_addr) = &hello.listen_addr {
        let peer = normalize_advertised_peer(listen_addr, remote_addr)?;
        if network.is_self_peer(&peer).await {
            P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
            forget_stale_self_peer(network, known_peer).await;
        } else {
            let verified = match verification_session.as_mut() {
                Some(session) => {
                    remember_verified_advertised_peer(
                        network,
                        session,
                        remote_addr,
                        known_peer,
                        peer.clone(),
                        remote_node_id.as_deref(),
                    )
                    .await?
                }
                None => false,
            };
            if !verified && debug_logging_enabled() {
                eprintln!(
                    "p2p advertised address {peer} ignored because ownership was not verified"
                );
            }
        }
    }
    record_peer_status(
        network,
        known_peer,
        remote_addr,
        &PeerStatus::with_time(hello.height, hello.tip_hash.clone(), hello.time_ms),
    )
    .await;
    if request_snapshot {
        Ok(PeerStatus::with_snapshot_request(
            hello.height,
            hello.tip_hash,
            hello.time_ms,
        ))
    } else if push_snapshot {
        Ok(PeerStatus::with_snapshot_push(
            hello.height,
            hello.tip_hash,
            hello.time_ms,
        ))
    } else {
        Ok(PeerStatus::with_time(
            hello.height,
            hello.tip_hash,
            hello.time_ms,
        ))
    }
}

fn setup_placeholder_genesis_hash() -> String {
    Ledger::new(BTreeMap::new(), 1).genesis_hash().to_string()
}

fn new_node_id() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).expect("secure randomness unavailable for p2p node id");
    let signing_key = SigningKey::from_bytes(&bytes);
    let node_id = hex_encode(&signing_key.verifying_key().to_bytes());
    node_signing_keys()
        .lock()
        .expect("node signing key registry mutex poisoned")
        .insert(node_id.clone(), signing_key);
    node_id
}

fn node_signing_keys() -> &'static StdMutex<BTreeMap<String, SigningKey>> {
    NODE_SIGNING_KEYS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        anyhow::bail!("hex value has {} chars, expected {}", value.len(), N * 2);
    }
    let mut bytes = [0_u8; N];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => anyhow::bail!("invalid hex digit"),
    }
}

fn new_verification_nonce() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .expect("secure randomness unavailable for p2p verification nonce");
    hex_encode(&bytes)
}

fn peer_verification_payload(address: &str, nonce: &str, node_id: &str) -> String {
    format!("iuna-peer-verification:v1:{NETWORK_ID}:{node_id}:{address}:{nonce}")
}

fn peer_verification_response(
    network: &GossipNetwork,
    address: &str,
    nonce: &str,
) -> Option<GossipEnvelope> {
    peer_verification_response_for_node_id(&network.inner.node_id, address, nonce)
}

fn peer_verification_response_for_node_id(
    node_id: &str,
    address: &str,
    nonce: &str,
) -> Option<GossipEnvelope> {
    let keys = node_signing_keys()
        .lock()
        .expect("node signing key registry mutex poisoned");
    let signing_key = keys.get(node_id)?;
    let payload = peer_verification_payload(address, nonce, node_id);
    let signature: Signature = signing_key.sign(payload.as_bytes());
    Some(GossipEnvelope::PeerVerificationResponse {
        address: address.to_string(),
        nonce: nonce.to_string(),
        node_id: node_id.to_string(),
        signature: hex_encode(&signature.to_bytes()),
    })
}

fn peer_verification_response_is_valid(
    response_address: &str,
    response_nonce: &str,
    response_node_id: &str,
    signature: &str,
    expected_address: &str,
    expected_nonce: &str,
    expected_node_id: &str,
) -> bool {
    if response_address != expected_address
        || response_nonce != expected_nonce
        || response_node_id != expected_node_id
    {
        return false;
    }
    let public_key = match decode_hex_array::<32>(response_node_id) {
        Ok(public_key) => public_key,
        Err(_) => return false,
    };
    let signature = match decode_hex_array::<64>(signature) {
        Ok(signature) => Signature::from_bytes(&signature),
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&public_key) {
        Ok(verifying_key) => verifying_key,
        Err(_) => return false,
    };
    verifying_key
        .verify(
            peer_verification_payload(expected_address, expected_nonce, expected_node_id)
                .as_bytes(),
            &signature,
        )
        .is_ok()
}

async fn remember_verified_advertised_peer(
    network: &GossipNetwork,
    session: &mut PeerVerificationSession<'_>,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    peer: String,
    expected_node_id: Option<&str>,
) -> Result<bool> {
    if !advertised_peer_is_discoverable(&peer, remote_addr)? {
        return Ok(false);
    }
    if known_peer.as_deref() != Some(peer.as_str()) {
        let Some(expected_node_id) = expected_node_id else {
            return Ok(false);
        };
        if !verify_connected_peer_node_id(network, session, &peer, expected_node_id).await? {
            return Ok(false);
        }
        if !verify_advertised_peer_node_id(network, &peer, expected_node_id).await {
            return Ok(false);
        }
    }
    remember_discoverable_advertised_peer(network, remote_addr, known_peer, peer).await
}

async fn verify_connected_peer_node_id(
    network: &GossipNetwork,
    session: &mut PeerVerificationSession<'_>,
    peer: &str,
    expected_node_id: &str,
) -> Result<bool> {
    let nonce = new_verification_nonce();
    write_envelope(
        session.writer,
        &GossipEnvelope::PeerVerificationChallenge {
            address: peer.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await?;

    for _ in 0..MAX_PEER_VERIFICATION_ENVELOPES {
        let envelope = match timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(network, session.connection_label, session.reader),
        )
        .await
        {
            Ok(Ok(Some(envelope))) => envelope,
            Ok(Ok(None)) | Err(_) => return Ok(false),
            Ok(Err(error)) => return Err(error),
        };
        match envelope {
            GossipEnvelope::PeerVerificationResponse {
                address,
                nonce: response_nonce,
                node_id,
                signature,
            } => {
                return Ok(peer_verification_response_is_valid(
                    &address,
                    &response_nonce,
                    &node_id,
                    &signature,
                    peer,
                    &nonce,
                    expected_node_id,
                ));
            }
            GossipEnvelope::PeerVerificationChallenge { address, nonce } => {
                if let Some(response) = peer_verification_response(network, &address, &nonce) {
                    write_envelope(session.writer, &response).await?;
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

async fn verify_advertised_peer_node_id(
    network: &GossipNetwork,
    peer: &str,
    expected_node_id: &str,
) -> bool {
    let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(peer)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            if debug_logging_enabled() {
                eprintln!("p2p announced address {peer} failed verification: {error}");
            }
            return false;
        }
        Err(_) => {
            if debug_logging_enabled() {
                eprintln!("p2p announced address {peer} failed verification: timeout");
            }
            return false;
        }
    };
    let (reader, mut writer) = stream.into_split();
    let mut reader = LimitedLineReader::new(reader);
    let line = match timeout(HANDSHAKE_TIMEOUT, reader.read_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return false,
        Ok(Err(error)) => {
            if debug_logging_enabled() {
                eprintln!(
                    "p2p announced address {peer} sent invalid verification hello: {error:#}"
                );
            }
            return false;
        }
        Err(_) => return false,
    };
    let hello = match parse_envelope(&line) {
        Ok(GossipEnvelope::Hello(hello)) => hello,
        Ok(_) | Err(_) => return false,
    };

    if !advertised_peer_hello_is_compatible(network, &hello).await
        || hello.node_id.as_deref() != Some(expected_node_id)
    {
        return false;
    }

    let nonce = new_verification_nonce();
    if write_envelope(
        &mut writer,
        &GossipEnvelope::PeerVerificationChallenge {
            address: peer.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return false;
    }
    for _ in 0..MAX_PEER_VERIFICATION_ENVELOPES {
        let line = match timeout(HANDSHAKE_TIMEOUT, reader.read_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => return false,
        };
        let envelope = match parse_envelope(&line) {
            Ok(envelope) => envelope,
            Err(_) => return false,
        };
        if let GossipEnvelope::PeerVerificationResponse {
            address,
            nonce: response_nonce,
            node_id,
            signature,
        } = envelope
        {
            return peer_verification_response_is_valid(
                &address,
                &response_nonce,
                &node_id,
                &signature,
                peer,
                &nonce,
                expected_node_id,
            );
        }
    }
    false
}

async fn advertised_peer_hello_is_compatible(
    network: &GossipNetwork,
    hello: &ProtocolHello,
) -> bool {
    if hello.protocol_version != PROTOCOL_VERSION || hello.network_id != NETWORK_ID {
        return false;
    }
    let (local_genesis, local_accepts_remote_genesis) = {
        let node = network.inner.node.lock().await;
        (
            node.ledger().genesis_hash().to_string(),
            node.ledger().is_setup_placeholder(),
        )
    };
    let remote_is_setup_placeholder =
        hello.height == 0 && hello.genesis_hash == setup_placeholder_genesis_hash();
    hello.genesis_hash == local_genesis
        || local_accepts_remote_genesis
        || remote_is_setup_placeholder
}

async fn remember_discoverable_advertised_peer(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    peer: String,
) -> Result<bool> {
    if !advertised_peer_is_discoverable(&peer, remote_addr)? {
        return Ok(false);
    }
    if let Some(previous_peer) = known_peer.as_deref() {
        network
            .inner
            .peers
            .lock()
            .await
            .replace_peer_address(previous_peer, peer.clone());
    } else {
        network
            .inner
            .peers
            .lock()
            .await
            .observe_inbound_peer(peer.clone());
    }
    *known_peer = Some(peer);
    Ok(true)
}

async fn forget_stale_self_peer(network: &GossipNetwork, known_peer: &mut Option<String>) {
    if let Some(previous_peer) = known_peer.take() {
        network.inner.peers.lock().await.remove_peer(&previous_peer);
    }
}

async fn record_inbound_result(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
    result: Result<()>,
) {
    let peer = known_peer
        .clone()
        .unwrap_or_else(|| remote_addr.to_string());
    match result {
        Ok(()) => {
            if known_peer.is_some() {
                network.inner.peers.lock().await.record_received(&peer, 1);
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            if known_peer.is_some() {
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_misbehavior(&peer, message.clone());
            }
            if debug_logging_enabled() {
                eprintln!("p2p envelope from {peer} ignored: {message}");
            }
        }
    }
}

fn next_reconnect_delay(current: Duration) -> Duration {
    (current * 2).min(MAX_RECONNECT_DELAY)
}

fn peer_needs_snapshot(peer_height: u64, envelopes: &[GossipEnvelope]) -> bool {
    envelopes
        .iter()
        .filter_map(|envelope| match envelope {
            GossipEnvelope::Block(block) => Some(block.height),
            GossipEnvelope::Inventory { blocks, .. } => {
                blocks.iter().map(|block| block.height).min()
            }
            _ => None,
        })
        .min()
        .is_some_and(|first_block_height| peer_height + 1 < first_block_height)
}

fn reachable_advertised_addr(advertised_addr: SocketAddr, remote_addr: SocketAddr) -> SocketAddr {
    let mut reachable_addr = advertised_addr;
    if reachable_addr.ip().is_unspecified() {
        reachable_addr.set_ip(remote_addr.ip());
    }
    reachable_addr
}

fn normalize_advertised_peer(address: &str, remote_addr: SocketAddr) -> Result<String> {
    let advertised_addr = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid announced peer address {address}"))?;
    Ok(reachable_advertised_addr(advertised_addr, remote_addr).to_string())
}

fn peer_list_address_is_discoverable(address: &str, remote_addr: SocketAddr) -> Result<bool> {
    let candidate = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid peer-list address {address}"))?;
    Ok(socket_addr_is_discoverable(candidate, remote_addr))
}

fn advertised_peer_is_discoverable(address: &str, remote_addr: SocketAddr) -> Result<bool> {
    let candidate = address
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid announced peer address {address}"))?;
    Ok(socket_addr_is_discoverable(candidate, remote_addr))
}

fn socket_addr_is_discoverable(candidate: SocketAddr, remote_addr: SocketAddr) -> bool {
    if candidate.ip().is_loopback() {
        return remote_addr.ip().is_loopback();
    }
    ip_is_publicly_discoverable(candidate.ip())
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
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || [a, b, c, d] == [255, 255, 255, 255])
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xff00) == 0xff00)
        }
    }
}

fn is_self_peer_address_for(
    address: &str,
    listen_addr: SocketAddr,
    advertised_addr: Option<SocketAddr>,
) -> bool {
    address.parse::<SocketAddr>().is_ok_and(|candidate| {
        is_self_socket_addr(candidate, listen_addr)
            || advertised_addr.is_some_and(|addr| is_self_socket_addr(candidate, addr))
    })
}

fn is_self_socket_addr(candidate: SocketAddr, listen_addr: SocketAddr) -> bool {
    if candidate == listen_addr {
        return true;
    }
    if candidate.port() != listen_addr.port() {
        return false;
    }

    let candidate_ip = candidate.ip();
    let listen_ip = listen_addr.ip();
    if listen_ip.is_unspecified() {
        return candidate_ip.is_unspecified() || candidate_ip.is_loopback();
    }
    if candidate_ip.is_unspecified() {
        return listen_ip.is_loopback();
    }
    false
}

fn is_quiet_disconnect(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionAborted
            )
        })
    })
}

fn is_possible_fork_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("does not extend local tip")
        || message.contains("conflicts with local chain")
        || message.contains("expected block height")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::SocketAddr,
        sync::{Arc, Mutex as StdMutex},
    };

    use crate::{
        app::{
            BlockInventory, GossipEnvelope, NETWORK_ID, NodeCore, PROTOCOL_VERSION, PeerBook,
            PeerDirection, ProtocolHello, TRANSACTION_BATCH_LIMIT,
        },
        domain::{Amount, GenesisBurn, Ledger, Wallet},
    };
    use tokio::io::AsyncWriteExt;

    use super::{
        INBOUND_ACCEPT_RATE_WINDOW_MS, InboundConnectionLimiter, InboundSessionRejection,
        MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW, MAX_INBOUND_SESSIONS, MAX_INBOUND_SESSIONS_PER_IP,
        MAX_INVENTORY_ITEMS, MAX_OBJECT_REQUESTS, next_reconnect_delay, parse_envelope,
        reachable_advertised_addr, validate_envelope_limits,
    };

    #[test]
    fn unspecified_announced_ip_uses_remote_ip_with_announced_port() {
        let advertised: SocketAddr = "0.0.0.0:9445".parse().unwrap();
        let remote: SocketAddr = "203.0.113.10:52144".parse().unwrap();

        assert_eq!(
            reachable_advertised_addr(advertised, remote).to_string(),
            "203.0.113.10:9445"
        );
    }

    #[test]
    fn explicit_announced_ip_is_kept() {
        let advertised: SocketAddr = "127.0.0.1:9445".parse().unwrap();
        let remote: SocketAddr = "127.0.0.1:52144".parse().unwrap();

        assert_eq!(
            reachable_advertised_addr(advertised, remote).to_string(),
            "127.0.0.1:9445"
        );
    }

    #[test]
    fn loopback_peer_on_unspecified_listen_port_is_self() {
        let listen_addr: SocketAddr = "0.0.0.0:9545".parse().unwrap();

        assert!(super::is_self_peer_address_for(
            "127.0.0.1:9545",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(super::is_self_peer_address_for(
            "0.0.0.0:9545",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(!super::is_self_peer_address_for(
            "127.0.0.1:9546",
            listen_addr,
            Some(listen_addr)
        ));
        assert!(!super::is_self_peer_address_for(
            "203.0.113.10:9545",
            listen_addr,
            Some(listen_addr)
        ));
    }

    #[test]
    fn oversized_object_requests_are_rejected_before_processing() {
        let envelope = GossipEnvelope::TransactionRequest {
            signatures: vec!["sig".to_string(); MAX_OBJECT_REQUESTS + 1],
        };

        let error = validate_envelope_limits(&envelope).unwrap_err();

        assert!(error.to_string().contains("transaction request"));
    }

    #[test]
    fn oversized_inventory_is_rejected_before_processing() {
        let envelope = GossipEnvelope::Inventory {
            txs: vec!["sig".to_string(); MAX_INVENTORY_ITEMS + 1],
            blocks: Vec::new(),
        };

        let error = validate_envelope_limits(&envelope).unwrap_err();

        assert!(error.to_string().contains("transaction inventory"));
    }

    #[test]
    fn oversized_transaction_ack_is_rejected_before_processing() {
        let envelope = GossipEnvelope::TransactionAck {
            accepted: vec!["sig".to_string(); MAX_OBJECT_REQUESTS + 1],
            rejected: Vec::new(),
        };

        let error = validate_envelope_limits(&envelope).unwrap_err();

        assert!(error.to_string().contains("transaction ack accepted"));
    }

    #[test]
    fn parser_applies_envelope_limits() {
        let line = serde_json::to_string(&GossipEnvelope::BlockRequest {
            hashes: vec!["hash".to_string(); MAX_OBJECT_REQUESTS + 1],
        })
        .unwrap();

        let error = parse_envelope(&line).unwrap_err();

        assert!(error.to_string().contains("block request"));
    }

    #[test]
    fn parser_rejects_empty_envelope_without_json_eof() {
        let error = parse_envelope("").unwrap_err();

        assert!(error.to_string().contains("empty p2p envelope"));
        assert!(!format!("{error:#}").contains("EOF while parsing"));
    }

    #[test]
    fn parser_accepts_legacy_peer_status_without_mempool_fields() {
        let envelope =
            parse_envelope(r#"{"type":"peer_status","height":7,"tip_hash":"tip"}"#).unwrap();

        assert_eq!(
            envelope,
            GossipEnvelope::PeerStatus {
                height: 7,
                tip_hash: "tip".to_string(),
                time_ms: 0,
                mempool_count: 0,
                mempool_root: String::new(),
                mempool_txs: Vec::new(),
            }
        );
    }

    #[test]
    fn oversized_mempool_status_is_rejected_before_processing() {
        let envelope = GossipEnvelope::PeerStatus {
            height: 7,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
            mempool_count: super::MEMPOOL_STATUS_LIMIT + 1,
            mempool_root: "root".to_string(),
            mempool_txs: vec!["sig".to_string(); super::MEMPOOL_STATUS_LIMIT + 1],
        };

        let error = validate_envelope_limits(&envelope).unwrap_err();

        assert!(error.to_string().contains("mempool status"));
    }

    #[test]
    fn mempool_status_can_advertise_full_pending_pool_beyond_inventory_batch_size() {
        let envelope = GossipEnvelope::PeerStatus {
            height: 7,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
            mempool_count: MAX_INVENTORY_ITEMS + 1,
            mempool_root: "root".to_string(),
            mempool_txs: vec!["sig".to_string(); MAX_INVENTORY_ITEMS + 1],
        };

        validate_envelope_limits(&envelope).unwrap();
    }

    #[test]
    fn received_envelope_metrics_are_categorized() {
        let metrics = super::P2pMetricsCounters::default();

        super::record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::PeerStatus {
                height: 7,
                tip_hash: "tip".to_string(),
                time_ms: 1_000,
                mempool_count: 0,
                mempool_root: String::new(),
                mempool_txs: Vec::new(),
            },
        );
        super::record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::Inventory {
                txs: Vec::new(),
                blocks: Vec::new(),
            },
        );
        super::record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::Blocks { blocks: Vec::new() },
        );
        super::record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::TransactionAck {
                accepted: Vec::new(),
                rejected: Vec::new(),
            },
        );
        super::record_received_envelope_kind(&metrics, &GossipEnvelope::ChainSnapshotRequest);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.peer_status_envelopes_received, 1);
        assert_eq!(snapshot.inventory_envelopes_received, 1);
        assert_eq!(snapshot.data_envelopes_received, 1);
        assert_eq!(snapshot.control_envelopes_received, 2);
    }

    #[tokio::test]
    async fn limited_line_reader_keeps_partial_line_after_cancelled_read() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut reader = super::LimitedLineReader::new(reader);
        let line = serde_json::to_string(&GossipEnvelope::PeerStatus {
            height: 7,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
            mempool_count: 0,
            mempool_root: String::new(),
            mempool_txs: Vec::new(),
        })
        .unwrap();
        let split_at = line.len() / 2;

        writer
            .write_all(&line.as_bytes()[..split_at])
            .await
            .unwrap();
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_millis(25), reader.read_line()).await;

        assert!(cancelled.is_err());

        writer
            .write_all(&line.as_bytes()[split_at..])
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();

        assert_eq!(
            reader.read_line().await.unwrap().as_deref(),
            Some(line.as_str())
        );
    }

    #[test]
    fn peer_needs_snapshot_when_block_gossip_skips_a_height() {
        let block = crate::domain::Block {
            height: 10,
            prev_hash: "prev".to_string(),
            timestamp_ms: 1,
            miner: "miner".to_string(),
            finalizer_rank: 0,
            reward: 100,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            transactions: Vec::new(),
            hash: "hash".to_string(),
        };

        assert!(super::peer_needs_snapshot(
            8,
            &[GossipEnvelope::Block(block.clone())]
        ));
        assert!(!super::peer_needs_snapshot(
            9,
            &[GossipEnvelope::Block(block)]
        ));
        assert!(!super::peer_needs_snapshot(
            8,
            &[GossipEnvelope::PeerAnnouncement {
                address: "127.0.0.1:9444".to_string(),
                node_id: Some("peer-node".to_string()),
            }]
        ));
    }

    #[test]
    fn inbound_limiter_enforces_per_ip_active_limit() {
        let ip = "203.0.113.10".parse().unwrap();
        let mut limiter = InboundConnectionLimiter::default();
        for _ in 0..MAX_INBOUND_SESSIONS_PER_IP {
            limiter.try_acquire(ip, 1_000).unwrap();
        }

        assert_eq!(
            limiter.try_acquire(ip, 1_000).unwrap_err(),
            InboundSessionRejection::PeerActive
        );

        limiter.release(ip);
        limiter.try_acquire(ip, 1_000).unwrap();
    }

    #[test]
    fn inbound_limiter_enforces_global_active_limit() {
        let mut limiter = InboundConnectionLimiter::default();
        for index in 0..MAX_INBOUND_SESSIONS {
            let ip = format!("198.51.100.{index}").parse().unwrap();
            limiter.try_acquire(ip, 1_000).unwrap();
        }

        assert_eq!(
            limiter
                .try_acquire("203.0.113.200".parse().unwrap(), 1_000)
                .unwrap_err(),
            InboundSessionRejection::GlobalActive
        );
    }

    #[test]
    fn inbound_limiter_enforces_per_ip_accept_rate() {
        let ip = "203.0.113.20".parse().unwrap();
        let mut limiter = InboundConnectionLimiter::default();
        for _ in 0..MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW {
            limiter.try_acquire(ip, 1_000).unwrap();
            limiter.release(ip);
        }

        assert_eq!(
            limiter.try_acquire(ip, 1_000).unwrap_err(),
            InboundSessionRejection::PeerRate
        );
        limiter
            .try_acquire(ip, 1_000 + INBOUND_ACCEPT_RATE_WINDOW_MS)
            .unwrap();
    }

    #[test]
    fn reconnect_backoff_is_capped() {
        assert_eq!(
            next_reconnect_delay(super::INITIAL_RECONNECT_DELAY),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_delay(super::MAX_RECONNECT_DELAY),
            super::MAX_RECONNECT_DELAY
        );
    }

    #[tokio::test]
    async fn peer_payload_repairs_lagging_peer_without_networking() {
        let alice = Wallet::from_seed("p2p-alice");
        let bob = Wallet::from_seed("p2p-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let block = {
            let mut node = node.lock().await;
            node.burn(1).unwrap();
            node.drain_outbox();
            let block = node.mine_one_at(1).unwrap();
            node.drain_outbox();
            block
        };

        let payload = super::envelopes_for_peer(
            Some(&node),
            Some(super::PeerStatus::new(0, "genesis".to_string())),
            &[GossipEnvelope::Block(block)],
        )
        .await;

        assert!(matches!(payload[0], GossipEnvelope::Blocks { .. }));
        match &payload[0] {
            GossipEnvelope::Blocks { blocks } => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].height, 1);
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn tx_and_block_gossip_sends_full_transaction_and_inventory() {
        let alice = Wallet::from_seed("inventory-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(Some("127.0.0.1:9544".parse().unwrap())),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        let (tx_signature, block_hash) = {
            let mut node = node.lock().await;
            let tx = node.burn(1).unwrap();
            node.drain_outbox();
            let block = node.mine_one_at(1).unwrap();
            (tx.signature().to_string(), block.hash)
        };
        let (tx, block) = {
            let node = node.lock().await;
            (
                node.transactions_by_signature(std::slice::from_ref(&tx_signature))
                    .remove(0),
                node.blocks_by_hash(std::slice::from_ref(&block_hash))
                    .remove(0),
            )
        };

        let prepared = network
            .prepare_gossip(vec![
                GossipEnvelope::Transaction(tx),
                GossipEnvelope::Block(block),
            ])
            .await;

        assert_eq!(prepared.len(), 2);
        match &prepared[0] {
            GossipEnvelope::Transactions { transactions } => {
                assert_eq!(transactions.len(), 1);
                assert_eq!(transactions[0].signature(), tx_signature);
            }
            other => panic!("expected transaction batch, got {other:?}"),
        }
        match &prepared[1] {
            GossipEnvelope::Inventory { txs, blocks } => {
                assert_eq!(txs, &[tx_signature]);
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].hash, block_hash);
            }
            other => panic!("expected inventory, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transaction_batch_gossip_keeps_full_transactions_for_mempool_repair() {
        let alice = Wallet::from_seed("mempool-repair-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        let (tx, signature) = {
            let mut node = node.lock().await;
            let tx = node.burn(1).unwrap();
            (tx.clone(), tx.signature().to_string())
        };

        let prepared = network
            .prepare_gossip(vec![GossipEnvelope::Transactions {
                transactions: vec![tx],
            }])
            .await;

        assert_eq!(prepared.len(), 2);
        assert!(matches!(
            &prepared[0],
            GossipEnvelope::Transactions { transactions } if transactions.len() == 1
        ));
        match &prepared[1] {
            GossipEnvelope::Inventory { txs, blocks } => {
                assert_eq!(txs, &[signature]);
                assert!(blocks.is_empty());
            }
            other => panic!("expected inventory, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn prepare_gossip_splits_transaction_batches_at_receiver_limit() {
        let alice = Wallet::from_seed("mempool-repair-batch-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        let transactions = {
            let mut node = node.lock().await;
            (0..(TRANSACTION_BATCH_LIMIT + 1))
                .map(|_| node.burn(1).unwrap())
                .collect::<Vec<_>>()
        };

        let prepared = network
            .prepare_gossip(vec![GossipEnvelope::Transactions { transactions }])
            .await;

        let batch_sizes = prepared
            .iter()
            .filter_map(|envelope| match envelope {
                GossipEnvelope::Transactions { transactions } => Some(transactions.len()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_sizes, vec![TRANSACTION_BATCH_LIMIT, 1]);
        assert!(prepared.iter().all(|envelope| match envelope {
            GossipEnvelope::Transactions { transactions } =>
                transactions.len() <= TRANSACTION_BATCH_LIMIT,
            _ => true,
        }));
        assert!(matches!(
            prepared.last(),
            Some(GossipEnvelope::Inventory { txs, blocks })
                if txs.len() == TRANSACTION_BATCH_LIMIT + 1 && blocks.is_empty()
        ));
    }

    #[tokio::test]
    async fn retry_transaction_batches_are_split_at_receiver_limit() {
        let alice = Wallet::from_seed("tx-ack-retry-batch-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let peer = "127.0.0.1:9545";
        {
            let mut node = node.lock().await;
            for _ in 0..(TRANSACTION_BATCH_LIMIT + 1) {
                node.burn(1).unwrap();
            }
        }

        let retry = network.pending_transactions_for_retry(peer).await;
        assert_eq!(retry.len(), TRANSACTION_BATCH_LIMIT + 1);

        let retry_batches = super::transaction_batch_envelopes(retry);
        let batch_sizes = retry_batches
            .iter()
            .map(|envelope| match envelope {
                GossipEnvelope::Transactions { transactions } => {
                    assert!(transactions.len() <= TRANSACTION_BATCH_LIMIT);
                    transactions.len()
                }
                other => panic!("expected transaction batch, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_sizes, vec![TRANSACTION_BATCH_LIMIT, 1]);
    }

    #[tokio::test]
    async fn unacked_pending_transactions_retry_until_peer_accepts() {
        let alice = Wallet::from_seed("tx-ack-retry-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let peer = "127.0.0.1:9545";
        let signature = {
            let mut node = node.lock().await;
            node.burn(1).unwrap().signature().to_string()
        };

        let first_retry = network.pending_transactions_for_retry(peer).await;
        assert_eq!(first_retry.len(), 1);
        assert_eq!(first_retry[0].signature(), signature);
        assert_eq!(network.metrics().transaction_ack_pending, 1);
        let immediate_retry = network.pending_transactions_for_retry(peer).await;
        assert!(immediate_retry.is_empty());

        network
            .record_transaction_ack(peer, std::slice::from_ref(&signature), &[])
            .await;
        let after_ack_retry = network.pending_transactions_for_retry(peer).await;

        assert!(after_ack_retry.is_empty());
        let metrics = network.metrics();
        assert_eq!(metrics.transaction_ack_pending, 0);
        assert_eq!(metrics.transaction_ack_envelopes_received, 1);
        assert_eq!(metrics.transactions_accepted_received, 1);
    }

    #[test]
    fn conflicting_input_transaction_is_rejected_not_acked_as_accepted() {
        let alice = Wallet::from_seed("tx-ack-conflict-alice");
        let bob = Wallet::from_seed("tx-ack-conflict-bob");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let ledger = Ledger::new(allocations, 25);
        let first = ledger
            .build_transfer(&alice, bob.address(), 100, 0)
            .unwrap();
        let conflicting = ledger.build_burn(&alice, 100, 0).unwrap();
        let mut receiver = NodeCore::from_ledger(bob, ledger, 0);

        let (accepted, rejected) = super::receive_transactions_for_ack(
            &mut receiver,
            vec![first.clone(), conflicting.clone()],
        );

        assert_eq!(accepted, vec![first.signature().to_string()]);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].signature, conflicting.signature());
        assert!(
            rejected[0].reason.contains("conflicts with pending"),
            "{}",
            rejected[0].reason
        );
        assert_eq!(receiver.ledger().pending().len(), 1);
        assert_eq!(
            receiver.ledger().pending()[0].signature(),
            first.signature()
        );
    }

    #[test]
    fn transaction_rejection_classifier_only_scores_structural_invalidity() {
        for reason in [
            "transaction signature is invalid",
            "mine transaction proof hash is invalid",
            "mine transaction proof does not meet difficulty",
            "mine required burn amount must be between",
            "mine transaction difficulty is invalid",
            "transaction inputs do not balance outputs, burn, and fee",
            "duplicate input in transaction",
            "transaction input owner does not match spent output",
            "transaction has no inputs",
            "transaction inputs must have one owner",
        ] {
            assert!(
                super::transaction_rejection_counts_as_misbehavior(reason),
                "{reason} should count as misbehavior"
            );
        }

        for reason in [
            "mempool is full",
            "mine transaction anchor is not on this chain",
            "mine transaction anchor is too old",
            "transaction conflicts with pending mempool inputs",
            "transaction spends missing output abc:0",
            "selected UTXOs do not cover transfer amount plus fee",
            "insufficient funds for address",
        ] {
            assert!(
                !super::transaction_rejection_counts_as_misbehavior(reason),
                "{reason} should be treated as state-dependent"
            );
        }
    }

    #[tokio::test]
    async fn inventory_requests_only_missing_objects() {
        let alice = Wallet::from_seed("missing-inv-alice");
        let bob = Wallet::from_seed("missing-inv-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let mut local = node("local", alice.clone(), allocations.clone());
        let mut remote = node("remote", alice, allocations);
        let tx = local.burn(1).unwrap();
        let block = local.mine_one_at(1).unwrap();

        let requests = remote.missing_inventory_requests(
            &[tx.signature().to_string()],
            &[BlockInventory {
                height: block.height,
                hash: block.hash.clone(),
            }],
        );
        assert!(matches!(
            requests[0],
            GossipEnvelope::TransactionRequest { .. }
        ));
        assert!(matches!(requests[1], GossipEnvelope::BlockRequest { .. }));

        remote.receive(GossipEnvelope::Transaction(tx)).unwrap();
        let requests = remote.missing_inventory_requests(
            &[],
            &[BlockInventory {
                height: block.height,
                hash: block.hash,
            }],
        );
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], GossipEnvelope::BlockRequest { .. }));
    }

    #[tokio::test]
    async fn inventory_gap_requests_range_instead_of_orphan_block() {
        let alice = Wallet::from_seed("gap-inv-alice");
        let bob = Wallet::from_seed("gap-inv-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let mut local = node("local", alice, allocations.clone());
        let remote = node("remote", bob, allocations);

        let mut latest = None;
        for height in 1..=3 {
            local.burn(1).unwrap();
            latest = Some(local.mine_one_at(height).unwrap());
        }
        let latest = latest.unwrap();

        let requests = remote.missing_inventory_requests(
            &[],
            &[BlockInventory {
                height: latest.height,
                hash: latest.hash,
            }],
        );

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            GossipEnvelope::BlockRangeRequest { from_height, limit } => {
                assert_eq!(*from_height, 1);
                assert_eq!(*limit, crate::app::BLOCK_REQUEST_LIMIT);
            }
            other => panic!("expected block range request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_catchup_payload_pushes_missing_blocks_to_lagging_peer() {
        let alice = Wallet::from_seed("catchup-alice");
        let bob = Wallet::from_seed("catchup-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        {
            let mut node = node.lock().await;
            for height in 1..=3 {
                node.burn(1).unwrap();
                node.drain_outbox();
                node.mine_one_at(height).unwrap();
                node.drain_outbox();
            }
        }

        let payload = super::catchup_payload_for_peer(
            &node,
            &super::PeerStatus::new(1, "old-tip".to_string()),
        )
        .await;

        assert_eq!(payload.len(), 1);
        match &payload[0] {
            GossipEnvelope::Blocks { blocks } => {
                assert_eq!(
                    blocks.iter().map(|block| block.height).collect::<Vec<_>>(),
                    vec![2, 3]
                );
            }
            other => panic!("expected missing block payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hello_rejects_wrong_network_or_genesis_without_banning() {
        let alice = Wallet::from_seed("hello-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        let wrong_network = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: "other-network".to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: None,
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        assert!(
            super::process_hello(
                &network,
                "127.0.0.1:9545".parse().unwrap(),
                &mut None,
                wrong_network,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("wrong network")
        );

        let wrong_genesis = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: "not-local-genesis".to_string(),
            listen_addr: Some("127.0.0.1:9545".to_string()),
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        assert!(
            super::process_hello(
                &network,
                "127.0.0.1:9545".parse().unwrap(),
                &mut None,
                wrong_genesis,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("wrong genesis")
        );

        let wrong_protocol = ProtocolHello {
            protocol_version: PROTOCOL_VERSION + 1,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: Some("127.0.0.1:9545".to_string()),
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        assert!(
            super::process_hello(
                &network,
                "127.0.0.1:9545".parse().unwrap(),
                &mut None,
                wrong_protocol,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported protocol version")
        );

        assert!(network.inner.peers.lock().await.list().is_empty());
    }

    #[tokio::test]
    async fn hello_records_remote_clock_observation() {
        let alice = Wallet::from_seed("hello-clock-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let remote_time_ms = crate::app::now_ms().saturating_add(60_000);
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: Some("127.0.0.1:9545".to_string()),
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: remote_time_ms,
        };

        let mut known_peer = None;
        super::process_hello(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        let peers = network.inner.peers.lock().await.list();
        let peer = peers
            .iter()
            .find(|peer| peer.address == "127.0.0.1:9545")
            .unwrap();
        assert!(peer.last_clock_offset_ms.unwrap() > 30_000);
        assert_eq!(peer.last_clock_offset_accepted, Some(true));
    }

    #[tokio::test]
    async fn hello_remembers_advertised_address_after_signed_session_and_dialback() {
        let alice = Wallet::from_seed("hello-dialback-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::clone(&peers),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let remote_node_id = super::new_node_id();
        let remote_addr = spawn_hello_server(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: None,
            node_id: Some(remote_node_id.clone()),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        })
        .await;
        let original_addr = spawn_verification_responder(remote_node_id.clone()).await;
        let stream = tokio::net::TcpStream::connect(original_addr).await.unwrap();
        let remote_socket = stream.peer_addr().unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = super::LimitedLineReader::new(reader);
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: Some(remote_addr.to_string()),
            node_id: Some(remote_node_id),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        let mut known_peer = None;

        super::process_hello_with_verification(
            &network,
            &mut writer,
            &mut reader,
            "test-original-peer",
            remote_socket,
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert_eq!(known_peer, Some(remote_addr.to_string()));
        let listed = peers.lock().await.list();
        let peer = listed
            .iter()
            .find(|peer| peer.address == remote_addr.to_string())
            .unwrap();
        assert_eq!(peer.direction, PeerDirection::Inbound);
    }

    #[tokio::test]
    async fn hello_ignores_advertised_address_when_connected_peer_cannot_sign_claimed_node_id() {
        let alice = Wallet::from_seed("hello-dialback-spoof-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::clone(&peers),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let victim_node_id = super::new_node_id();
        let attacker_node_id = super::new_node_id();
        let remote_addr = spawn_hello_server(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: None,
            node_id: Some(victim_node_id.clone()),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        })
        .await;
        let original_addr = spawn_verification_responder(attacker_node_id).await;
        let stream = tokio::net::TcpStream::connect(original_addr).await.unwrap();
        let remote_socket = stream.peer_addr().unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = super::LimitedLineReader::new(reader);
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: Some(remote_addr.to_string()),
            node_id: Some(victim_node_id),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        let mut known_peer = None;

        super::process_hello_with_verification(
            &network,
            &mut writer,
            &mut reader,
            "test-attacker-peer",
            remote_socket,
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert!(known_peer.is_none());
        assert!(
            !peers
                .lock()
                .await
                .addresses()
                .contains(&remote_addr.to_string())
        );
    }

    #[tokio::test]
    async fn dialback_rejects_address_that_signs_with_different_node_id() {
        let alice = Wallet::from_seed("hello-dialback-mismatch-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let honest_node_id = super::new_node_id();
        let claimed_node_id = super::new_node_id();
        let remote_addr = spawn_hello_server(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: None,
            node_id: Some(honest_node_id),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        })
        .await;

        assert!(
            !super::verify_advertised_peer_node_id(
                &network,
                &remote_addr.to_string(),
                &claimed_node_id
            )
            .await
        );
    }

    #[tokio::test]
    async fn inbound_announced_address_replaces_gateway_address_for_ui() {
        let alice = Wallet::from_seed("hello-public-announced-inbound-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let mut known_peer = None;

        let remembered = super::remember_discoverable_advertised_peer(
            &network,
            "10.42.0.1:51234".parse().unwrap(),
            &mut known_peer,
            "142.132.164.59:9444".to_string(),
        )
        .await
        .unwrap();
        super::record_peer_status(
            &network,
            &known_peer,
            "10.42.0.1:51234".parse().unwrap(),
            &super::PeerStatus::with_time(7, "tip".to_string(), 1_000),
        )
        .await;

        assert!(remembered);
        assert_eq!(known_peer.as_deref(), Some("142.132.164.59:9444"));
        let listed = peers.lock().await.list();
        assert_eq!(listed.len(), 1);
        let peer = &listed[0];
        assert_eq!(peer.address, "142.132.164.59:9444");
        assert_eq!(peer.direction, PeerDirection::Inbound);
        assert_eq!(peer.last_known_height, Some(7));
        assert_eq!(peer.messages_received, 0);

        let repeated = super::remember_discoverable_advertised_peer(
            &network,
            "10.42.0.1:51234".parse().unwrap(),
            &mut known_peer,
            "142.132.164.59:9444".to_string(),
        )
        .await
        .unwrap();

        assert!(repeated);
        assert_eq!(
            peers.lock().await.list()[0].direction,
            PeerDirection::Inbound
        );
    }

    #[tokio::test]
    async fn inbound_verification_only_session_closes_after_response() {
        let alice = Wallet::from_seed("verification-only-close-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen_addr = listener.local_addr().unwrap();
        drop(listener);
        let network = super::GossipNetwork::start(node, peers, listen_addr, None, true)
            .await
            .unwrap();

        let stream = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = super::LimitedLineReader::new(reader);
        let hello_line = reader.read_line().await.unwrap().unwrap();
        let node_id = match super::parse_envelope(&hello_line).unwrap() {
            GossipEnvelope::Hello(hello) => hello.node_id.unwrap(),
            other => panic!("expected hello, got {other:?}"),
        };
        let nonce = super::new_verification_nonce();
        super::write_envelope(
            &mut writer,
            &GossipEnvelope::PeerVerificationChallenge {
                address: listen_addr.to_string(),
                nonce: nonce.clone(),
            },
        )
        .await
        .unwrap();

        let response_line =
            tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        match super::parse_envelope(&response_line).unwrap() {
            GossipEnvelope::PeerVerificationResponse {
                address,
                nonce: response_nonce,
                node_id: response_node_id,
                signature,
            } => assert!(super::peer_verification_response_is_valid(
                &address,
                &response_nonce,
                &response_node_id,
                &signature,
                &listen_addr.to_string(),
                &nonce,
                &node_id,
            )),
            other => panic!("expected verification response, got {other:?}"),
        }

        let closed = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line())
            .await
            .unwrap()
            .unwrap();
        assert!(closed.is_none());
        network.set_accept_inbound(false).await.unwrap();
    }

    #[tokio::test]
    async fn setup_placeholder_accepts_remote_genesis_and_adopts_snapshot() {
        let local_wallet = Wallet::from_seed("setup-placeholder-local");
        let local_ledger = Ledger::new(BTreeMap::new(), 1);
        let local_node = Arc::new(tokio::sync::Mutex::new(NodeCore::from_ledger(
            local_wallet,
            local_ledger.clone(),
            0,
        )));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: local_node,
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
                    "iuna.jhx.app:9444".to_string(),
                ]))),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        let remote_wallet = Wallet::from_seed("setup-placeholder-remote");
        let remote_snapshot = node(
            "remote",
            remote_wallet.clone(),
            allocations(std::slice::from_ref(&remote_wallet), 1_000),
        )
        .chain_snapshot();
        let remote_genesis = remote_snapshot.blocks[0].hash.clone();
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: remote_genesis.clone(),
            listen_addr: Some("142.132.164.59:9444".to_string()),
            node_id: None,
            height: 5,
            tip_hash: "remote-tip".to_string(),
            time_ms: 1_000,
        };
        let mut known_peer = Some("iuna.jhx.app:9444".to_string());
        let peer_status = super::process_hello(
            &network,
            "142.132.164.59:51234".parse().unwrap(),
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert!(peer_status.request_snapshot);
        assert!(!peer_status.push_snapshot);
        assert_eq!(known_peer.as_deref(), Some("iuna.jhx.app:9444"));
        let listed = network.inner.peers.lock().await.list();
        assert_eq!(listed.len(), 1);
        let peer = listed
            .into_iter()
            .find(|peer| peer.address == "iuna.jhx.app:9444")
            .unwrap();
        assert_eq!(peer.misbehavior_score, 0);
        assert!(!peer.is_banned_at(crate::app::now_ms()));

        let adopted =
            super::validate_snapshot_extension(local_ledger, remote_snapshot, crate::app::now_ms())
                .await
                .unwrap();
        assert_eq!(adopted.genesis_hash(), remote_genesis);
        assert!(
            network
                .inner
                .node
                .lock()
                .await
                .import_verified_ledger(adopted)
                .unwrap()
        );
        assert_eq!(
            network.inner.node.lock().await.ledger().genesis_hash(),
            remote_genesis
        );
    }

    #[tokio::test]
    async fn real_node_accepts_setup_placeholder_peer_and_pushes_snapshot() {
        let wallet = Wallet::from_seed("setup-placeholder-peer-real-node");
        let node = Arc::new(tokio::sync::Mutex::new(node(
            "real",
            wallet.clone(),
            allocations(std::slice::from_ref(&wallet), 1_000),
        )));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let setup_ledger = Ledger::new(BTreeMap::new(), 1);
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: setup_ledger.genesis_hash().to_string(),
            listen_addr: Some("127.0.0.1:9545".to_string()),
            node_id: None,
            height: 0,
            tip_hash: setup_ledger.status().tip_hash,
            time_ms: 1_000,
        };

        let peer_status = super::process_hello(
            &network,
            "127.0.0.1:51234".parse().unwrap(),
            &mut None,
            hello,
        )
        .await
        .unwrap();

        assert!(!peer_status.request_snapshot);
        assert!(peer_status.push_snapshot);
        let payload = super::catchup_payload_for_peer(&node, &peer_status).await;
        assert!(matches!(
            payload.as_slice(),
            [GossipEnvelope::ChainSnapshot(_)]
        ));
    }

    #[tokio::test]
    async fn hello_ignores_private_advertised_listen_address() {
        let alice = Wallet::from_seed("hello-private-listen-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let status = node.lock().await.ledger().status();
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: node.lock().await.ledger().genesis_hash().to_string(),
            listen_addr: Some("10.42.1.1:12138".to_string()),
            node_id: None,
            height: status.height,
            tip_hash: status.tip_hash,
            time_ms: 1_000,
        };

        let mut known_peer = None;
        super::process_hello(
            &network,
            "142.132.164.59:51234".parse().unwrap(),
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert!(known_peer.is_none());
        assert!(peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn inbound_status_does_not_create_outbound_ephemeral_peer() {
        let alice = Wallet::from_seed("inbound-status-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        super::record_peer_status(
            &network,
            &None,
            "127.0.0.1:51729".parse().unwrap(),
            &super::PeerStatus::new(4, "tip".to_string()),
        )
        .await;

        let peers = peers.lock().await;
        assert!(peers.addresses().is_empty());
        let listed = peers.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].direction, PeerDirection::Inbound);
    }

    #[test]
    fn join_snapshot_response_ignores_status_noise_before_snapshot() {
        assert!(
            super::join_snapshot_response(
                "127.0.0.1:9544",
                GossipEnvelope::PeerStatus {
                    height: 0,
                    tip_hash: "tip".to_string(),
                    time_ms: 1_000,
                    mempool_count: 0,
                    mempool_root: String::new(),
                    mempool_txs: Vec::new(),
                }
            )
            .unwrap()
            .is_none()
        );

        let alice = Wallet::from_seed("join-noise-alice");
        let snapshot = node(
            "alice",
            alice.clone(),
            allocations(std::slice::from_ref(&alice), 1_000),
        )
        .chain_snapshot();
        let parsed = super::join_snapshot_response(
            "127.0.0.1:9544",
            GossipEnvelope::ChainSnapshot(snapshot.clone()),
        )
        .unwrap();

        assert_eq!(parsed, Some(snapshot));
    }

    #[tokio::test]
    async fn peer_exchange_does_not_advertise_self_when_outbound_only() {
        let alice = Wallet::from_seed("px-private-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "127.0.0.1:9545".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers,
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"127.0.0.1:9544".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_exchange_omits_hostname_bootstrap_peers() {
        let alice = Wallet::from_seed("px-hostname-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "iuna.jhx.app:9444".to_string(),
            "127.0.0.1:9545".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers,
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"iuna.jhx.app:9444".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_exchange_advertises_stable_listen_and_known_peers() {
        let alice = Wallet::from_seed("px-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "127.0.0.1:9545".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers,
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        network.set_accept_inbound(true).await.unwrap();

        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(peers.contains(&"127.0.0.1:9544".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
        network.set_accept_inbound(false).await.unwrap();
    }

    #[tokio::test]
    async fn peer_exchange_filters_announced_self_from_known_peers() {
        let alice = Wallet::from_seed("px-announced-self-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "8.8.8.8:9444".to_string(),
            "8.8.4.4:9444".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers,
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(Some("8.8.8.8:9444".parse().unwrap())),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        network.set_accept_inbound(true).await.unwrap();

        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert_eq!(
                    peers.iter().filter(|peer| *peer == "8.8.8.8:9444").count(),
                    1
                );
                assert!(peers.contains(&"8.8.4.4:9444".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
        network.set_accept_inbound(false).await.unwrap();
    }

    #[tokio::test]
    async fn peer_list_adds_stable_outbound_peers() {
        let alice = Wallet::from_seed("px-recv-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        super::apply_peer_list(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            vec!["127.0.0.1:9544".to_string(), "127.0.0.1:9546".to_string()],
        )
        .await
        .unwrap();

        let addresses = peers.lock().await.addresses();
        assert!(!addresses.contains(&"127.0.0.1:9544".to_string()));
        assert!(addresses.contains(&"127.0.0.1:9546".to_string()));
    }

    #[tokio::test]
    async fn peer_list_ignores_invalid_peer_addresses() {
        let alice = Wallet::from_seed("px-list-invalid-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        super::apply_peer_list(
            &network,
            "127.0.0.1:9545".parse().unwrap(),
            vec![
                "iuna.jhx.app:9444".to_string(),
                "127.0.0.1:9546".to_string(),
            ],
        )
        .await
        .unwrap();

        let addresses = peers.lock().await.addresses();
        assert!(!addresses.contains(&"iuna.jhx.app:9444".to_string()));
        assert!(addresses.contains(&"127.0.0.1:9546".to_string()));
    }

    #[tokio::test]
    async fn peer_list_ignores_announced_self_address() {
        let alice = Wallet::from_seed("px-list-announced-self-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(Some("8.8.8.8:9444".parse().unwrap())),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        super::apply_peer_list(
            &network,
            "8.8.4.4:9444".parse().unwrap(),
            vec!["8.8.8.8:9444".to_string(), "8.8.4.4:9445".to_string()],
        )
        .await
        .unwrap();

        let addresses = peers.lock().await.addresses();
        assert!(!addresses.contains(&"8.8.8.8:9444".to_string()));
        assert!(addresses.contains(&"8.8.4.4:9445".to_string()));
        network.set_accept_inbound(false).await.unwrap();
    }

    #[tokio::test]
    async fn peer_list_ignores_private_ephemeral_addresses() {
        let alice = Wallet::from_seed("px-private-ephemeral-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        super::apply_peer_list(
            &network,
            "142.132.164.59:9444".parse().unwrap(),
            vec![
                "10.42.1.1:10091".to_string(),
                "142.132.164.59:9444".to_string(),
            ],
        )
        .await
        .unwrap();

        let addresses = peers.lock().await.addresses();
        assert!(!addresses.contains(&"10.42.1.1:10091".to_string()));
        assert!(addresses.contains(&"142.132.164.59:9444".to_string()));
    }

    #[tokio::test]
    async fn peer_announcement_ignores_private_ephemeral_address() {
        let alice = Wallet::from_seed("px-private-announcement-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let mut known_peer = None;

        let remembered = super::remember_discoverable_advertised_peer(
            &network,
            "142.132.164.59:51234".parse().unwrap(),
            &mut known_peer,
            "10.42.1.1:10091".to_string(),
        )
        .await
        .unwrap();

        assert!(!remembered);
        assert!(known_peer.is_none());
        assert!(peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn peer_announcement_removes_outbound_peer_that_announces_self_address() {
        let alice = Wallet::from_seed("px-self-announcement-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "10.42.1.1:30508".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let mut known_peer = Some("10.42.1.1:30508".to_string());

        super::forget_stale_self_peer(&network, &mut known_peer).await;

        assert_eq!(known_peer, None);
        assert!(peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn peer_list_ignores_loopback_alias_for_unspecified_self() {
        let alice = Wallet::from_seed("px-self-alias-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9545".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };

        super::apply_peer_list(
            &network,
            "127.0.0.1:9544".parse().unwrap(),
            vec!["127.0.0.1:9545".to_string(), "127.0.0.1:9546".to_string()],
        )
        .await
        .unwrap();

        let addresses = peers.lock().await.addresses();
        assert!(!addresses.contains(&"127.0.0.1:9545".to_string()));
        assert!(addresses.contains(&"127.0.0.1:9546".to_string()));
        assert_eq!(network.metrics().self_peer_skips, 1);
    }

    #[tokio::test]
    async fn hello_ignores_loopback_alias_for_unspecified_self() {
        let alice = Wallet::from_seed("hello-self-alias-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9545".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: Some("127.0.0.1:9545".to_string()),
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };

        super::process_hello(
            &network,
            "127.0.0.1:52144".parse().unwrap(),
            &mut None,
            hello,
        )
        .await
        .unwrap();

        assert_eq!(network.metrics().self_peer_rejections, 1);
        assert!(peers.lock().await.addresses().is_empty());
        let listed = peers.lock().await.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].direction, PeerDirection::Inbound);
    }

    #[tokio::test]
    async fn hello_removes_outbound_peer_that_announces_self_address() {
        let alice = Wallet::from_seed("hello-self-outbound-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "10.42.1.1:16987".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: Some("127.0.0.1:9444".to_string()),
            node_id: None,
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        let mut known_peer = Some("10.42.1.1:16987".to_string());

        super::process_hello(
            &network,
            "10.42.1.1:16987".parse().unwrap(),
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert_eq!(network.metrics().self_peer_rejections, 1);
        assert!(known_peer.is_none());
        assert!(peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn hello_removes_outbound_peer_with_same_node_id() {
        let alice = Wallet::from_seed("hello-self-node-id-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "142.132.164.59:9444".to_string(),
        ])));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::clone(&peers),
                listen_addr: "0.0.0.0:9444".parse().unwrap(),
                p2p_announce_addr: tokio::sync::Mutex::new(None),
                node_id: super::new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                tx_delivery: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(
                    StdMutex::new(super::InboundConnectionLimiter::default()),
                ),
                metrics: super::P2pMetricsCounters::default(),
            }),
        };
        let hello = ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: network
                .inner
                .node
                .lock()
                .await
                .ledger()
                .genesis_hash()
                .to_string(),
            listen_addr: Some("0.0.0.0:9444".to_string()),
            node_id: Some(network.inner.node_id.clone()),
            height: 0,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        };
        let mut known_peer = Some("142.132.164.59:9444".to_string());

        super::process_hello(
            &network,
            "142.132.164.59:52144".parse().unwrap(),
            &mut known_peer,
            hello,
        )
        .await
        .unwrap();

        assert_eq!(network.metrics().self_peer_rejections, 1);
        assert!(known_peer.is_none());
        assert!(peers.lock().await.addresses().is_empty());
    }

    async fn spawn_hello_server(hello: ProtocolHello) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let node_id = hello.node_id.clone();
            let (reader, mut writer) = stream.into_split();
            let line = serde_json::to_string(&GossipEnvelope::Hello(hello)).unwrap();
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            let Some(node_id) = node_id else {
                return;
            };
            let mut reader = super::LimitedLineReader::new(reader);
            let Ok(Some(line)) = reader.read_line().await else {
                return;
            };
            let Ok(GossipEnvelope::PeerVerificationChallenge { address, nonce }) =
                super::parse_envelope(&line)
            else {
                return;
            };
            let Some(response) =
                super::peer_verification_response_for_node_id(&node_id, &address, &nonce)
            else {
                return;
            };
            let line = serde_json::to_string(&response).unwrap();
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        });
        addr
    }

    async fn spawn_verification_responder(node_id: String) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut reader = super::LimitedLineReader::new(reader);
            let Ok(Some(line)) = reader.read_line().await else {
                return;
            };
            let Ok(GossipEnvelope::PeerVerificationChallenge { address, nonce }) =
                super::parse_envelope(&line)
            else {
                return;
            };
            let Some(response) =
                super::peer_verification_response_for_node_id(&node_id, &address, &nonce)
            else {
                return;
            };
            let line = serde_json::to_string(&response).unwrap();
            let _ = writer.write_all(line.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
        });
        addr
    }

    fn node(_network_key: &str, wallet: Wallet, allocations: BTreeMap<String, Amount>) -> NodeCore {
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            25,
        )
        .unwrap();
        NodeCore::from_ledger(wallet, ledger, 0)
    }

    fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
        wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), amount))
            .collect()
    }
}
