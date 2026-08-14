use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    net::tcp::OwnedReadHalf,
};

use crate::app::{GossipEnvelope, TRANSACTION_BATCH_LIMIT};

use super::{
    GossipNetwork, MAX_BLOCK_BATCH, MAX_GOSSIP_LINE_BYTES, MAX_INVENTORY_ITEMS,
    MAX_OBJECT_REQUESTS, MAX_PEER_LIST, MAX_SNAPSHOT_BLOCKS, metrics::P2pMetricsCounters,
};

pub(super) struct LimitedLineReader<R> {
    reader: BufReader<R>,
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LimitedLineReader<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            pending: Vec::new(),
        }
    }

    pub(super) async fn read_line(&mut self) -> Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("peer closed before completing a gossip message");
            }

            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if self.pending.len() + newline > MAX_GOSSIP_LINE_BYTES {
                    anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
                }
                self.pending.extend_from_slice(&available[..newline]);
                self.reader.consume(newline + 1);
                if self.pending.ends_with(b"\r") {
                    self.pending.pop();
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes)
                    .context("p2p message is not valid UTF-8")
                    .map(Some);
            }

            if self.pending.len() + available.len() > MAX_GOSSIP_LINE_BYTES {
                anyhow::bail!("p2p message exceeds {} byte limit", MAX_GOSSIP_LINE_BYTES);
            }
            let consumed = available.len();
            self.pending.extend_from_slice(available);
            self.reader.consume(consumed);
        }
    }
}

pub(super) async fn read_session_envelope(
    network: &GossipNetwork,
    connection_label: &str,
    reader: &mut LimitedLineReader<OwnedReadHalf>,
) -> Result<Option<GossipEnvelope>> {
    let Some(line) = reader.read_line().await? else {
        return Ok(None);
    };
    P2pMetricsCounters::add(&network.inner.metrics.bytes_received, line.len() as u64 + 1);
    if line.trim().is_empty() {
        P2pMetricsCounters::inc(&network.inner.metrics.empty_frames);
        P2pMetricsCounters::set_last(
            &network.inner.metrics.last_empty_frame_remote,
            connection_label.to_string(),
        );
        anyhow::bail!("empty p2p envelope");
    }

    match parse_envelope(&line) {
        Ok(envelope) => {
            P2pMetricsCounters::inc(&network.inner.metrics.envelopes_received);
            record_received_envelope_kind(&network.inner.metrics, &envelope);
            Ok(Some(envelope))
        }
        Err(error) => {
            P2pMetricsCounters::inc(&network.inner.metrics.parse_errors);
            P2pMetricsCounters::set_last(
                &network.inner.metrics.last_parse_error,
                format!("{connection_label}: {error:#}"),
            );
            Err(error)
        }
    }
}

pub(super) fn record_received_envelope_kind(
    metrics: &P2pMetricsCounters,
    envelope: &GossipEnvelope,
) {
    match envelope {
        GossipEnvelope::Hello(_) => {
            P2pMetricsCounters::inc(&metrics.hello_envelopes_received);
        }
        GossipEnvelope::PeerStatus { .. } => {
            P2pMetricsCounters::inc(&metrics.peer_status_envelopes_received);
        }
        GossipEnvelope::Inventory { .. } => {
            P2pMetricsCounters::inc(&metrics.inventory_envelopes_received);
        }
        GossipEnvelope::BlindedTransaction(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_transaction_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_transactions_received);
        }
        GossipEnvelope::BlindedTransactions { transactions } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_transaction_envelopes_received);
            P2pMetricsCounters::add(
                &metrics.blinded_transactions_received,
                transactions.len() as u64,
            );
        }
        GossipEnvelope::MineAction(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
        }
        GossipEnvelope::MineActions { .. } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
        }
        GossipEnvelope::BlindedReveal(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_reveal_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_reveals_received);
        }
        GossipEnvelope::BlindedReveals { reveals } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_reveal_envelopes_received);
            P2pMetricsCounters::add(&metrics.blinded_reveals_received, reveals.len() as u64);
        }
        GossipEnvelope::RevealBundle(bundle) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_reveal_envelopes_received);
            P2pMetricsCounters::add(
                &metrics.blinded_reveals_received,
                bundle.reveals.len() as u64,
            );
        }
        GossipEnvelope::RevealBundles { bundles } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.blinded_reveal_envelopes_received);
            P2pMetricsCounters::add(
                &metrics.blinded_reveals_received,
                bundles
                    .iter()
                    .map(|bundle| bundle.reveals.len() as u64)
                    .sum::<u64>(),
            );
        }
        GossipEnvelope::Block(_)
        | GossipEnvelope::Blocks { .. }
        | GossipEnvelope::ChainSnapshot(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
        }
        GossipEnvelope::ChainSnapshotRequest
        | GossipEnvelope::BlockRangeRequest { .. }
        | GossipEnvelope::BlockRequest { .. }
        | GossipEnvelope::PeerAnnouncement { .. }
        | GossipEnvelope::PeerVerificationChallenge { .. }
        | GossipEnvelope::PeerVerificationResponse { .. }
        | GossipEnvelope::PeerList { .. } => {
            P2pMetricsCounters::inc(&metrics.control_envelopes_received);
        }
    }
}

pub(super) fn parse_envelope(line: &str) -> Result<GossipEnvelope> {
    if line.trim().is_empty() {
        anyhow::bail!("empty p2p envelope");
    }
    let envelope = serde_json::from_str(line).context("invalid p2p envelope JSON")?;
    validate_envelope_limits(&envelope)?;
    Ok(envelope)
}

pub(super) fn validate_envelope_limits(envelope: &GossipEnvelope) -> Result<()> {
    match envelope {
        GossipEnvelope::BlockRangeRequest { limit, .. } => {
            ensure_len("block range request", *limit, MAX_BLOCK_BATCH)?;
        }
        GossipEnvelope::BlockRequest { hashes } => {
            ensure_len("block request", hashes.len(), MAX_OBJECT_REQUESTS)?;
        }
        GossipEnvelope::Inventory { blocks } => {
            ensure_len("block inventory", blocks.len(), MAX_INVENTORY_ITEMS)?;
        }
        GossipEnvelope::BlindedTransactions { transactions } => {
            ensure_len(
                "blinded transaction batch",
                transactions.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
        }
        GossipEnvelope::MineActions { transactions } => {
            ensure_len(
                "mine action batch",
                transactions.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
        }
        GossipEnvelope::BlindedReveals { reveals } => {
            ensure_len(
                "blinded reveal batch",
                reveals.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
        }
        GossipEnvelope::RevealBundles { bundles } => {
            ensure_len(
                "reveal bundle batch",
                bundles.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
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
        | GossipEnvelope::ChainSnapshotRequest
        | GossipEnvelope::PeerStatus { .. }
        | GossipEnvelope::BlindedTransaction(_)
        | GossipEnvelope::MineAction(_)
        | GossipEnvelope::BlindedReveal(_)
        | GossipEnvelope::RevealBundle(_)
        | GossipEnvelope::Block(_)
        | GossipEnvelope::PeerAnnouncement { .. }
        | GossipEnvelope::PeerVerificationChallenge { .. }
        | GossipEnvelope::PeerVerificationResponse { .. } => {}
    }
    Ok(())
}

fn ensure_len(label: &str, len: usize, max: usize) -> Result<()> {
    if len > max {
        anyhow::bail!("{label} has {len} items, exceeding limit {max}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use crate::{
        adapters::p2p::{
            MAX_GOSSIP_LINE_BYTES, MAX_INVENTORY_ITEMS, MAX_OBJECT_REQUESTS,
            metrics::P2pMetricsCounters,
        },
        app::{BlockInventory, GossipEnvelope},
        domain::{BlindedReveal, BlindedTransaction},
    };

    use super::{
        LimitedLineReader, parse_envelope, record_received_envelope_kind, validate_envelope_limits,
    };

    #[test]
    fn oversized_inventory_is_rejected_before_processing() {
        let envelope = GossipEnvelope::Inventory {
            blocks: vec![
                BlockInventory {
                    height: 1,
                    hash: "hash".to_string()
                };
                MAX_INVENTORY_ITEMS + 1
            ],
        };

        let error = validate_envelope_limits(&envelope).unwrap_err();

        assert!(error.to_string().contains("block inventory"));
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
    fn parser_rejects_empty_envelope_without_json_eof() {
        let error = parse_envelope("").unwrap_err();

        assert!(error.to_string().contains("empty p2p envelope"));
        assert!(!format!("{error:#}").contains("EOF while parsing"));
    }

    #[test]
    fn parser_accepts_legacy_peer_status_without_mempool_fields() {
        let envelope =
            parse_envelope(r#"{"type":"peer_status","height":7,"tip_hash":"tip"}"#).unwrap();

        assert_eq!(
            envelope,
            GossipEnvelope::PeerStatus {
                height: 7,
                tip_hash: "tip".to_string(),
                time_ms: 0,
            }
        );
    }

    #[test]
    fn received_envelope_metrics_are_categorized() {
        let metrics = P2pMetricsCounters::default();
        let blinded_tx = BlindedTransaction {
            commitment: "commitment".to_string(),
            inputs: Vec::new(),
            fee: 3,
            encrypted_size: 128,
            expires_at_height: 20,
            nonce: "nonce".to_string(),
            ciphertext: "ciphertext".to_string(),
            payload_hash: "payload-hash".to_string(),
        };
        let blinded_reveal = BlindedReveal {
            commitment: "commitment".to_string(),
            key: "key".to_string(),
        };

        record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::PeerStatus {
                height: 7,
                tip_hash: "tip".to_string(),
                time_ms: 1_000,
            },
        );
        record_received_envelope_kind(&metrics, &GossipEnvelope::Inventory { blocks: Vec::new() });
        record_received_envelope_kind(&metrics, &GossipEnvelope::Blocks { blocks: Vec::new() });
        record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::BlindedTransactions {
                transactions: vec![blinded_tx.clone(), blinded_tx],
            },
        );
        record_received_envelope_kind(&metrics, &GossipEnvelope::BlindedReveal(blinded_reveal));
        record_received_envelope_kind(&metrics, &GossipEnvelope::ChainSnapshotRequest);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.peer_status_envelopes_received, 1);
        assert_eq!(snapshot.inventory_envelopes_received, 1);
        assert_eq!(snapshot.data_envelopes_received, 3);
        assert_eq!(snapshot.blinded_transaction_envelopes_received, 1);
        assert_eq!(snapshot.blinded_transactions_received, 2);
        assert_eq!(snapshot.blinded_reveal_envelopes_received, 1);
        assert_eq!(snapshot.blinded_reveals_received, 1);
        assert_eq!(snapshot.control_envelopes_received, 1);
    }

    #[tokio::test]
    async fn limited_line_reader_keeps_partial_line_after_cancelled_read() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut reader = LimitedLineReader::new(reader);
        let line = serde_json::to_string(&GossipEnvelope::PeerStatus {
            height: 7,
            tip_hash: "tip".to_string(),
            time_ms: 1_000,
        })
        .unwrap();
        let split_at = line.len() / 2;

        writer
            .write_all(&line.as_bytes()[..split_at])
            .await
            .unwrap();
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_millis(25), reader.read_line()).await;

        assert!(cancelled.is_err());

        writer
            .write_all(&line.as_bytes()[split_at..])
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();

        assert_eq!(
            reader.read_line().await.unwrap().as_deref(),
            Some(line.as_str())
        );
    }

    #[tokio::test]
    async fn limited_line_reader_rejects_oversized_partial_frame_without_newline() {
        let bytes = vec![b'a'; MAX_GOSSIP_LINE_BYTES + 1];
        let mut reader = LimitedLineReader::new(bytes.as_slice());

        let error = reader.read_line().await.unwrap_err();

        assert!(error.to_string().contains("p2p message exceeds"));
    }
}
