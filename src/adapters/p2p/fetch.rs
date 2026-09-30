use std::{collections::BTreeMap, net::SocketAddr, time::Instant};

use anyhow::{Context, Result};
use tokio::{
    net::{TcpStream, tcp::OwnedReadHalf},
    time::timeout,
};

use crate::{
    app::{
        ChainBootstrap, GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION, ProtocolHello,
        debug_logging_enabled, now_ms, protocol_capabilities, validate_network_genesis,
        validate_protocol_capabilities, validate_transaction_v2_peer_capability,
    },
    domain::{Block, CHAIN_SEGMENT_BLOCKS, ChainSnapshot, LaunchProfile, Ledger, verify_vdf},
};

use super::{
    GossipNetwork, JOIN_RESPONSE_TIMEOUT, LimitedLineReader, MAX_JOIN_RESPONSE_ENVELOPES,
    PeerStatus, negotiated_block_batch_limit, parse_envelope, write_envelope,
};

pub async fn fetch_snapshot(peer: &str) -> Result<ChainSnapshot> {
    fetch_snapshot_with_announcement(peer, None, &LaunchProfile::default().profile_id).await
}

pub async fn fetch_peer_height(peer: &str) -> Result<u64> {
    fetch_peer_status(peer).await.map(|status| status.height)
}

/// Validates a downloaded snapshot without making VDF verification a serial part of replay.
/// State-dependent consensus rules are checked first, then independent VDF proofs are checked
/// across a bounded number of worker threads before the candidate ledger is returned.
pub async fn validate_chain_snapshot(snapshot: ChainSnapshot) -> Result<Ledger> {
    let validation_time_ms = now_ms();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let ledger = Ledger::from_preverified_snapshot_at(snapshot, validation_time_ms)?;
        let replay_elapsed = started.elapsed();

        let vdf_started = Instant::now();
        verify_block_vdfs_parallel(ledger.chain().iter().skip(1))?;
        if debug_logging_enabled() {
            eprintln!(
                "initial chain validation: state={:.3}s vdf={:.3}s blocks={}",
                replay_elapsed.as_secs_f64(),
                vdf_started.elapsed().as_secs_f64(),
                ledger.chain().len().saturating_sub(1),
            );
        }
        Ok(ledger)
    })
    .await
    .context("chain snapshot validation worker failed")?
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
            validate_protocol_capabilities(&hello.capabilities)?;
            validate_transaction_v2_peer_capability(&hello.capabilities, 0, hello.height)?;
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
        } => Ok(PeerStatus::from_envelope(height, tip_hash, time_ms)),
        other => anyhow::bail!("peer {peer} sent {other:?} instead of peer status"),
    }
}

pub async fn fetch_snapshot_with_announcement(
    peer: &str,
    _advertised_addr: Option<SocketAddr>,
    expected_profile_id: &str,
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
    let peer_capabilities = match parse_envelope(&line)? {
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
            validate_protocol_capabilities(&hello.capabilities)?;
            validate_transaction_v2_peer_capability(&hello.capabilities, 0, hello.height)?;
            hello.capabilities
        }
        GossipEnvelope::PeerStatus { .. } => Vec::new(),
        other => anyhow::bail!("join peer {peer} sent {other:?} instead of peer status"),
    };
    let block_batch_limit = negotiated_block_batch_limit(&peer_capabilities);

    write_envelope(&mut writer, &join_client_hello()).await?;
    write_envelope(&mut writer, &GossipEnvelope::ChainBootstrapRequest).await?;
    let bootstrap = read_join_bootstrap_response(peer, &mut reader).await?;
    validate_bootstrap_genesis(expected_profile_id, &bootstrap)?;

    let mut snapshot = ChainSnapshot {
        genesis_allocations: bootstrap.genesis_allocations,
        vdf_rounds: bootstrap.vdf_rounds,
        launch_profile: bootstrap.launch_profile,
        blocks: vec![bootstrap.genesis_block],
    };
    let mut checked_segments = 0_usize;
    while snapshot.blocks.last().map_or(0, |block| block.height) < bootstrap.height {
        let from_height = snapshot.blocks.last().map_or(0, |block| block.height) + 1;
        let remaining = bootstrap.height - from_height + 1;
        write_envelope(
            &mut writer,
            &GossipEnvelope::BlockRangeRequest {
                from_height,
                limit: remaining.min(block_batch_limit as u64) as usize,
            },
        )
        .await?;
        let blocks = read_join_blocks_response(peer, &mut reader).await?;
        if blocks.is_empty() {
            anyhow::bail!("join peer {peer} returned an empty block page at height {from_height}");
        }
        if blocks[0].height != from_height {
            anyhow::bail!("join peer {peer} returned a non-contiguous block page");
        }
        snapshot.blocks.extend(blocks);
        while let Some(summary) = bootstrap.segment_summaries.get(checked_segments) {
            if summary.end_height >= snapshot.blocks.len() as u64 {
                break;
            }
            let block = &snapshot.blocks[summary.end_height as usize];
            if block.hash != summary.end_block_hash {
                anyhow::bail!(
                    "join peer {peer} segment {} does not match its announced end hash",
                    summary.segment_id
                );
            }
            checked_segments += 1;
        }
    }
    if snapshot.blocks.last().map(|block| &block.hash) != Some(&bootstrap.tip_hash) {
        anyhow::bail!("join peer {peer} changed tips while serving block pages");
    }

    Ok(snapshot)
}

fn join_client_hello() -> GossipEnvelope {
    let setup = Ledger::new(BTreeMap::new(), 1);
    GossipEnvelope::Hello(ProtocolHello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: protocol_capabilities(),
        network_id: NETWORK_ID.to_string(),
        genesis_hash: setup.genesis_hash().to_string(),
        listen_addr: None,
        node_id: None,
        height: 0,
        tip_hash: setup.tip_hash().to_string(),
        time_ms: now_ms(),
    })
}

async fn read_join_bootstrap_response(
    peer: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
) -> Result<ChainBootstrap> {
    for _ in 0..MAX_JOIN_RESPONSE_ENVELOPES {
        let envelope = read_join_envelope(peer, reader, "chain bootstrap").await?;
        match envelope {
            GossipEnvelope::ChainBootstrap(bootstrap) => return Ok(bootstrap),
            envelope if is_join_control_envelope(&envelope) => continue,
            other => anyhow::bail!("join peer {peer} sent {other:?} instead of chain bootstrap"),
        }
    }
    anyhow::bail!("join peer {peer} sent too many control envelopes while joining")
}

async fn read_join_blocks_response(
    peer: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
) -> Result<Vec<Block>> {
    for _ in 0..MAX_JOIN_RESPONSE_ENVELOPES {
        let envelope = read_join_envelope(peer, reader, "block page").await?;
        match envelope {
            GossipEnvelope::Blocks { blocks } => return Ok(blocks),
            envelope if is_join_control_envelope(&envelope) => continue,
            other => anyhow::bail!("join peer {peer} sent {other:?} instead of a block page"),
        }
    }
    anyhow::bail!("join peer {peer} sent too many control envelopes while joining")
}

async fn read_join_envelope(
    peer: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
    expected: &str,
) -> Result<GossipEnvelope> {
    let line = timeout(JOIN_RESPONSE_TIMEOUT, reader.read_line())
        .await
        .with_context(|| format!("join peer {peer} timed out waiting for {expected}"))??
        .with_context(|| format!("join peer {peer} closed before sending {expected}"))?;
    parse_envelope(&line)
}

fn is_join_control_envelope(envelope: &GossipEnvelope) -> bool {
    matches!(
        envelope,
        GossipEnvelope::Hello(_)
            | GossipEnvelope::PeerStatus { .. }
            | GossipEnvelope::PeerList { .. }
            | GossipEnvelope::PeerVerificationChallenge { .. }
            | GossipEnvelope::PeerVerificationResponse { .. }
            | GossipEnvelope::Inventory { .. }
    )
}

pub(super) async fn validate_chain_bootstrap(
    expected_profile_id: &str,
    bootstrap: ChainBootstrap,
    now_ms: u64,
) -> Result<Ledger> {
    validate_bootstrap_genesis(expected_profile_id, &bootstrap)?;
    let snapshot = ChainSnapshot {
        genesis_allocations: bootstrap.genesis_allocations,
        vdf_rounds: bootstrap.vdf_rounds,
        launch_profile: bootstrap.launch_profile,
        blocks: vec![bootstrap.genesis_block],
    };
    tokio::task::spawn_blocking(move || Ledger::from_snapshot_at(snapshot, now_ms))
        .await
        .context("chain bootstrap adoption worker failed")?
}

fn validate_bootstrap_genesis(expected_profile_id: &str, bootstrap: &ChainBootstrap) -> Result<()> {
    if bootstrap.launch_profile.profile_id != expected_profile_id {
        anyhow::bail!(
            "chain bootstrap profile {} does not match expected profile {expected_profile_id}",
            bootstrap.launch_profile.profile_id
        );
    }
    validate_network_genesis(expected_profile_id, &bootstrap.genesis_block.hash)?;
    if !bootstrap.segment_summaries.is_empty() {
        let segment_blocks = CHAIN_SEGMENT_BLOCKS as u64;
        let expected_count = bootstrap.height / segment_blocks
            + u64::from(bootstrap.height % segment_blocks == segment_blocks - 1);
        if bootstrap.segment_summaries.len() as u64 != expected_count {
            anyhow::bail!("chain bootstrap has an incomplete segment summary list");
        }
        for (segment_id, summary) in bootstrap.segment_summaries.iter().enumerate() {
            let expected_end_height = (segment_id as u64 + 1) * segment_blocks - 1;
            if summary.segment_id != segment_id as u64
                || summary.end_height != expected_end_height
                || summary.end_block_hash.len() != 64
                || !summary
                    .end_block_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                anyhow::bail!("chain bootstrap has an invalid segment summary");
            }
        }
    }
    Ok(())
}

pub(super) async fn validate_blocks_extension(
    mut ledger: Ledger,
    blocks: Vec<Block>,
    now_ms: u64,
    on_progress: impl Fn(u64) + Send + 'static,
) -> Result<Ledger> {
    if blocks.is_empty() {
        return Ok(ledger);
    }

    tokio::task::spawn_blocking(move || {
        let state_started = Instant::now();
        let target_height = blocks.last().map(|block| block.height);
        if blocks[0].prev_hash != ledger.tip_hash() {
            let mut candidate = ledger.snapshot();
            let ancestor = candidate
                .blocks
                .iter()
                .position(|block| block.hash == blocks[0].prev_hash)
                .ok_or(super::SyncError::BlockPageHasNoCommonAncestor)?;
            candidate.blocks.truncate(ancestor + 1);
            candidate.blocks.extend(blocks.iter().cloned());
            ledger.extend_from_preverified_snapshot_at(candidate, now_ms)?;
            let state_elapsed = state_started.elapsed();
            let vdf_started = Instant::now();
            verify_block_vdfs_parallel(&blocks)?;
            log_batch_validation_timing(blocks.len(), state_elapsed, vdf_started.elapsed(), true);
            if let Some(target_height) = target_height {
                on_progress(target_height);
            }
            return Ok(ledger);
        }
        for block in blocks.iter().cloned() {
            ledger.apply_preverified_block_at(block, now_ms)?;
        }
        let state_elapsed = state_started.elapsed();
        let vdf_started = Instant::now();
        verify_block_vdfs_parallel(&blocks)?;
        log_batch_validation_timing(blocks.len(), state_elapsed, vdf_started.elapsed(), false);
        for block in &blocks {
            on_progress(block.height);
        }
        Ok(ledger)
    })
    .await
    .context("block batch extension worker failed")?
}

fn verify_block_vdfs_parallel<'a>(blocks: impl IntoIterator<Item = &'a Block>) -> Result<()> {
    let blocks = blocks.into_iter().collect::<Vec<_>>();
    if blocks.is_empty() {
        return Ok(());
    }

    // Two chain candidates may be validated concurrently by the network coordinator. Giving
    // each validation at most half the available CPUs prevents the pair from oversubscribing the
    // machine, while the upper bound keeps untrusted batches from creating excessive threads.
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let workers = available.div_ceil(2).clamp(1, 8).min(blocks.len());
    let chunk_size = blocks.len().div_ceil(workers);
    let invalid_height = std::thread::scope(|scope| -> Result<Option<u64>> {
        let handles = blocks
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk.iter().find_map(|block| {
                        (!verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output))
                            .then_some(block.height)
                    })
                })
            })
            .collect::<Vec<_>>();

        let mut invalid_height = None;
        for handle in handles {
            let height = handle
                .join()
                .map_err(|_| anyhow::anyhow!("VDF verification worker panicked"))?;
            invalid_height = match (invalid_height, height) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (height @ Some(_), None) | (None, height @ Some(_)) => height,
                (None, None) => None,
            };
        }
        Ok(invalid_height)
    })?;
    if invalid_height.is_some() {
        anyhow::bail!("block VDF output is invalid");
    }
    Ok(())
}

fn log_batch_validation_timing(
    blocks: usize,
    state_elapsed: std::time::Duration,
    vdf_elapsed: std::time::Duration,
    fork: bool,
) {
    if debug_logging_enabled() {
        eprintln!(
            "block batch validation: state={:.3}s vdf={:.3}s blocks={blocks} fork={fork}",
            state_elapsed.as_secs_f64(),
            vdf_elapsed.as_secs_f64(),
        );
    }
}

pub(super) async fn network_adjusted_time_ms(network: &GossipNetwork) -> u64 {
    let local_time_ms = now_ms();
    network
        .inner
        .peers
        .lock()
        .await
        .adjusted_time_ms_at(local_time_ms)
}

pub(super) async fn verify_block_vdf(block: Block) -> Result<Block> {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{join_client_hello, validate_bootstrap_genesis, verify_block_vdfs_parallel};
    use crate::app::{
        ChainBootstrap, ChainSegmentSummary, GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION,
    };
    use crate::domain::Ledger;

    #[test]
    fn snapshot_join_identifies_as_an_unannounced_setup_placeholder() {
        let GossipEnvelope::Hello(hello) = join_client_hello() else {
            panic!("join handshake must start with Hello");
        };

        assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
        assert_eq!(hello.network_id, NETWORK_ID);
        assert_eq!(hello.height, 0);
        assert_eq!(hello.genesis_hash, hello.tip_hash);
        assert!(hello.listen_addr.is_none());
        assert!(hello.node_id.is_none());
    }

    #[test]
    fn parallel_vdf_verification_rejects_an_invalid_proof() {
        let genesis = Ledger::new(BTreeMap::new(), 1).chain()[0].clone();
        let mut later = genesis.clone();
        later.height = 9;
        later.vdf_output = "invalid-vdf".to_string();
        let mut earlier = genesis;
        earlier.height = 3;
        earlier.vdf_output = "also-invalid".to_string();

        let error = verify_block_vdfs_parallel([&later, &earlier]).unwrap_err();

        assert_eq!(error.to_string(), "block VDF output is invalid");
    }

    #[test]
    fn bootstrap_rejects_segment_summaries_that_do_not_match_its_height() {
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mut snapshot = ledger.snapshot();
        snapshot.launch_profile.profile_id = "segment-summary-test".to_string();
        let bootstrap = ChainBootstrap {
            genesis_allocations: snapshot.genesis_allocations,
            vdf_rounds: snapshot.vdf_rounds,
            launch_profile: snapshot.launch_profile,
            genesis_block: snapshot.blocks.remove(0),
            height: 0,
            tip_hash: ledger.tip_hash().to_string(),
            segment_summaries: vec![ChainSegmentSummary {
                segment_id: 0,
                end_height: 255,
                end_block_hash: ledger.tip_hash().to_string(),
            }],
        };

        let error = validate_bootstrap_genesis("segment-summary-test", &bootstrap).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("incomplete segment summary list")
        );
    }
}
