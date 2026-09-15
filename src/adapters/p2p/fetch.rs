use std::{collections::BTreeMap, net::SocketAddr};

use anyhow::{Context, Result};
use tokio::{
    net::{TcpStream, tcp::OwnedReadHalf},
    time::timeout,
};

use crate::{
    app::{
        ChainBootstrap, GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION, ProtocolHello, now_ms,
        protocol_capabilities, validate_network_genesis, validate_protocol_capabilities,
        validate_transaction_v2_peer_capability,
    },
    domain::{Block, ChainSnapshot, LaunchProfile, Ledger, verify_vdf},
};

use super::{
    GossipNetwork, JOIN_RESPONSE_TIMEOUT, LimitedLineReader, MAX_BLOCK_BATCH,
    MAX_JOIN_RESPONSE_ENVELOPES, PeerStatus, parse_envelope, write_envelope,
};

pub async fn fetch_snapshot(peer: &str) -> Result<ChainSnapshot> {
    fetch_snapshot_with_announcement(peer, None, &LaunchProfile::default().profile_id).await
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
        }
        GossipEnvelope::PeerStatus { .. } => {}
        other => anyhow::bail!("join peer {peer} sent {other:?} instead of peer status"),
    }

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
    while snapshot.blocks.last().map_or(0, |block| block.height) < bootstrap.height {
        let from_height = snapshot.blocks.last().map_or(0, |block| block.height) + 1;
        let remaining = bootstrap.height - from_height + 1;
        write_envelope(
            &mut writer,
            &GossipEnvelope::BlockRangeRequest {
                from_height,
                limit: remaining.min(MAX_BLOCK_BATCH as u64) as usize,
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
    validate_network_genesis(expected_profile_id, &bootstrap.genesis_block.hash)
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
        let target_height = blocks.last().map(|block| block.height);
        if blocks[0].prev_hash != ledger.tip_hash() {
            let mut candidate = ledger.snapshot();
            let ancestor = candidate
                .blocks
                .iter()
                .position(|block| block.hash == blocks[0].prev_hash)
                .ok_or(super::SyncError::BlockPageHasNoCommonAncestor)?;
            #[cfg(feature = "e2e")]
            for block in &blocks {
                if !verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output) {
                    anyhow::bail!("block VDF output is invalid");
                }
            }
            candidate.blocks.truncate(ancestor + 1);
            candidate.blocks.extend(blocks);
            #[cfg(feature = "e2e")]
            ledger.extend_from_preverified_snapshot_for_e2e(candidate)?;
            #[cfg(not(feature = "e2e"))]
            ledger.extend_from_snapshot_at(candidate, now_ms)?;
            if let Some(target_height) = target_height {
                on_progress(target_height);
            }
            return Ok(ledger);
        }
        for block in blocks {
            let height = block.height;
            ledger.apply_block_at(block, now_ms)?;
            on_progress(height);
        }
        Ok(ledger)
    })
    .await
    .context("block batch extension worker failed")?
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
    use super::join_client_hello;
    use crate::app::{GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION};

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
}
