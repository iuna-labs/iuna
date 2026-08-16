use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, tcp::OwnedReadHalf},
    time::timeout,
};

use crate::{
    app::{GossipEnvelope, NETWORK_ID, PROTOCOL_VERSION, now_ms},
    domain::{Block, ChainSnapshot, Ledger, verify_vdf},
};

use super::{
    GossipNetwork, JOIN_RESPONSE_TIMEOUT, LimitedLineReader, MAX_JOIN_RESPONSE_ENVELOPES,
    PeerStatus, parse_envelope,
};

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
        } => Ok(PeerStatus::from_envelope(height, tip_hash, time_ms)),
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

pub(super) fn join_snapshot_response(
    peer: &str,
    envelope: GossipEnvelope,
) -> Result<Option<ChainSnapshot>> {
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

pub(super) async fn validate_snapshot_extension(
    mut ledger: Ledger,
    snapshot: ChainSnapshot,
    now_ms: u64,
) -> Result<Ledger> {
    if ledger.is_setup_placeholder() {
        return tokio::task::spawn_blocking(move || Ledger::from_snapshot_at(snapshot, now_ms))
            .await
            .context("chain snapshot adoption worker failed")?;
    }

    tokio::task::spawn_blocking(move || {
        ledger.extend_from_snapshot_at(snapshot, now_ms)?;
        Ok(ledger)
    })
    .await
    .context("chain snapshot extension worker failed")?
}

pub(super) async fn validate_blocks_extension(
    mut ledger: Ledger,
    blocks: Vec<Block>,
    now_ms: u64,
) -> Result<Ledger> {
    if blocks.is_empty() {
        return Ok(ledger);
    }

    tokio::task::spawn_blocking(move || {
        for block in blocks {
            ledger.apply_block_at(block, now_ms)?;
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
    use crate::{
        app::GossipEnvelope,
        domain::{Block, FinalizerMode, RevealBundleSection, Wallet},
    };

    use super::super::test_support::{allocations, node};
    use super::{join_snapshot_response, validate_blocks_extension};

    #[test]
    fn join_snapshot_response_ignores_status_noise_before_snapshot() {
        assert!(
            join_snapshot_response(
                "127.0.0.1:9544",
                GossipEnvelope::PeerStatus {
                    height: 0,
                    tip_hash: "tip".to_string(),
                    time_ms: 1_000,
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
        let parsed = join_snapshot_response(
            "127.0.0.1:9544",
            GossipEnvelope::ChainSnapshot(snapshot.clone()),
        )
        .unwrap();

        assert_eq!(parsed, Some(snapshot));
    }

    #[tokio::test]
    async fn block_batch_prechecks_before_vdf_verification() {
        let alice = Wallet::from_seed("batch-precheck-alice");
        let test_node = node(
            "alice",
            alice.clone(),
            allocations(std::slice::from_ref(&alice), 1_000),
        );
        let tip = test_node.chain_snapshot().blocks.last().unwrap().clone();
        let ledger = test_node.clone_ledger();
        let invalid_height_block = Block {
            height: ledger.height() + 2,
            prev_hash: tip.hash,
            timestamp_ms: tip.timestamp_ms + 1,
            miner: alice.address().to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: ledger.vdf_rounds(),
            vdf_output: "not-a-vdf-solution".to_string(),
            leader_proof: None,
            blinded_transactions: Vec::new(),
            reveal_bundle_section: RevealBundleSection::default(),
            transactions: Vec::new(),
            hash: "invalid-hash".to_string(),
        };

        let error = validate_blocks_extension(ledger, vec![invalid_height_block], u64::MAX)
            .await
            .unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("expected block height"));
        assert!(!message.contains("VDF output is invalid"));
    }
}
