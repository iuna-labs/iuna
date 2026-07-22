use std::{collections::BTreeMap, io::ErrorKind, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, mpsc},
    time::{Instant, interval, interval_at, sleep, timeout},
};

use crate::{
    app::{
        BlockInventory, GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION, ProtocolHello, SharedNode,
        SharedPeerBook,
    },
    domain::{Block, ChainSnapshot, Ledger, verify_vdf},
};

const MAX_BLOCK_BATCH: usize = 128;
const MAX_OBJECT_REQUESTS: usize = 128;
const MAX_INVENTORY_ITEMS: usize = 512;
const MAX_PEER_LIST: usize = 128;
const MAX_SNAPSHOT_BLOCKS: usize = 10_000;
const MAX_GOSSIP_LINE_BYTES: usize = 8 * 1024 * 1024;
const PEER_QUEUE_SIZE: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const JOIN_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JOIN_RESPONSE_ENVELOPES: usize = 16;
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

type PeerStatus = (u64, String);
type OutboundBatch = Vec<GossipEnvelope>;

#[derive(Clone)]
pub struct GossipNetwork {
    inner: Arc<GossipNetworkInner>,
}

struct GossipNetworkInner {
    node: SharedNode,
    peers: SharedPeerBook,
    listen_addr: SocketAddr,
    sessions: Mutex<BTreeMap<String, mpsc::Sender<OutboundBatch>>>,
}

impl GossipNetwork {
    pub async fn start(node: SharedNode, peers: SharedPeerBook, addr: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding p2p listener on {addr}"))?;
        let network = Self {
            inner: Arc::new(GossipNetworkInner {
                node,
                peers,
                listen_addr: addr,
                sessions: Mutex::new(BTreeMap::new()),
            }),
        };

        tokio::spawn(accept_loop(network.clone(), listener));
        tokio::spawn(outbound_supervisor(network.clone()));
        network.ensure_outbound_sessions().await;
        Ok(network)
    }

    pub async fn broadcast(&self, envelopes: Vec<GossipEnvelope>) -> Result<()> {
        let envelopes = self.prepare_gossip(envelopes).await;
        if envelopes.is_empty() {
            return Ok(());
        }

        let sessions = self.inner.sessions.lock().await.clone();
        for (peer, sender) in sessions {
            match sender.try_send(envelopes.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.inner
                        .peers
                        .lock()
                        .await
                        .record_error(&peer, "outbound gossip queue is full");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.inner.sessions.lock().await.remove(&peer);
                }
            }
        }
        Ok(())
    }

    async fn prepare_gossip(&self, envelopes: Vec<GossipEnvelope>) -> Vec<GossipEnvelope> {
        let mut txs = Vec::new();
        let mut blocks = Vec::new();
        let mut passthrough = Vec::new();

        for envelope in envelopes {
            match envelope {
                GossipEnvelope::Transaction(tx) => txs.push(tx.signature().to_string()),
                GossipEnvelope::Transactions { transactions } => {
                    txs.extend(
                        transactions
                            .iter()
                            .map(|tx| tx.signature().to_string())
                            .collect::<Vec<_>>(),
                    );
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
        let self_addr = self.inner.listen_addr.to_string();
        let peers = self.inner.peers.lock().await.addresses_except(&self_addr);
        GossipEnvelope::PeerList {
            peers: std::iter::once(self_addr)
                .chain(peers.into_iter())
                .collect(),
        }
    }

    async fn ensure_outbound_sessions(&self) {
        let addresses = self.inner.peers.lock().await.addresses();
        let mut sessions = self.inner.sessions.lock().await;
        for peer in addresses {
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
            eprintln!("p2p rebroadcast failed: {error:#}");
        }
    }
}

async fn accept_loop(network: GossipNetwork, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, remote_addr)) => {
                let network = network.clone();
                tokio::spawn(async move {
                    let result =
                        session_loop(network, stream, remote_addr, None, mpsc::channel(1).1).await;
                    if let Err(error) = result {
                        if !is_quiet_disconnect(&error) {
                            eprintln!(
                                "p2p inbound connection from {remote_addr} failed: {error:#}"
                            );
                        }
                    }
                });
            }
            Err(error) => eprintln!("p2p accept failed: {error:#}"),
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
        let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(&peer)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
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
        let result = session_loop(
            network.clone(),
            stream,
            remote_addr,
            Some(peer.clone()),
            receiver,
        )
        .await;
        match result {
            Ok(()) => {}
            Err(error) if is_quiet_disconnect(&error) => {}
            Err(error) => {
                let message = format!("{error:#}");
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, message.clone());
                eprintln!("p2p session with {peer} failed: {message}");
            }
        }

        let (sender, next_receiver) = mpsc::channel(PEER_QUEUE_SIZE);
        receiver = next_receiver;
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
    let hello = network
        .inner
        .node
        .lock()
        .await
        .hello(Some(network.inner.listen_addr.to_string()));
    write_envelope(&mut writer, &hello).await?;
    let mut reader = BufReader::new(reader);
    let mut sync_tick = interval_at(
        Instant::now() + SESSION_SYNC_INTERVAL,
        SESSION_SYNC_INTERVAL,
    );
    let mut outbound_closed = false;
    let mut peer_status: Option<PeerStatus> = None;
    let mut known_peer = stable_peer;

    if known_peer.is_some() {
        if let Ok(Ok(Some(line))) = timeout(HANDSHAKE_TIMEOUT, read_limited_line(&mut reader)).await
        {
            let envelope = parse_envelope(&line)?;
            if let GossipEnvelope::Hello(hello) = envelope {
                peer_status =
                    Some(process_hello(&network, remote_addr, &mut known_peer, hello).await?);
                maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
            } else if let GossipEnvelope::PeerStatus { height, tip_hash } = envelope {
                peer_status = Some((height, tip_hash.clone()));
                record_peer_status(&network, &known_peer, remote_addr, height, tip_hash).await;
                maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
            } else {
                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                )
                .await?;
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
            }
            line = read_limited_line(&mut reader) => {
                let Some(line) = line? else {
                    return Ok(());
                };
                let envelope = parse_envelope(&line)?;
                if let GossipEnvelope::Hello(hello) = envelope {
                    peer_status = Some(process_hello(&network, remote_addr, &mut known_peer, hello).await?);
                    maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
                    continue;
                }
                if let GossipEnvelope::PeerStatus { height, tip_hash } = &envelope {
                    peer_status = Some((*height, tip_hash.clone()));
                    record_peer_status(&network, &known_peer, remote_addr, *height, tip_hash.clone()).await;
                    maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
                    continue;
                }

                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                ).await?;
            }
        }
    }
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
            if !transactions.is_empty() {
                write_envelope(writer, &GossipEnvelope::Transactions { transactions }).await?;
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
        GossipEnvelope::PeerAnnouncement { address } => {
            let peer = normalize_advertised_peer(&address, remote_addr)?;
            *known_peer = Some(peer.clone());
            {
                let mut peers = network.inner.peers.lock().await;
                peers.add_peer(peer.clone());
                peers.record_received(&peer, 1);
            }
            let snapshot = network.inner.node.lock().await.chain_snapshot();
            write_envelope(writer, &GossipEnvelope::ChainSnapshot(snapshot)).await?;
        }
        GossipEnvelope::PeerList { peers } => {
            apply_peer_list(network, remote_addr, peers).await?;
        }
        GossipEnvelope::Block(block) => {
            let needs_vdf = {
                let node = network.inner.node.lock().await;
                node.block_requires_vdf_verification(&block)
            };
            let result = match needs_vdf {
                Ok(false) => Ok(()),
                Ok(true) => match verify_block_vdf(block).await {
                    Ok(block) => network
                        .inner
                        .node
                        .lock()
                        .await
                        .receive_preverified_block(block),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
        GossipEnvelope::Blocks { blocks } => {
            let local_ledger = network.inner.node.lock().await.clone_ledger();
            let result = match validate_blocks_extension(local_ledger, blocks).await {
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
            let local_ledger = network.inner.node.lock().await.clone_ledger();
            let result = match validate_snapshot_extension(local_ledger, snapshot).await {
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
    let (peer_height, peer_tip_hash) = peer_status;
    if *peer_height > local_height {
        write_envelope(
            writer,
            &GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: MAX_BLOCK_BATCH,
            },
        )
        .await?;
    } else if *peer_height == local_height && peer_tip_hash != &local_tip_hash {
        write_envelope(writer, &GossipEnvelope::ChainSnapshotRequest).await?;
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
            .map(|block| (block.height, block.hash.clone())),
        GossipEnvelope::ChainSnapshot(snapshot) => snapshot
            .blocks
            .last()
            .map(|block| (block.height, block.hash.clone())),
        _ => None,
    });
    write_payload(writer, &payload).await?;
    Ok(updated_status)
}

async fn catchup_payload_for_peer(
    node: &SharedNode,
    peer_status: &PeerStatus,
) -> Vec<GossipEnvelope> {
    let (peer_height, peer_tip_hash) = peer_status;
    let node = node.lock().await;
    let local_status = node.ledger().status();
    if *peer_height < local_status.height {
        let blocks = node.blocks_from(peer_height + 1, MAX_BLOCK_BATCH);
        if blocks.is_empty() {
            Vec::new()
        } else {
            vec![GossipEnvelope::Blocks { blocks }]
        }
    } else if *peer_height == local_status.height && peer_tip_hash != &local_status.tip_hash {
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
    let self_addr = network.inner.listen_addr.to_string();
    let mut peerbook = network.inner.peers.lock().await;
    for address in peers {
        let peer = normalize_advertised_peer(&address, remote_addr)?;
        if peer != self_addr {
            peerbook.add_peer(peer);
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

async fn read_limited_line(reader: &mut BufReader<OwnedReadHalf>) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            anyhow::bail!("peer closed before completing a gossip message");
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if bytes.len() + newline > MAX_GOSSIP_LINE_BYTES {
                anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
            }
            bytes.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
            return String::from_utf8(bytes)
                .context("p2p message is not valid UTF-8")
                .map(Some);
        }

        if bytes.len() + available.len() > MAX_GOSSIP_LINE_BYTES {
            anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
        }
        let consumed = available.len();
        bytes.extend_from_slice(available);
        reader.consume(consumed);
    }
}

fn parse_envelope(line: &str) -> Result<GossipEnvelope> {
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
        GossipEnvelope::Transactions { transactions } => {
            ensure_len("transaction batch", transactions.len(), MAX_OBJECT_REQUESTS)?;
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
        GossipEnvelope::Hello(_)
        | GossipEnvelope::PeerStatus { .. }
        | GossipEnvelope::ChainSnapshotRequest
        | GossipEnvelope::Transaction(_)
        | GossipEnvelope::Block(_)
        | GossipEnvelope::PeerAnnouncement { .. } => {}
    }
    Ok(())
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
    let Some((peer_height, peer_tip_hash)) = peer_status else {
        return envelopes.to_vec();
    };

    let node = node.lock().await;
    let local_status = node.ledger().status();
    if peer_height < local_status.height {
        let mut payload = vec![GossipEnvelope::Blocks {
            blocks: node.blocks_from(peer_height + 1, MAX_BLOCK_BATCH),
        }];
        payload.extend(
            envelopes
                .iter()
                .filter(|envelope| !matches!(envelope, GossipEnvelope::Block(_)))
                .cloned(),
        );
        return payload;
    }

    if peer_height == local_status.height && peer_tip_hash != local_status.tip_hash {
        return vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
    }

    if peer_needs_snapshot(peer_height, envelopes) {
        return vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
    }

    envelopes
        .iter()
        .filter(|envelope| match envelope {
            GossipEnvelope::Block(block) => block.height > peer_height,
            GossipEnvelope::Inventory { blocks, .. } => {
                blocks.iter().any(|block| block.height > peer_height)
            }
            _ => true,
        })
        .map(|envelope| match envelope {
            GossipEnvelope::Inventory { txs, blocks } => GossipEnvelope::Inventory {
                txs: txs.clone(),
                blocks: blocks
                    .iter()
                    .filter(|block| block.height > peer_height)
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
    fetch_peer_status(peer).await.map(|(height, _)| height)
}

async fn fetch_peer_status(peer: &str) -> Result<PeerStatus> {
    let stream = TcpStream::connect(peer)
        .await
        .with_context(|| format!("connecting to peer {peer}"))?;
    let (reader, _writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let line = read_limited_line(&mut reader)
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
            Ok((hello.height, hello.tip_hash))
        }
        GossipEnvelope::PeerStatus { height, tip_hash } => Ok((height, tip_hash)),
        other => anyhow::bail!("peer {peer} sent {other:?} instead of peer status"),
    }
}

pub async fn fetch_snapshot_with_announcement(
    peer: &str,
    advertised_addr: Option<SocketAddr>,
) -> Result<ChainSnapshot> {
    let stream = TcpStream::connect(peer)
        .await
        .with_context(|| format!("connecting to join peer {peer}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let line = read_limited_line(&mut reader)
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

    if let Some(address) = advertised_addr {
        let line = serde_json::to_string(&GossipEnvelope::PeerAnnouncement {
            address: address.to_string(),
        })?;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        if let Ok(Ok(Some(line))) =
            timeout(Duration::from_secs(2), read_limited_line(&mut reader)).await
        {
            if let GossipEnvelope::ChainSnapshot(fresh_snapshot) = parse_envelope(&line)? {
                if snapshot_height(&fresh_snapshot) >= snapshot_height(&snapshot) {
                    return Ok(fresh_snapshot);
                }
            }
        }
    }

    Ok(snapshot)
}

async fn read_join_snapshot_response(
    peer: &str,
    reader: &mut BufReader<OwnedReadHalf>,
) -> Result<ChainSnapshot> {
    for _ in 0..MAX_JOIN_RESPONSE_ENVELOPES {
        let line = timeout(JOIN_RESPONSE_TIMEOUT, read_limited_line(reader))
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
        | GossipEnvelope::Inventory { .. } => Ok(None),
        other => anyhow::bail!("join peer {peer} sent {other:?} instead of a chain snapshot"),
    }
}

async fn validate_snapshot_extension(
    mut ledger: Ledger,
    snapshot: ChainSnapshot,
) -> Result<Ledger> {
    let missing_blocks = ledger.missing_snapshot_blocks(&snapshot)?;
    verify_blocks_vdf(missing_blocks).await?;

    tokio::task::spawn_blocking(move || {
        ledger.extend_from_preverified_snapshot(snapshot)?;
        Ok(ledger)
    })
    .await
    .context("chain snapshot extension worker failed")?
}

async fn validate_blocks_extension(mut ledger: Ledger, blocks: Vec<Block>) -> Result<Ledger> {
    if blocks.is_empty() {
        return Ok(ledger);
    }
    verify_blocks_vdf(blocks.clone()).await?;

    tokio::task::spawn_blocking(move || {
        for block in blocks {
            ledger.apply_preverified_block(block)?;
        }
        Ok(ledger)
    })
    .await
    .context("block batch extension worker failed")?
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
    height: u64,
    tip_hash: String,
) {
    if let Some(peer) = known_peer {
        network
            .inner
            .peers
            .lock()
            .await
            .record_status(peer, height, tip_hash);
    } else {
        network
            .inner
            .peers
            .lock()
            .await
            .record_received(&remote_addr.to_string(), 1);
    }
}

async fn process_hello(
    network: &GossipNetwork,
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
    let local_genesis = network
        .inner
        .node
        .lock()
        .await
        .ledger()
        .genesis_hash()
        .to_string();
    if hello.genesis_hash != local_genesis {
        anyhow::bail!(
            "wrong genesis {}; expected {local_genesis}",
            hello.genesis_hash
        );
    }

    if let Some(listen_addr) = &hello.listen_addr {
        let peer = normalize_advertised_peer(listen_addr, remote_addr)?;
        if peer != network.inner.listen_addr.to_string() {
            *known_peer = Some(peer.clone());
            network.inner.peers.lock().await.add_peer(peer);
        }
    }
    record_peer_status(
        network,
        known_peer,
        remote_addr,
        hello.height,
        hello.tip_hash.clone(),
    )
    .await;
    Ok((hello.height, hello.tip_hash))
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
                    .record_inbound_error(&peer, message.clone());
            }
            eprintln!("p2p envelope from {peer} ignored: {message}");
        }
    }
}

fn next_reconnect_delay(current: Duration) -> Duration {
    (current * 2).min(MAX_RECONNECT_DELAY)
}

fn snapshot_height(snapshot: &ChainSnapshot) -> u64 {
    snapshot
        .blocks
        .last()
        .map(|block| block.height)
        .unwrap_or(0)
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
    use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

    use crate::{
        app::{
            BlockInventory, GossipEnvelope, NETWORK_ID, NodeCore, PROTOCOL_VERSION, PeerBook,
            PeerDirection, ProtocolHello,
        },
        domain::{Amount, GenesisBurn, Ledger, Wallet},
    };

    use super::{
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
    fn parser_applies_envelope_limits() {
        let line = serde_json::to_string(&GossipEnvelope::BlockRequest {
            hashes: vec!["hash".to_string(); MAX_OBJECT_REQUESTS + 1],
        })
        .unwrap();

        let error = parse_envelope(&line).unwrap_err();

        assert!(error.to_string().contains("block request"));
    }

    #[test]
    fn peer_needs_snapshot_when_block_gossip_skips_a_height() {
        let block = crate::domain::Block {
            height: 10,
            prev_hash: "prev".to_string(),
            timestamp_ms: 1,
            miner: "miner".to_string(),
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
                address: "127.0.0.1:9444".to_string()
            }]
        ));
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
            Some((0, "genesis".to_string())),
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
    async fn tx_and_block_gossip_is_announced_as_inventory() {
        let alice = Wallet::from_seed("inventory-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node: Arc::clone(&node),
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
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

        assert_eq!(prepared.len(), 1);
        match &prepared[0] {
            GossipEnvelope::Inventory { txs, blocks } => {
                assert_eq!(txs, &[tx_signature]);
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].hash, block_hash);
            }
            other => panic!("expected inventory, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn inventory_requests_only_missing_objects() {
        let alice = Wallet::from_seed("missing-inv-alice");
        let bob = Wallet::from_seed("missing-inv-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let mut local = node("local", alice.clone(), allocations.clone());
        let mut remote = node("remote", bob, allocations);
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

        let payload = super::catchup_payload_for_peer(&node, &(1, "old-tip".to_string())).await;

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
    async fn hello_rejects_wrong_network_or_genesis() {
        let alice = Wallet::from_seed("hello-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let network = super::GossipNetwork {
            inner: Arc::new(super::GossipNetworkInner {
                node,
                peers: Arc::new(tokio::sync::Mutex::new(PeerBook::default())),
                listen_addr: "127.0.0.1:9544".parse().unwrap(),
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
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
            listen_addr: Some("127.0.0.1:9545".to_string()),
            height: 0,
            tip_hash: "tip".to_string(),
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
            height: 0,
            tip_hash: "tip".to_string(),
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
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            }),
        };

        super::record_peer_status(
            &network,
            &None,
            "127.0.0.1:51729".parse().unwrap(),
            4,
            "tip".to_string(),
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
                    tip_hash: "tip".to_string()
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
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            }),
        };

        match network.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(peers.contains(&"127.0.0.1:9544".to_string()));
                assert!(peers.contains(&"127.0.0.1:9545".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }
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
                sessions: tokio::sync::Mutex::new(BTreeMap::new()),
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

    fn node(name: &str, wallet: Wallet, allocations: BTreeMap<String, Amount>) -> NodeCore {
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            25,
        )
        .unwrap();
        NodeCore::from_ledger(name.to_string(), wallet, ledger, 0)
    }

    fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
        wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), amount))
            .collect()
    }
}
