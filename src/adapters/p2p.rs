use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinHandle,
};

use crate::app::{GossipEnvelope, SharedNode, SharedPeerBook};

mod fetch;
mod handshake;
mod identity;
mod inbound_limiter;
mod line_codec;
mod metrics;
mod network;
mod peer_addr;
mod peer_status;
mod process;
mod session;
mod sync;
#[cfg(test)]
mod test_support;
mod writer;
pub use fetch::{fetch_peer_height, fetch_snapshot, fetch_snapshot_with_announcement};
use fetch::{
    network_adjusted_time_ms, validate_blocks_extension, validate_chain_bootstrap, verify_block_vdf,
};
#[cfg(test)]
use handshake::verify_advertised_peer_node_id;
use handshake::{
    forget_stale_self_peer, process_hello, process_hello_with_verification, record_peer_status,
};
#[cfg(test)]
use identity::peer_verification_response_for_node_id;
use identity::{new_node_id, peer_verification_response};
#[cfg(test)]
use identity::{new_verification_nonce, peer_verification_response_is_valid};
use inbound_limiter::{InboundConnectionLimiter, InboundSessionPermit, InboundSessionRejection};
use line_codec::{LimitedLineReader, parse_envelope, read_session_envelope};
pub use metrics::P2pMetrics;
use metrics::P2pMetricsCounters;
use peer_addr::{
    inbound_error_counts_as_misbehavior, is_possible_fork_error, is_quiet_disconnect,
    is_self_peer_address_for, next_reconnect_delay as next_reconnect_delay_with_max,
    normalize_advertised_peer,
};
use peer_status::PeerStatus;
use process::{process_envelope, respond_to_peer_verification_challenge};
use session::{accept_loop, outbound_session, outbound_supervisor};
use sync::{apply_peer_list, envelopes_for_peer, maybe_request_catchup, write_peer_exchange};
use writer::{byte_bounded_block_page, write_envelope, write_payload};

const MAX_BLOCK_BATCH: usize = 128;
const MAX_OBJECT_REQUESTS: usize = 128;
const MAX_INVENTORY_ITEMS: usize = 512;
const MAX_BLOCK_LOCATOR_HASHES: usize = 64;
const MAX_PEER_LIST: usize = 128;
const MAX_GOSSIP_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INBOUND_SESSIONS: usize = 64;
const MAX_INBOUND_SESSIONS_PER_IP: usize = 8;
const MAX_INBOUND_ACCEPTS_PER_IP_PER_WINDOW: usize = 24;
const INBOUND_ACCEPT_RATE_WINDOW_MS: u64 = 10_000;
const PEER_QUEUE_SIZE: usize = 256;
const INBOUND_PEER_QUEUE_SIZE: usize = 16;
const MAX_OUTBOUND_BATCH_BYTES: usize = MAX_GOSSIP_LINE_BYTES + 1;
const PEER_QUEUE_BYTES: usize = 4 * MAX_OUTBOUND_BATCH_BYTES;
const INBOUND_PEER_QUEUE_BYTES: usize = 2 * MAX_OUTBOUND_BATCH_BYTES;
const MAX_CONCURRENT_CHAIN_VALIDATIONS: usize = 2;
const STALE_INBOUND_PEER_RETENTION_MS: u64 = 60 * 60 * 1_000;
const STALE_DISCOVERED_PEER_RETENTION_MS: u64 = 60 * 60 * 1_000;
const MAX_DISCOVERED_OUTBOUND_DIALS_PER_CYCLE: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const CATCHUP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_EXCHANGE_INTERVAL: Duration = Duration::from_secs(30);
const JOIN_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JOIN_RESPONSE_ENVELOPES: usize = 16;
const MAX_PEER_VERIFICATION_ENVELOPES: usize = 8;
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const INBOUND_SESSION_PREFIX: &str = "inbound://";
struct OutboundBatch {
    envelopes: Arc<[GossipEnvelope]>,
    _queued_bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct GossipSession {
    peer: String,
    sender: mpsc::Sender<OutboundBatch>,
    shutdown: watch::Sender<bool>,
    queue_bytes: Arc<Semaphore>,
}

struct ChainValidationCoordinator {
    permits: Arc<Semaphore>,
    active: StdMutex<BTreeMap<String, watch::Sender<bool>>>,
}

impl Default for ChainValidationCoordinator {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_CHAIN_VALIDATIONS)),
            active: StdMutex::new(BTreeMap::new()),
        }
    }
}

struct ChainValidationGuard {
    coordinator: Arc<ChainValidationCoordinator>,
    key: String,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ChainValidationGuard {
    fn drop(&mut self) {
        let sender = self
            .coordinator
            .active
            .lock()
            .expect("chain validation mutex poisoned")
            .remove(&self.key);
        if let Some(sender) = sender {
            let _ = sender.send(true);
        }
    }
}

#[cfg(feature = "fuzzing")]
pub fn fuzz_parse_envelope(line: &str) -> anyhow::Result<GossipEnvelope> {
    parse_envelope(line)
}

#[derive(Clone)]
pub struct GossipNetwork {
    inner: Arc<GossipNetworkInner>,
}

pub(super) struct SyncProgressGuard {
    network: GossipNetwork,
    id: u64,
    generation: u64,
}

impl SyncProgressGuard {
    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn is_current(&self) -> bool {
        self.network.sync_generation_is_current(self.generation)
    }
}

impl Drop for SyncProgressGuard {
    fn drop(&mut self) {
        self.network.finish_sync_progress(self.id);
    }
}

struct GossipNetworkInner {
    node: SharedNode,
    peers: SharedPeerBook,
    listen_addr: SocketAddr,
    p2p_announce_addr: Mutex<Option<SocketAddr>>,
    node_id: String,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    sessions: Mutex<BTreeMap<String, GossipSession>>,
    inbound_limiter: Arc<StdMutex<InboundConnectionLimiter>>,
    metrics: P2pMetricsCounters,
    sync_progress: StdMutex<SyncProgressState>,
    chain_validation: Arc<ChainValidationCoordinator>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncProgress {
    pub start_height: u64,
    pub validated_height: u64,
    pub target_height: u64,
}

#[derive(Default)]
struct SyncProgressState {
    next_id: u64,
    generation: u64,
    active: BTreeMap<u64, SyncProgress>,
    last_activity: Option<std::time::Instant>,
}

#[cfg(test)]
mod tests;
