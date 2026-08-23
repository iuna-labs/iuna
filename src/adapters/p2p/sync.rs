use std::net::SocketAddr;

use anyhow::Result;
use tokio::net::tcp::OwnedWriteHalf;

use super::metrics::P2pMetricsCounters;
use super::peer_addr::{
    is_self_peer_address_for, normalize_advertised_peer, peer_has_block_gap,
    peer_list_address_is_discoverable,
};
use super::{
    GossipNetwork, MAX_BLOCK_BATCH, PeerStatus, byte_bounded_block_page, write_envelope,
    write_payload,
};
use crate::app::{GossipEnvelope, SharedNode, debug_logging_enabled};

pub(super) async fn maybe_request_catchup(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    peer_status: &PeerStatus,
) -> Result<()> {
    let (local_height, local_tip_hash) = {
        let node = network.inner.node.lock().await;
        let status = node.ledger().status();
        (status.height, status.tip_hash)
    };
    if peer_status.request_bootstrap {
        write_envelope(writer, &GossipEnvelope::ChainBootstrapRequest).await?;
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
        let locator = network.inner.node.lock().await.block_locator();
        write_envelope(
            writer,
            &GossipEnvelope::BlockLocatorRequest {
                locator,
                limit: MAX_BLOCK_BATCH,
            },
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn push_catchup_to_peer(
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
        GossipEnvelope::ChainBootstrap(bootstrap) => {
            Some(PeerStatus::new(0, bootstrap.genesis_block.hash.clone()))
        }
        _ => None,
    });
    write_payload(writer, &payload).await?;
    Ok(updated_status)
}

pub(super) async fn catchup_payload_for_peer(
    node: &SharedNode,
    peer_status: &PeerStatus,
) -> Vec<GossipEnvelope> {
    let mut node = node.lock().await;
    let local_status = node.ledger().status();
    if node.ledger().is_setup_placeholder() {
        return Vec::new();
    }
    if peer_status.push_bootstrap {
        return vec![GossipEnvelope::ChainBootstrap(node.chain_bootstrap())];
    }
    if peer_status.height < local_status.height {
        let blocks =
            byte_bounded_block_page(node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH));
        return (!blocks.is_empty())
            .then_some(GossipEnvelope::Blocks { blocks })
            .into_iter()
            .collect();
    } else if peer_status.height == local_status.height
        && peer_status.tip_hash != local_status.tip_hash
    {
        return Vec::new();
    }
    node.mempool_gossip()
}

pub(super) async fn apply_peer_list(
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
            peerbook.add_discovered_peer(peer);
        } else {
            P2pMetricsCounters::inc(&network.inner.metrics.self_peer_skips);
        }
    }
    Ok(())
}

pub(super) async fn write_peer_exchange(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    known_peer: &Option<String>,
) -> Result<()> {
    let envelope = network.peer_exchange().await;
    let GossipEnvelope::PeerList { peers } = &envelope else {
        return Ok(());
    };
    if peers.is_empty() {
        return Ok(());
    }
    write_envelope(writer, &envelope).await?;
    if let Some(peer) = known_peer {
        network.inner.peers.lock().await.record_sent(peer, 1);
    }
    Ok(())
}

pub(super) async fn envelopes_for_peer(
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
        let blocks =
            byte_bounded_block_page(node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH));
        return (!blocks.is_empty())
            .then_some(GossipEnvelope::Blocks { blocks })
            .into_iter()
            .collect();
    }

    if peer_status.height == local_status.height && peer_status.tip_hash != local_status.tip_hash {
        return Vec::new();
    }

    if peer_has_block_gap(peer_status.height, envelopes) {
        let blocks =
            byte_bounded_block_page(node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH));
        return vec![GossipEnvelope::Blocks { blocks }];
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
            GossipEnvelope::Inventory { blocks } => GossipEnvelope::Inventory {
                blocks: blocks
                    .iter()
                    .filter(|block| block.height > peer_status.height)
                    .cloned()
                    .collect(),
            },
            other => other.clone(),
        })
        .filter(|envelope| match envelope {
            GossipEnvelope::Inventory { blocks } => !blocks.is_empty(),
            _ => true,
        })
        .collect()
}
