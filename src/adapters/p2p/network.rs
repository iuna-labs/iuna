use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex as StdMutex},
};

use anyhow::{Context, Result};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
};

use crate::app::{
    BlockInventory, GossipEnvelope, SharedNode, SharedPeerBook, debug_logging_enabled, now_ms,
};

use super::{
    ChainValidationCoordinator, ChainValidationGuard, GossipNetwork, GossipNetworkInner,
    GossipSession, INBOUND_SESSION_PREFIX, InboundConnectionLimiter, InboundSessionPermit,
    InboundSessionRejection, MAX_DISCOVERED_OUTBOUND_DIALS_PER_CYCLE, MAX_GOSSIP_LINE_BYTES,
    MAX_OUTBOUND_BATCH_BYTES, OutboundBatch, P2pMetrics, P2pMetricsCounters, PEER_QUEUE_BYTES,
    PEER_QUEUE_SIZE, STALE_DISCOVERED_PEER_RETENTION_MS, STALE_INBOUND_PEER_RETENTION_MS,
    accept_loop, is_self_peer_address_for, new_node_id, outbound_session, outbound_supervisor,
};

impl GossipNetwork {
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
                p2p_announce_addr: tokio::sync::Mutex::new(p2p_announce_addr),
                node_id: new_node_id(),
                accept_task: tokio::sync::Mutex::new(None),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
                inbound_limiter: Arc::new(StdMutex::new(InboundConnectionLimiter::default())),
                metrics: P2pMetricsCounters::default(),
                sync_progress: StdMutex::new(super::SyncProgressState::default()),
                chain_validation: Arc::new(ChainValidationCoordinator::default()),
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

    pub fn listen_addr(&self) -> SocketAddr {
        self.inner.listen_addr
    }

    pub async fn set_p2p_announce_addr(&self, addr: Option<SocketAddr>) {
        *self.inner.p2p_announce_addr.lock().await = addr;
    }

    pub(super) async fn advertised_addr(&self) -> Option<SocketAddr> {
        if !self.accepts_inbound().await {
            return None;
        }
        Some((*self.inner.p2p_announce_addr.lock().await).unwrap_or(self.inner.listen_addr))
    }

    pub(super) async fn self_filter_addr(&self) -> Option<SocketAddr> {
        if let Some(addr) = *self.inner.p2p_announce_addr.lock().await {
            return Some(addr);
        }
        self.accepts_inbound()
            .await
            .then_some(self.inner.listen_addr)
    }

    pub(super) async fn is_self_peer(&self, address: &str) -> bool {
        is_self_peer_address_for(
            address,
            self.inner.listen_addr,
            self.self_filter_addr().await,
        )
    }

    pub fn metrics(&self) -> P2pMetrics {
        self.inner.metrics.snapshot()
    }

    pub fn sync_progress(&self) -> Option<super::SyncProgress> {
        self.inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned")
            .active
            .values()
            .copied()
            .max_by_key(|progress| (progress.target_height, progress.validated_height))
    }

    pub(super) fn begin_sync_progress(
        &self,
        start_height: u64,
        target_height: u64,
    ) -> super::SyncProgressGuard {
        let mut state = self
            .inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned");
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        state.active.insert(
            id,
            super::SyncProgress {
                start_height,
                validated_height: start_height,
                target_height: target_height.max(start_height),
            },
        );
        state.last_activity = Some(std::time::Instant::now());
        super::SyncProgressGuard {
            network: self.clone(),
            id,
            generation: state.generation,
        }
    }

    pub(crate) fn invalidate_sync_progress(&self) {
        let mut state = self
            .inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned");
        state.generation = state.generation.wrapping_add(1);
        state.active.clear();
    }

    pub(super) fn sync_generation(&self) -> u64 {
        self.inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned")
            .generation
    }

    pub(super) fn sync_generation_is_current(&self, generation: u64) -> bool {
        self.sync_generation() == generation
    }

    pub(super) fn update_sync_progress(&self, id: u64, validated_height: u64) {
        let mut state = self
            .inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned");
        if let Some(progress) = state.active.get_mut(&id) {
            progress.validated_height =
                validated_height.clamp(progress.start_height, progress.target_height);
            state.last_activity = Some(std::time::Instant::now());
        }
    }

    pub(super) fn finish_sync_progress(&self, id: u64) {
        let mut state = self
            .inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned");
        state.active.remove(&id);
        state.last_activity = Some(std::time::Instant::now());
    }

    pub fn chain_sync_active_or_recent(&self, quiet_period: std::time::Duration) -> bool {
        let state = self
            .inner
            .sync_progress
            .lock()
            .expect("sync progress mutex poisoned");
        if !state.active.is_empty() {
            return true;
        }
        state
            .last_activity
            .is_some_and(|last_activity| last_activity.elapsed() <= quiet_period)
    }

    pub(super) fn try_acquire_inbound_session(
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

    pub(super) async fn claim_chain_validation(&self, key: String) -> Option<ChainValidationGuard> {
        loop {
            let active = self
                .inner
                .chain_validation
                .active
                .lock()
                .expect("chain validation mutex poisoned")
                .get(&key)
                .map(watch::Sender::subscribe);
            if let Some(mut active) = active {
                while !*active.borrow() {
                    if active.changed().await.is_err() {
                        break;
                    }
                }
                continue;
            }

            let permit = Arc::clone(&self.inner.chain_validation.permits)
                .acquire_owned()
                .await
                .ok()?;
            let duplicate = {
                let mut active = self
                    .inner
                    .chain_validation
                    .active
                    .lock()
                    .expect("chain validation mutex poisoned");
                if let Some(sender) = active.get(&key) {
                    Some(sender.subscribe())
                } else {
                    let (sender, _) = watch::channel(false);
                    active.insert(key.clone(), sender);
                    None
                }
            };
            if let Some(mut receiver) = duplicate {
                drop(permit);
                while !*receiver.borrow() {
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
                continue;
            }
            return Some(ChainValidationGuard {
                coordinator: Arc::clone(&self.inner.chain_validation),
                key,
                _permit: permit,
            });
        }
    }

    pub async fn broadcast(&self, envelopes: Vec<GossipEnvelope>) -> Result<()> {
        let envelopes = self.prepare_gossip(envelopes).await;
        if envelopes.is_empty() {
            return Ok(());
        }

        let batches = byte_bounded_gossip_batches(envelopes)?;
        let sessions = self.inner.sessions.lock().await.clone();
        let mut disconnect = Vec::new();
        for (session_id, session) in sessions {
            if self.inner.peers.lock().await.is_banned(&session.peer) {
                continue;
            }
            for (envelopes, encoded_bytes) in &batches {
                let permit =
                    match Arc::clone(&session.queue_bytes).try_acquire_many_owned(*encoded_bytes) {
                        Ok(permit) => permit,
                        Err(_) => {
                            P2pMetricsCounters::inc(&self.inner.metrics.outbound_queue_full);
                            if session_id.starts_with(INBOUND_SESSION_PREFIX) {
                                let _ = session.shutdown.send(true);
                                disconnect.push((session_id.clone(), session.sender.clone()));
                            }
                            break;
                        }
                    };
                let batch = OutboundBatch {
                    envelopes: Arc::clone(envelopes),
                    _queued_bytes: permit,
                };
                match session.sender.try_send(batch) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        P2pMetricsCounters::inc(&self.inner.metrics.outbound_queue_full);
                        if session_id.starts_with(INBOUND_SESSION_PREFIX) {
                            let _ = session.shutdown.send(true);
                            disconnect.push((session_id.clone(), session.sender.clone()));
                        }
                        break;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        P2pMetricsCounters::inc(&self.inner.metrics.outbound_queue_closed);
                        let _ = session.shutdown.send(true);
                        disconnect.push((session_id.clone(), session.sender.clone()));
                        break;
                    }
                }
            }
        }
        if !disconnect.is_empty() {
            let mut sessions = self.inner.sessions.lock().await;
            for (session_id, sender) in disconnect {
                if sessions
                    .get(&session_id)
                    .is_some_and(|session| session.sender.same_channel(&sender))
                {
                    sessions.remove(&session_id);
                }
            }
        }
        Ok(())
    }

    async fn prepare_gossip(&self, envelopes: Vec<GossipEnvelope>) -> Vec<GossipEnvelope> {
        let mut blocks = Vec::new();
        let mut passthrough = Vec::new();

        for envelope in envelopes {
            match envelope {
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
                GossipEnvelope::Inventory { blocks: inv_blocks } => blocks.extend(inv_blocks),
                other => passthrough.push(other),
            }
        }

        blocks.sort_by(|left, right| {
            left.height
                .cmp(&right.height)
                .then_with(|| left.hash.cmp(&right.hash))
        });
        blocks.dedup_by(|left, right| left.hash == right.hash);

        if !blocks.is_empty() {
            passthrough.push(GossipEnvelope::Inventory { blocks });
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

    pub(super) async fn ensure_outbound_sessions(&self) {
        self.inner.peers.lock().await.prune_stale_peers_at(
            crate::app::now_ms(),
            STALE_INBOUND_PEER_RETENTION_MS,
            STALE_DISCOVERED_PEER_RETENTION_MS,
        );
        let max_discovered_outbound_dials = {
            let node = self.inner.node.lock().await;
            if node.ledger().is_setup_placeholder() {
                0
            } else {
                MAX_DISCOVERED_OUTBOUND_DIALS_PER_CYCLE
            }
        };
        let addresses = self
            .inner
            .peers
            .lock()
            .await
            .outbound_session_candidates_at(crate::app::now_ms(), max_discovered_outbound_dials);
        let address_set = addresses.iter().cloned().collect::<BTreeSet<_>>();
        let self_filter_addr = self.self_filter_addr().await;
        let mut sessions = self.inner.sessions.lock().await;
        sessions.retain(|peer, _| {
            let keep = peer.starts_with(INBOUND_SESSION_PREFIX)
                || (address_set.contains(peer)
                    && !is_self_peer_address_for(peer, self.inner.listen_addr, self_filter_addr));
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
            let (shutdown, shutdown_receiver) = watch::channel(false);
            let queue_bytes = Arc::new(tokio::sync::Semaphore::new(PEER_QUEUE_BYTES));
            sessions.insert(
                peer.clone(),
                GossipSession {
                    peer: peer.clone(),
                    sender,
                    shutdown,
                    queue_bytes,
                },
            );
            tokio::spawn(outbound_session(
                self.clone(),
                peer,
                receiver,
                shutdown_receiver,
            ));
        }
    }

    pub(super) async fn forward_outbox(&self) {
        let outbox = self.inner.node.lock().await.drain_outbox();
        if let Err(error) = self.broadcast(outbox).await {
            if debug_logging_enabled() {
                eprintln!("p2p rebroadcast failed: {error:#}");
            }
        }
    }
}

fn byte_bounded_gossip_batches(
    envelopes: Vec<GossipEnvelope>,
) -> Result<Vec<(Arc<[GossipEnvelope]>, u32)>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut batch_bytes = 0_usize;
    for envelope in envelopes {
        let encoded_bytes = serde_json::to_vec(&envelope)?.len().saturating_add(1);
        if encoded_bytes > MAX_GOSSIP_LINE_BYTES.saturating_add(1) {
            anyhow::bail!(
                "p2p message is {} bytes, exceeding {} byte limit",
                encoded_bytes.saturating_sub(1),
                MAX_GOSSIP_LINE_BYTES
            );
        }
        if !batch.is_empty() && batch_bytes.saturating_add(encoded_bytes) > MAX_OUTBOUND_BATCH_BYTES
        {
            batches.push((Arc::from(std::mem::take(&mut batch)), batch_bytes as u32));
            batch_bytes = 0;
        }
        batch_bytes = batch_bytes.saturating_add(encoded_bytes);
        batch.push(envelope);
    }
    if !batch.is_empty() {
        batches.push((Arc::from(batch), batch_bytes as u32));
    }
    Ok(batches)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use crate::{
        app::{GossipEnvelope, NodeCore, PeerBook},
        domain::{Ledger, Wallet},
    };

    use super::super::test_support::{allocations, gossip_network, node};

    #[test]
    fn outbound_batches_are_bounded_by_encoded_bytes() {
        let large_tip = "a".repeat(super::super::MAX_GOSSIP_LINE_BYTES / 2);
        let envelopes = vec![
            GossipEnvelope::PeerStatus {
                height: 1,
                tip_hash: large_tip.clone(),
                time_ms: 1,
            },
            GossipEnvelope::PeerStatus {
                height: 2,
                tip_hash: large_tip,
                time_ms: 2,
            },
        ];

        let batches = super::byte_bounded_gossip_batches(envelopes).unwrap();

        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|(_, bytes)| {
            usize::try_from(*bytes).unwrap() <= super::super::MAX_OUTBOUND_BATCH_BYTES
        }));
    }

    #[tokio::test]
    async fn sync_progress_is_incremental_and_scoped_to_the_active_validation() {
        let alice = Wallet::from_seed("sync-progress-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);

        let first = network.begin_sync_progress(0, 90);
        network.update_sync_progress(first.id(), 23);
        assert_eq!(
            network.sync_progress(),
            Some(super::super::SyncProgress {
                start_height: 0,
                validated_height: 23,
                target_height: 90,
            })
        );

        let second = network.begin_sync_progress(23, 76);
        network.update_sync_progress(second.id(), 41);
        assert_eq!(network.sync_progress().unwrap().target_height, 90);
        drop(first);
        assert_eq!(network.sync_progress().unwrap().validated_height, 41);

        drop(second);
        assert_eq!(network.sync_progress(), None);
    }

    #[tokio::test]
    async fn invalidating_sync_progress_hides_and_rejects_stale_validation() {
        let alice = Wallet::from_seed("invalidated-sync-progress-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);

        let stale = network.begin_sync_progress(20, 90);
        network.update_sync_progress(stale.id(), 41);
        assert!(stale.is_current());

        network.invalidate_sync_progress();

        assert_eq!(network.sync_progress(), None);
        assert!(!stale.is_current());
        network.update_sync_progress(stale.id(), 42);
        assert_eq!(network.sync_progress(), None);

        let fresh = network.begin_sync_progress(0, 90);
        assert!(fresh.is_current());
        assert_eq!(network.sync_progress().unwrap().validated_height, 0);

        drop(stale);
        assert_eq!(network.sync_progress().unwrap().validated_height, 0);
    }

    #[tokio::test]
    async fn identical_chain_validations_cannot_run_concurrently() {
        let alice = Wallet::from_seed("validation-coordinator-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);

        let first = network
            .claim_chain_validation("same-page".to_string())
            .await
            .unwrap();
        let waiting_network = network.clone();
        let waiting = tokio::spawn(async move {
            waiting_network
                .claim_chain_validation("same-page".to_string())
                .await
                .unwrap()
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(!waiting.is_finished());

        drop(first);
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        drop(second);
        assert!(
            network
                .inner
                .chain_validation
                .active
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn peer_exchange_does_not_advertise_self_when_outbound_only() {
        let alice = Wallet::from_seed("px-private-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "127.0.0.1:9545".to_string(),
        ])));
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);
        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"127.0.0.1:9544".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn setup_placeholder_only_dials_explicit_outbound_peers() {
        let alice = Wallet::from_seed("setup-placeholder-outbound-alice");
        let node = Arc::new(tokio::sync::Mutex::new(NodeCore::from_ledger(
            alice,
            Ledger::new(BTreeMap::new(), 1),
            0,
        )));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::from_addresses(vec![
            "127.0.0.1:9545".to_string(),
        ])));
        peers
            .lock()
            .await
            .add_discovered_peer("127.0.0.1:9546".to_string());
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);

        network.ensure_outbound_sessions().await;

        let sessions = network.inner.sessions.lock().await;
        assert!(sessions.contains_key("127.0.0.1:9545"));
        assert!(!sessions.contains_key("127.0.0.1:9546"));
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
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);
        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"iuna.jhx.app:9444".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_exchange_does_not_advertise_discovered_listening_peers() {
        let alice = Wallet::from_seed("px-discovered-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        peers
            .lock()
            .await
            .add_discovered_peer("127.0.0.1:9546".to_string());
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);

        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"127.0.0.1:9546".to_string()));
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
        let network = gossip_network(node, peers, "127.0.0.1:9544".parse().unwrap(), None);
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
        let network = gossip_network(
            node,
            peers,
            "127.0.0.1:0".parse().unwrap(),
            Some("8.8.8.8:9444".parse().unwrap()),
        );
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
}
