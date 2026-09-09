use std::net::SocketAddr;

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use tokio::net::tcp::OwnedWriteHalf;

use crate::{
    app::{GossipEnvelope, debug_logging_enabled},
    domain::{BurnBundle, Transaction},
};

use super::{
    GossipNetwork, MAX_BLOCK_BATCH, P2pMetricsCounters, apply_peer_list, forget_stale_self_peer,
    is_possible_fork_error, normalize_advertised_peer, peer_verification_response, process_hello,
    validate_blocks_extension, validate_chain_bootstrap, verify_block_vdf, write_envelope,
};

pub(super) async fn respond_to_peer_verification_challenge(
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

pub(super) async fn process_envelope(
    network: &GossipNetwork,
    writer: &mut OwnedWriteHalf,
    remote_addr: SocketAddr,
    known_peer: &mut Option<String>,
    envelope: GossipEnvelope,
) -> Result<bool> {
    let mut requested_chain_data = false;
    match envelope {
        GossipEnvelope::Hello(hello) => {
            let _ = process_hello(network, remote_addr, known_peer, hello).await?;
        }
        GossipEnvelope::ChainBootstrapRequest => {
            let bootstrap = network.inner.node.lock().await.chain_bootstrap();
            write_envelope(writer, &GossipEnvelope::ChainBootstrap(bootstrap)).await?;
        }
        GossipEnvelope::BlockLocatorRequest { locator, limit } => {
            let blocks = network
                .inner
                .node
                .lock()
                .await
                .blocks_after_locator(&locator, limit.min(MAX_BLOCK_BATCH));
            let blocks = super::byte_bounded_block_page(blocks);
            write_envelope(writer, &GossipEnvelope::Blocks { blocks }).await?;
        }
        GossipEnvelope::BlockRangeRequest { from_height, limit } => {
            let blocks = network
                .inner
                .node
                .lock()
                .await
                .blocks_from(from_height, limit.min(MAX_BLOCK_BATCH));
            let blocks = super::byte_bounded_block_page(blocks);
            write_envelope(writer, &GossipEnvelope::Blocks { blocks }).await?;
        }
        GossipEnvelope::BlockRequest { hashes } => {
            let blocks = network.inner.node.lock().await.blocks_by_hash(&hashes);
            if !blocks.is_empty() {
                let blocks = super::byte_bounded_block_page(blocks);
                write_envelope(writer, &GossipEnvelope::Blocks { blocks }).await?;
            }
        }
        GossipEnvelope::PeerAnnouncement { address, node_id } => {
            let peer = normalize_advertised_peer(&address, remote_addr)?;
            if network.is_self_peer(&peer).await {
                P2pMetricsCounters::inc(&network.inner.metrics.self_peer_rejections);
                forget_stale_self_peer(network, known_peer).await;
            } else if node_id.is_some() && debug_logging_enabled() {
                eprintln!("p2p peer announcement for {peer} ignored until hello verification");
            }
            let bootstrap = network.inner.node.lock().await.chain_bootstrap();
            write_envelope(writer, &GossipEnvelope::ChainBootstrap(bootstrap)).await?;
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
            process_transactions(network, remote_addr, known_peer, vec![tx]).await;
        }
        GossipEnvelope::Transactions { transactions } => {
            process_transactions(network, remote_addr, known_peer, transactions).await;
        }
        GossipEnvelope::BurnBundle(bundle) => {
            process_burn_bundles(network, remote_addr, known_peer, vec![bundle]).await;
        }
        GossipEnvelope::BurnBundles { bundles } => {
            process_burn_bundles(network, remote_addr, known_peer, bundles).await;
        }
        GossipEnvelope::BurnBundleRequest {
            height,
            prev_hash,
            slots,
        } => {
            let bundles = network
                .inner
                .node
                .lock()
                .await
                .burn_bundles_for_request(height, &prev_hash, &slots);
            if !bundles.is_empty() {
                write_envelope(writer, &GossipEnvelope::BurnBundles { bundles }).await?;
            }
        }
        GossipEnvelope::Block(block) => {
            let adjusted_time_ms = super::network_adjusted_time_ms(network).await;
            let (needs_vdf, base_tip, sync_generation) = {
                let node = network.inner.node.lock().await;
                (
                    node.block_requires_vdf_verification_at(&block, adjusted_time_ms),
                    node.ledger().tip_hash().to_string(),
                    network.sync_generation(),
                )
            };
            let result = match needs_vdf {
                Ok(false) => Ok(()),
                Ok(true) => {
                    let validation_key =
                        chain_validation_key("block", &base_tip, std::slice::from_ref(&block));
                    let Some(_validation) = network.claim_chain_validation(validation_key).await
                    else {
                        return Ok(false);
                    };
                    let base_is_current = {
                        let node = network.inner.node.lock().await;
                        network.sync_generation_is_current(sync_generation)
                            && node.ledger().tip_hash() == base_tip
                    };
                    if !base_is_current {
                        return Ok(false);
                    }
                    match verify_block_vdf(block).await {
                        Ok(block) => {
                            let mut node = network.inner.node.lock().await;
                            if network.sync_generation_is_current(sync_generation)
                                && node.ledger().tip_hash() == base_tip
                            {
                                node.receive_preverified_block_at(block, adjusted_time_ms)
                            } else {
                                Ok(())
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            let request_locator = result.as_ref().err().is_some_and(is_possible_fork_error);
            record_rejected_chain_payload(
                network,
                &network.inner.metrics.rejected_blocks,
                "block",
                &result,
            );
            record_inbound_result(network, known_peer, remote_addr, result).await;
            if request_locator {
                request_fork_blocks(network, writer).await?;
                requested_chain_data = true;
            }
            network.forward_outbox().await;
        }
        GossipEnvelope::Blocks { blocks } => {
            let adjusted_time_ms = super::network_adjusted_time_ms(network).await;
            let (base_tip, sync_generation) = {
                let node = network.inner.node.lock().await;
                if blocks.is_empty() || node.ledger().contains_block_sequence(&blocks) {
                    drop(node);
                    record_inbound_result(network, known_peer, remote_addr, Ok(())).await;
                    return Ok(false);
                }
                (
                    node.ledger().tip_hash().to_string(),
                    network.sync_generation(),
                )
            };
            let validation_key = chain_validation_key("blocks", &base_tip, &blocks);
            let Some(_validation) = network.claim_chain_validation(validation_key).await else {
                return Ok(false);
            };
            let (local_ledger, progress_guard) = {
                let node = network.inner.node.lock().await;
                if !network.sync_generation_is_current(sync_generation)
                    || node.ledger().tip_hash() != base_tip
                    || node.ledger().contains_block_sequence(&blocks)
                {
                    drop(node);
                    record_inbound_result(network, known_peer, remote_addr, Ok(())).await;
                    return Ok(false);
                }
                let start_height = blocks
                    .first()
                    .map(|block| block.height.saturating_sub(1))
                    .unwrap_or_else(|| node.ledger().height());
                let target_height = blocks
                    .last()
                    .map(|block| block.height)
                    .unwrap_or(start_height);
                let progress_guard = network.begin_sync_progress(start_height, target_height);
                (node.clone_ledger(), progress_guard)
            };
            let progress_id = progress_guard.id();
            let progress_network = network.clone();
            let result = match validate_blocks_extension(
                local_ledger,
                blocks,
                adjusted_time_ms,
                move |height| progress_network.update_sync_progress(progress_id, height),
            )
            .await
            {
                Ok(ledger) => {
                    let mut node = network.inner.node.lock().await;
                    if progress_guard.is_current()
                        && network.sync_generation_is_current(sync_generation)
                        && node.ledger().tip_hash() == base_tip
                    {
                        node.import_verified_ledger(ledger).map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                Err(error) => Err(error),
            };
            drop(progress_guard);
            let request_locator = result.as_ref().err().is_some_and(is_possible_fork_error);
            record_rejected_chain_payload(
                network,
                &network.inner.metrics.rejected_block_batches,
                "block batch",
                &result,
            );
            record_inbound_result(network, known_peer, remote_addr, result).await;
            if request_locator {
                request_fork_blocks(network, writer).await?;
                requested_chain_data = true;
            }
            network.forward_outbox().await;
        }
        GossipEnvelope::ChainBootstrap(bootstrap) => {
            let sync_generation = network.sync_generation();
            let (base_tip, expected_profile_id) = {
                let node = network.inner.node.lock().await;
                (
                    node.ledger().tip_hash().to_string(),
                    node.ledger().launch_profile().profile_id.clone(),
                )
            };
            let validation_key = chain_validation_key(
                "bootstrap",
                &base_tip,
                std::slice::from_ref(&bootstrap.genesis_block),
            );
            let Some(_validation) = network.claim_chain_validation(validation_key).await else {
                return Ok(false);
            };
            let base_is_current = {
                let node = network.inner.node.lock().await;
                network.sync_generation_is_current(sync_generation)
                    && node.ledger().tip_hash() == base_tip
            };
            if !base_is_current {
                return Ok(false);
            }
            let adjusted_time_ms = super::network_adjusted_time_ms(network).await;
            let result =
                match validate_chain_bootstrap(&expected_profile_id, bootstrap, adjusted_time_ms)
                    .await
                {
                    Ok(ledger) => {
                        let mut node = network.inner.node.lock().await;
                        if network.sync_generation_is_current(sync_generation)
                            && node.ledger().tip_hash() == base_tip
                        {
                            node.import_verified_ledger(ledger).map(|_| ())
                        } else {
                            Ok(())
                        }
                    }
                    Err(error) => Err(error),
                };
            record_rejected_chain_payload(
                network,
                &network.inner.metrics.rejected_snapshots,
                "chain bootstrap",
                &result,
            );
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
        other => {
            let result = network.inner.node.lock().await.receive(other);
            record_inbound_result(network, known_peer, remote_addr, result).await;
            network.forward_outbox().await;
        }
    }
    Ok(requested_chain_data)
}

fn chain_validation_key(kind: &str, base_tip: &str, blocks: &[crate::domain::Block]) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(base_tip.as_bytes());
    for block in blocks {
        digest.update(block.height.to_be_bytes());
        digest.update(block.prev_hash.as_bytes());
        digest.update([0]);
        digest.update(block.hash.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

async fn request_fork_blocks(network: &GossipNetwork, writer: &mut OwnedWriteHalf) -> Result<()> {
    let locator = network.inner.node.lock().await.block_locator();
    write_envelope(
        writer,
        &GossipEnvelope::BlockLocatorRequest {
            locator,
            limit: MAX_BLOCK_BATCH,
        },
    )
    .await
}

async fn process_transactions(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &Option<String>,
    transactions: Vec<Transaction>,
) {
    let first_error = {
        let mut node = network.inner.node.lock().await;
        let mut first_error = None;
        for tx in transactions {
            if let Err(error) = node.receive_gossiped_transaction(tx) {
                first_error.get_or_insert(error);
            }
        }
        first_error
    };
    record_inbound_result(
        network,
        known_peer,
        remote_addr,
        first_error
            .map(|error| Err(anyhow!(format!("{error:#}"))))
            .unwrap_or(Ok(())),
    )
    .await;
    network.forward_outbox().await;
}

async fn process_burn_bundles(
    network: &GossipNetwork,
    remote_addr: SocketAddr,
    known_peer: &Option<String>,
    bundles: Vec<BurnBundle>,
) {
    let first_error = {
        let mut node = network.inner.node.lock().await;
        let mut first_error = None;
        for bundle in bundles {
            if let Err(error) = node.receive_burn_bundle(bundle) {
                first_error.get_or_insert(error);
            }
        }
        first_error
    };
    record_inbound_result(
        network,
        known_peer,
        remote_addr,
        first_error
            .map(|error| Err(anyhow!(format!("{error:#}"))))
            .unwrap_or(Ok(())),
    )
    .await;
    network.forward_outbox().await;
}

fn record_rejected_chain_payload(
    network: &GossipNetwork,
    counter: &std::sync::atomic::AtomicU64,
    kind: &str,
    result: &Result<()>,
) {
    let Err(error) = result else {
        return;
    };
    P2pMetricsCounters::inc(counter);
    P2pMetricsCounters::set_last(
        &network.inner.metrics.last_chain_payload_error,
        format!("{kind}: {error:#}"),
    );
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
            let mut peers = network.inner.peers.lock().await;
            if super::inbound_error_counts_as_misbehavior(&message) {
                if known_peer.is_some() {
                    peers.record_misbehavior(&peer, message.clone());
                } else {
                    peers.record_inbound_misbehavior(&peer, message.clone());
                }
            } else {
                peers.record_inbound_error(&peer, message.clone());
            }
            if debug_logging_enabled() {
                eprintln!("p2p envelope from {peer} ignored: {message}");
            }
        }
    }
}
