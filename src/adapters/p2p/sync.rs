use std::net::SocketAddr;

use anyhow::Result;
use tokio::net::tcp::OwnedWriteHalf;

use super::metrics::P2pMetricsCounters;
use super::peer_addr::{
    is_self_peer_address_for, normalize_advertised_peer, peer_list_address_is_discoverable,
};
use super::{GossipNetwork, PeerStatus, negotiated_block_batch_limit, write_envelope};
use crate::app::{
    CAPABILITY_TRANSACTION_V2_MEMPOOL, GossipEnvelope, SharedNode, debug_logging_enabled,
};

pub(super) async fn maybe_request_catchup(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    peer_status: &PeerStatus,
) -> Result<bool> {
    let (local_height, local_tip_hash) = {
        let node = network.inner.node.lock().await;
        let status = node.ledger().status();
        (status.height, status.tip_hash)
    };
    let block_batch_limit = negotiated_block_batch_limit(&peer_status.capabilities);
    if peer_status.request_bootstrap {
        write_envelope(writer, &GossipEnvelope::ChainBootstrapRequest).await?;
        return Ok(true);
    } else if peer_status.height > local_height {
        write_envelope(
            writer,
            &GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: block_batch_limit,
            },
        )
        .await?;
        return Ok(true);
    } else if peer_status.height == local_height && peer_status.tip_hash != local_tip_hash {
        let locator = network.inner.node.lock().await.block_locator();
        write_envelope(
            writer,
            &GossipEnvelope::BlockLocatorRequest {
                locator,
                limit: block_batch_limit,
            },
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
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
    let supports_v2_mempool = peer_status
        .as_ref()
        .is_some_and(|status| status.supports(CAPABILITY_TRANSACTION_V2_MEMPOOL));
    let envelopes = envelopes
        .iter()
        .filter(|envelope| {
            supports_v2_mempool
                || !matches!(
                    envelope,
                    GossipEnvelope::TransactionV2 { .. } | GossipEnvelope::TransactionsV2 { .. }
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    let Some(peer_status) = peer_status else {
        return envelopes;
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
        return envelopes
            .iter()
            .filter(|envelope| {
                matches!(
                    envelope,
                    GossipEnvelope::PeerStatus { .. } | GossipEnvelope::Inventory { .. }
                )
            })
            .cloned()
            .collect();
    }

    if peer_status.height == local_status.height && peer_status.tip_hash != local_status.tip_hash {
        return Vec::new();
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
