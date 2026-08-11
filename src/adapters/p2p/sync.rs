use std::net::SocketAddr;

use anyhow::Result;
use tokio::net::tcp::OwnedWriteHalf;

use super::metrics::P2pMetricsCounters;
use super::peer_addr::{
    is_self_peer_address_for, normalize_advertised_peer, peer_list_address_is_discoverable,
    peer_needs_snapshot,
};
use super::{GossipNetwork, MAX_BLOCK_BATCH, PeerStatus, write_envelope, write_payload};
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
        GossipEnvelope::ChainSnapshot(snapshot) => snapshot
            .blocks
            .last()
            .map(|block| PeerStatus::new(block.height, block.hash.clone())),
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
    let mempool = node.mempool_gossip();
    if peer_status.push_snapshot {
        let mut payload = vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
        payload.extend(mempool);
        return payload;
    }
    if peer_status.height < local_status.height {
        let blocks = node.blocks_from(peer_status.height + 1, MAX_BLOCK_BATCH);
        if blocks.is_empty() {
            mempool
        } else {
            let mut payload = vec![GossipEnvelope::Blocks { blocks }];
            payload.extend(mempool);
            payload
        }
    } else if peer_status.height == local_status.height
        && peer_status.tip_hash != local_status.tip_hash
    {
        let mut payload = vec![GossipEnvelope::ChainSnapshot(node.chain_snapshot())];
        payload.extend(mempool);
        payload
    } else {
        mempool
    }
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
            peerbook.add_peer(peer);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        app::{GossipEnvelope, PeerBook},
        domain::Wallet,
    };

    use super::super::{
        PeerStatus,
        test_support::{allocations, gossip_network, node, queue_plaintext_burn},
    };
    use super::{apply_peer_list, catchup_payload_for_peer, envelopes_for_peer};

    #[tokio::test]
    async fn peer_payload_repairs_lagging_peer_without_networking() {
        let alice = Wallet::from_seed("p2p-alice");
        let bob = Wallet::from_seed("p2p-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node(
            "alice",
            alice.clone(),
            allocations,
        )));
        let block = {
            let mut node = node.lock().await;
            queue_plaintext_burn(&mut node, &alice, 1);
            node.drain_outbox();
            let block = node.mine_one_at(1).unwrap();
            node.drain_outbox();
            block
        };

        let payload = envelopes_for_peer(
            Some(&node),
            Some(PeerStatus::new(0, "genesis".to_string())),
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
    async fn session_catchup_payload_pushes_missing_blocks_to_lagging_peer() {
        let alice = Wallet::from_seed("catchup-alice");
        let bob = Wallet::from_seed("catchup-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node(
            "alice",
            alice.clone(),
            allocations,
        )));
        {
            let mut node = node.lock().await;
            for height in 1..=3 {
                queue_plaintext_burn(&mut node, &alice, 1);
                node.drain_outbox();
                node.mine_one_at(height).unwrap();
                node.drain_outbox();
            }
        }

        let payload =
            catchup_payload_for_peer(&node, &PeerStatus::new(1, "old-tip".to_string())).await;

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
    async fn session_catchup_payload_pushes_blinded_mempool_to_synced_peer() {
        let alice = Wallet::from_seed("catchup-mempool-alice");
        let bob = Wallet::from_seed("catchup-mempool-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node(
            "alice",
            alice.clone(),
            allocations,
        )));
        let expected_commitment = {
            let mut node = node.lock().await;
            let tx = node.ledger().build_burn(&alice, 1, 0).unwrap();
            let built = node
                .ledger()
                .build_blinded_transaction(&alice, tx, 20)
                .unwrap();
            let commitment = built.transaction.commitment.clone();
            node.receive_blinded_transaction(built.transaction).unwrap();
            node.drain_outbox();
            commitment
        };
        let peer_status = {
            let node = node.lock().await;
            let status = node.ledger().status();
            PeerStatus::new(status.height, status.tip_hash)
        };

        let payload = catchup_payload_for_peer(&node, &peer_status).await;

        assert_eq!(payload.len(), 1);
        match &payload[0] {
            GossipEnvelope::BlindedTransactions { transactions } => {
                assert_eq!(transactions.len(), 1);
                assert_eq!(transactions[0].commitment, expected_commitment);
            }
            other => panic!("expected blinded mempool payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_list_adds_stable_outbound_peers() {
        let alice = Wallet::from_seed("px-recv-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "127.0.0.1:9544".parse().unwrap(),
            None,
        );
        apply_peer_list(
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
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "127.0.0.1:9544".parse().unwrap(),
            None,
        );
        apply_peer_list(
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
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9444".parse().unwrap(),
            Some("8.8.8.8:9444".parse().unwrap()),
        );

        apply_peer_list(
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
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9444".parse().unwrap(),
            None,
        );

        apply_peer_list(
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
    async fn peer_list_ignores_loopback_alias_for_unspecified_self() {
        let alice = Wallet::from_seed("px-self-alias-alice");
        let allocations = allocations(std::slice::from_ref(&alice), 1_000);
        let node = Arc::new(tokio::sync::Mutex::new(node("alice", alice, allocations)));
        let peers = Arc::new(tokio::sync::Mutex::new(PeerBook::default()));
        let network = gossip_network(
            node,
            Arc::clone(&peers),
            "0.0.0.0:9545".parse().unwrap(),
            None,
        );

        apply_peer_list(
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
}
