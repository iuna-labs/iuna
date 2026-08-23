use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    net::tcp::OwnedReadHalf,
};

use crate::{
    app::{GossipEnvelope, TRANSACTION_BATCH_LIMIT},
    domain::BURN_COMMITTEE_SIZE,
};

use super::{
    GossipNetwork, MAX_BLOCK_BATCH, MAX_BLOCK_LOCATOR_HASHES, MAX_GOSSIP_LINE_BYTES,
    MAX_INVENTORY_ITEMS, MAX_OBJECT_REQUESTS, MAX_PEER_LIST, metrics::P2pMetricsCounters,
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
        GossipEnvelope::Transaction(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.transaction_envelopes_received);
            P2pMetricsCounters::inc(&metrics.transactions_received);
        }
        GossipEnvelope::Transactions { transactions } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.transaction_envelopes_received);
            P2pMetricsCounters::add(&metrics.transactions_received, transactions.len() as u64);
        }
        GossipEnvelope::BurnBundle(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.burn_bundle_envelopes_received);
            P2pMetricsCounters::inc(&metrics.burn_bundles_received);
        }
        GossipEnvelope::BurnBundles { bundles } => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
            P2pMetricsCounters::inc(&metrics.burn_bundle_envelopes_received);
            P2pMetricsCounters::add(&metrics.burn_bundles_received, bundles.len() as u64);
        }
        GossipEnvelope::Block(_)
        | GossipEnvelope::Blocks { .. }
        | GossipEnvelope::ChainBootstrap(_) => {
            P2pMetricsCounters::inc(&metrics.data_envelopes_received);
        }
        GossipEnvelope::ChainBootstrapRequest
        | GossipEnvelope::BlockLocatorRequest { .. }
        | GossipEnvelope::BlockRangeRequest { .. }
        | GossipEnvelope::BlockRequest { .. }
        | GossipEnvelope::BurnBundleRequest { .. }
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
        GossipEnvelope::BlockLocatorRequest { locator, limit } => {
            ensure_len("block locator", locator.len(), MAX_BLOCK_LOCATOR_HASHES)?;
            ensure_len("block locator request", *limit, MAX_BLOCK_BATCH)?;
        }
        GossipEnvelope::BlockRequest { hashes } => {
            ensure_len("block request", hashes.len(), MAX_OBJECT_REQUESTS)?;
        }
        GossipEnvelope::Inventory { blocks } => {
            ensure_len("block inventory", blocks.len(), MAX_INVENTORY_ITEMS)?;
        }
        GossipEnvelope::Transactions { transactions } => {
            ensure_len(
                "transaction batch",
                transactions.len(),
                TRANSACTION_BATCH_LIMIT,
            )?;
        }
        GossipEnvelope::BurnBundles { bundles } => {
            ensure_len("burn bundle batch", bundles.len(), TRANSACTION_BATCH_LIMIT)?;
        }
        GossipEnvelope::BurnBundleRequest { slots, .. } => {
            ensure_len("burn bundle request", slots.len(), BURN_COMMITTEE_SIZE)?;
        }
        GossipEnvelope::Blocks { blocks } => {
            ensure_len("block batch", blocks.len(), MAX_BLOCK_BATCH)?;
        }
        GossipEnvelope::PeerList { peers } => {
            ensure_len("peer list", peers.len(), MAX_PEER_LIST)?;
        }
        GossipEnvelope::Hello(_)
        | GossipEnvelope::ChainBootstrapRequest
        | GossipEnvelope::ChainBootstrap(_)
        | GossipEnvelope::PeerStatus { .. }
        | GossipEnvelope::Transaction(_)
        | GossipEnvelope::BurnBundle(_)
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
        adapters::p2p::metrics::P2pMetricsCounters,
        app::{BlockInventory, GossipEnvelope, TRANSACTION_BATCH_LIMIT},
        domain::{
            BURN_COMMITTEE_SIZE, Block, BurnBundle, BurnBundleSection, FinalizerMode, OutPoint,
            Transaction, TxInput, TxOutput,
        },
    };

    use super::{
        LimitedLineReader, MAX_BLOCK_BATCH, MAX_BLOCK_LOCATOR_HASHES, MAX_GOSSIP_LINE_BYTES,
        MAX_INVENTORY_ITEMS, MAX_OBJECT_REQUESTS, MAX_PEER_LIST, parse_envelope,
        record_received_envelope_kind, validate_envelope_limits,
    };

    fn burn(signature: &str) -> Transaction {
        Transaction::Burn {
            inputs: vec![TxInput {
                outpoint: OutPoint {
                    txid: format!("{signature:0<64}"),
                    index: 0,
                },
                owner: "owner".to_string(),
                signature: signature.to_string(),
            }],
            change: vec![TxOutput {
                address: "owner".to_string(),
                amount: 1,
            }],
            amount: 1,
            fee: 1,
            signature: signature.to_string(),
        }
    }

    fn burn_bundle(slot: u8, signature: &str) -> BurnBundle {
        BurnBundle {
            height: 1,
            prev_hash: "parent".to_string(),
            slot,
            member: format!("member-{slot}"),
            burns: vec![burn(signature)],
            signature: format!("bundle-{signature}"),
        }
    }

    fn dummy_block(height: u64) -> Block {
        Block {
            height,
            prev_hash: "0".repeat(64),
            timestamp_ms: height,
            miner: "0".repeat(64),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: 0,
            vdf_output: "0:0".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: Vec::new(),
            hash: format!("{height:064x}"),
        }
    }

    #[test]
    fn metrics_count_transaction_and_burn_bundle_batches() {
        let metrics = P2pMetricsCounters::default();

        record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::Transactions {
                transactions: vec![burn("a"), burn("b")],
            },
        );
        record_received_envelope_kind(
            &metrics,
            &GossipEnvelope::BurnBundles {
                bundles: vec![burn_bundle(1, "c"), burn_bundle(2, "d")],
            },
        );
        record_received_envelope_kind(&metrics, &GossipEnvelope::Transaction(burn("e")));
        record_received_envelope_kind(&metrics, &GossipEnvelope::BurnBundle(burn_bundle(1, "f")));

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.data_envelopes_received, 4);
        assert_eq!(snapshot.transaction_envelopes_received, 2);
        assert_eq!(snapshot.transactions_received, 3);
        assert_eq!(snapshot.burn_bundle_envelopes_received, 2);
        assert_eq!(snapshot.burn_bundles_received, 3);
    }

    #[test]
    fn parser_accepts_burn_bundle_envelopes() {
        let envelope = GossipEnvelope::BurnBundles {
            bundles: vec![burn_bundle(1, "a")],
        };
        let line = serde_json::to_string(&envelope).unwrap();

        assert_eq!(parse_envelope(&line).unwrap(), envelope);
    }

    #[test]
    fn envelope_item_limits_reject_only_above_the_boundary() {
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockRangeRequest {
                from_height: 1,
                limit: MAX_BLOCK_BATCH
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockRangeRequest {
                from_height: 1,
                limit: MAX_BLOCK_BATCH + 1
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockRequest {
                hashes: vec!["0".repeat(64); MAX_OBJECT_REQUESTS]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockRequest {
                hashes: vec!["0".repeat(64); MAX_OBJECT_REQUESTS + 1]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Inventory {
                blocks: vec![
                    BlockInventory {
                        height: 1,
                        hash: "0".repeat(64)
                    };
                    MAX_INVENTORY_ITEMS
                ]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Inventory {
                blocks: vec![
                    BlockInventory {
                        height: 1,
                        hash: "0".repeat(64)
                    };
                    MAX_INVENTORY_ITEMS + 1
                ]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Transactions {
                transactions: vec![burn("a"); TRANSACTION_BATCH_LIMIT]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Transactions {
                transactions: vec![burn("a"); TRANSACTION_BATCH_LIMIT + 1]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BurnBundles {
                bundles: vec![burn_bundle(1, "a"); TRANSACTION_BATCH_LIMIT]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BurnBundles {
                bundles: vec![burn_bundle(1, "a"); TRANSACTION_BATCH_LIMIT + 1]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BurnBundleRequest {
                height: 1,
                prev_hash: "0".repeat(64),
                slots: vec![1; BURN_COMMITTEE_SIZE]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BurnBundleRequest {
                height: 1,
                prev_hash: "0".repeat(64),
                slots: vec![1; BURN_COMMITTEE_SIZE + 1]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Blocks {
                blocks: vec![dummy_block(1); MAX_BLOCK_BATCH]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::Blocks {
                blocks: vec![dummy_block(1); MAX_BLOCK_BATCH + 1]
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockLocatorRequest {
                locator: vec!["0".repeat(64); MAX_BLOCK_LOCATOR_HASHES],
                limit: MAX_BLOCK_BATCH,
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::BlockLocatorRequest {
                locator: vec!["0".repeat(64); MAX_BLOCK_LOCATOR_HASHES + 1],
                limit: MAX_BLOCK_BATCH,
            })
            .is_err()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::PeerList {
                peers: vec!["127.0.0.1:9444".to_string(); MAX_PEER_LIST]
            })
            .is_ok()
        );
        assert!(
            validate_envelope_limits(&GossipEnvelope::PeerList {
                peers: vec!["127.0.0.1:9444".to_string(); MAX_PEER_LIST + 1]
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn performance_budget_p2p_line_reader_enforces_message_size() {
        let (mut client, server) = tokio::io::duplex(MAX_GOSSIP_LINE_BYTES + 1);
        let mut reader = LimitedLineReader::new(server);
        let line = vec![b'a'; MAX_GOSSIP_LINE_BYTES];
        client.write_all(&line).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        let read = reader.read_line().await.unwrap().unwrap();

        assert_eq!(read.len(), MAX_GOSSIP_LINE_BYTES);

        let (mut client, server) = tokio::io::duplex(MAX_GOSSIP_LINE_BYTES + 2);
        let mut reader = LimitedLineReader::new(server);
        let line = vec![b'a'; MAX_GOSSIP_LINE_BYTES + 1];
        client.write_all(&line).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        let error = reader.read_line().await.unwrap_err();

        assert!(error.to_string().contains("p2p message exceeds"));
    }

    #[test]
    fn performance_budget_p2p_batch_parser_enforces_item_limits() {
        let at_budget = GossipEnvelope::PeerList {
            peers: vec!["127.0.0.1:9444".to_string(); MAX_PEER_LIST],
        };
        let line = serde_json::to_string(&at_budget).unwrap();

        assert_eq!(parse_envelope(&line).unwrap(), at_budget);

        let over_budget = GossipEnvelope::PeerList {
            peers: vec!["127.0.0.1:9444".to_string(); MAX_PEER_LIST + 1],
        };
        let line = serde_json::to_string(&over_budget).unwrap();
        let error = parse_envelope(&line).unwrap_err();

        assert!(error.to_string().contains("peer list has"));
    }
}
