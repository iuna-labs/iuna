use std::collections::BTreeMap;

use crate::{
    adapters::ui_data_store::BlockMetricRow,
    app::{PeerDirection, PeerInfo},
    domain::{Amount, Block, Transaction},
};

use super::{
    PEER_STALE_AFTER_MS, now_ms,
    types::{
        MempoolCounts, MetricsChart, MetricsLeaderboards, MetricsPoint, MetricsResponse,
        MetricsValueKind, MineProofLeaderboardEntry, NetworkHealthLocalState,
        NetworkHealthResponse,
    },
    ui::proof_bits,
};

pub(super) fn network_health(
    local: NetworkHealthLocalState,
    peers: &[PeerInfo],
    mempool: MempoolCounts,
) -> NetworkHealthResponse {
    network_health_at(local, peers, mempool, now_ms())
}

pub(super) fn metrics_response(
    enabled: bool,
    preparing: bool,
    rows: Vec<BlockMetricRow>,
    leaderboards: MetricsLeaderboards,
    chain_storage_bytes: &BTreeMap<String, u64>,
    top_mine_proofs: Vec<MineProofLeaderboardEntry>,
) -> MetricsResponse {
    let latest = rows.last().cloned();
    MetricsResponse {
        enabled,
        preparing,
        latest,
        leaderboards,
        top_mine_proofs,
        charts: vec![
            metrics_chart(
                "block-time",
                "Time per block",
                "s",
                MetricsValueKind::Seconds,
                &rows,
                |row| {
                    row.block_time_ms
                        .filter(|_| row.height > 1)
                        .map(|ms| ms as f64 / 1_000.0)
                },
            ),
            metrics_chart(
                "difficulty",
                "Difficulty",
                "bits",
                MetricsValueKind::Number,
                &rows,
                |row| Some(row.mine_difficulty_bits as f64),
            ),
            metrics_chart(
                "supply",
                "IUNA in circulation",
                "IUNA",
                MetricsValueKind::Iuna,
                &rows,
                |row| Some(micro_iuna_as_iuna(row.circulating_supply)),
            ),
            metrics_chart(
                "chain-storage-bytes",
                "Total chain size",
                "bytes",
                MetricsValueKind::Bytes,
                &rows,
                |row| {
                    chain_storage_bytes
                        .get(&row.block_hash)
                        .map(|bytes| *bytes as f64)
                },
            ),
            metrics_chart(
                "known-wallet-addresses",
                "Known wallet addresses",
                "addresses",
                MetricsValueKind::Number,
                &rows,
                |row| Some(row.known_wallet_addresses as f64),
            ),
            metrics_chart(
                "transactions",
                "Transactions",
                "tx",
                MetricsValueKind::Number,
                &rows,
                |row| Some(row.transaction_count as f64),
            ),
            metrics_chart(
                "burn-count",
                "Burn transactions",
                "burns",
                MetricsValueKind::Number,
                &rows,
                |row| Some(row.burn_count as f64),
            ),
            metrics_chart(
                "burn-amount",
                "Burn amount",
                "IUNA",
                MetricsValueKind::Iuna,
                &rows,
                |row| Some(micro_iuna_as_iuna(row.burned_amount)),
            ),
            metrics_chart(
                "total-burn",
                "Total burn",
                "IUNA",
                MetricsValueKind::Iuna,
                &rows,
                |row| Some(micro_iuna_as_iuna(row.total_burned_amount)),
            ),
            metrics_chart(
                "fees",
                "Fees",
                "IUNA",
                MetricsValueKind::Iuna,
                &rows,
                |row| Some(micro_iuna_as_iuna(row.fees_amount)),
            ),
            metrics_chart(
                "mine-actions",
                "Mine actions",
                "mine",
                MetricsValueKind::Number,
                &rows,
                |row| Some(row.mine_count as f64),
            ),
            metrics_chart(
                "vdf-rounds",
                "VDF rounds",
                "rounds",
                MetricsValueKind::Number,
                &rows,
                |row| (row.vdf_rounds > 0).then_some(row.vdf_rounds as f64),
            ),
        ],
    }
}

pub(super) fn top_mine_proofs(blocks: &[Block], limit: usize) -> Vec<MineProofLeaderboardEntry> {
    let mut proofs = blocks
        .iter()
        .flat_map(|block| {
            block.transactions.iter().filter_map(|transaction| {
                let Transaction::Mine {
                    recipient,
                    difficulty_bits,
                    signature,
                    ..
                } = transaction
                else {
                    return None;
                };
                Some(MineProofLeaderboardEntry {
                    height: block.height,
                    address: recipient.clone(),
                    proof_bits: proof_bits(signature),
                    difficulty_bits: *difficulty_bits,
                    proof_hash: signature.clone(),
                })
            })
        })
        .collect::<Vec<_>>();
    proofs.sort_by(|left, right| {
        right
            .proof_bits
            .cmp(&left.proof_bits)
            .then_with(|| left.height.cmp(&right.height))
            .then_with(|| left.proof_hash.cmp(&right.proof_hash))
    });
    proofs.truncate(limit);
    proofs
}

fn metrics_chart(
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    value_kind: MetricsValueKind,
    rows: &[BlockMetricRow],
    value: impl Fn(&BlockMetricRow) -> Option<f64>,
) -> MetricsChart {
    MetricsChart {
        id,
        title,
        unit,
        value_kind,
        points: rows
            .iter()
            .filter_map(|row| {
                value(row).map(|value| MetricsPoint {
                    height: row.height,
                    value,
                })
            })
            .collect(),
    }
}

fn micro_iuna_as_iuna(amount: Amount) -> f64 {
    amount as f64 / 1_000_000.0
}

pub(super) fn network_health_at(
    local: NetworkHealthLocalState,
    peers: &[PeerInfo],
    mempool: MempoolCounts,
    now_ms: u64,
) -> NetworkHealthResponse {
    let local_height = local.height;
    let last_block_age_ms = local
        .tip_timestamp_ms
        .map(|tip_timestamp_ms| now_ms.saturating_sub(tip_timestamp_ms));
    let remote_best_height = peers.iter().filter_map(|peer| peer.last_known_height).max();
    let best_known_height = remote_best_height
        .unwrap_or(local_height)
        .max(local.sync_target_height.unwrap_or(local_height))
        .max(local_height);
    let healthy_heights = peers
        .iter()
        .filter(|peer| peer.last_error.is_none())
        .filter_map(|peer| peer.last_known_height)
        .collect::<Vec<_>>();
    let shared_height = healthy_heights
        .iter()
        .copied()
        .min()
        .unwrap_or(local_height)
        .min(local_height);
    let outbound_peers = peers
        .iter()
        .filter(|peer| peer.direction != PeerDirection::Inbound)
        .count();
    let inbound_peers = peers
        .iter()
        .filter(|peer| peer.direction == PeerDirection::Inbound)
        .count();
    let healthy_peers = peers
        .iter()
        .filter(|peer| peer.last_error.is_none() && peer.last_known_height.is_some())
        .count();
    let failed_peers = peers
        .iter()
        .filter(|peer| peer.last_error.is_some())
        .count();
    let stale_peers = peers
        .iter()
        .filter(|peer| {
            peer.last_success_ms.is_some_and(|last_success| {
                now_ms.saturating_sub(last_success) > PEER_STALE_AFTER_MS
            })
        })
        .count();
    let banned_peers = peers
        .iter()
        .filter(|peer| peer.is_banned_at(now_ms))
        .count();
    let network_time_offset_ms = median_peer_clock_offset(peers, now_ms);
    let bad_clock_peers = peers
        .iter()
        .filter(|peer| {
            peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                now_ms.saturating_sub(observed_ms) <= PEER_STALE_AFTER_MS
            })
        })
        .filter(|peer| peer.last_clock_offset_accepted == Some(false))
        .count();
    let lag_blocks = best_known_height.saturating_sub(local_height);
    let last_error = peers.iter().rev().find_map(|peer| {
        peer.last_error
            .as_ref()
            .map(|error| format!("{}: {error}", peer.address))
    });
    let rejected_chain_payloads = local
        .rejected_blocks
        .saturating_add(local.rejected_block_batches)
        .saturating_add(local.rejected_snapshots);
    let actively_syncing = local
        .sync_target_height
        .is_some_and(|target_height| target_height > local_height);

    let state = if actively_syncing {
        "syncing"
    } else if peers.is_empty() {
        "isolated"
    } else if banned_peers > 0 && healthy_peers == 0 {
        "banned"
    } else if lag_blocks > 0 {
        "syncing"
    } else if failed_peers > 0 && healthy_peers == 0 {
        "peer errors"
    } else if stale_peers > 0 && healthy_peers == stale_peers {
        "stale"
    } else if remote_best_height.is_some_and(|height| local_height > height) {
        "ahead of peers"
    } else {
        "healthy"
    }
    .to_string();

    NetworkHealthResponse {
        ok: !peers.is_empty() && lag_blocks == 0 && healthy_peers > stale_peers,
        state,
        local_height,
        local_tip_hash: local.tip_hash,
        finalized_height: local.finalized_height,
        finalized_hash: local.finalized_hash,
        last_block_age_ms,
        best_known_height,
        sync_start_height: local.sync_start_height,
        sync_validated_height: local.sync_validated_height,
        sync_target_height: local.sync_target_height,
        shared_height,
        lag_blocks,
        outbound_peers,
        inbound_peers,
        healthy_peers,
        failed_peers,
        stale_peers,
        banned_peers,
        pending_transactions: local.pending_transactions,
        pending_plain_transactions: mempool.plain_transactions,
        pending_v2_transactions: mempool.v2_transactions,
        last_finalizer_mode: local.last_finalizer_mode,
        last_finalizer_rank: local.last_finalizer_rank,
        last_block_finalizer: local.last_block_finalizer,
        current_leader: local.current_leader,
        wallet_is_current_leader: local.wallet_is_current_leader,
        last_auto_finalization_status: local.last_auto_finalization_status,
        vdf_rounds: local.vdf_rounds,
        vdf_target_block_ms: local.vdf_target_block_ms,
        rejected_blocks: local.rejected_blocks,
        rejected_block_batches: local.rejected_block_batches,
        rejected_snapshots: local.rejected_snapshots,
        rejected_chain_payloads,
        last_chain_payload_error: local.last_chain_payload_error,
        network_time_offset_ms,
        bad_clock_peers,
        last_error,
    }
}

fn median_peer_clock_offset(peers: &[PeerInfo], now_ms: u64) -> Option<i64> {
    let mut offsets = peers
        .iter()
        .filter(|peer| peer.last_error.is_none())
        .filter(|peer| !peer.is_banned_at(now_ms))
        .filter(|peer| peer.last_clock_offset_accepted == Some(true))
        .filter(|peer| {
            peer.last_clock_observed_ms.is_some_and(|observed_ms| {
                now_ms.saturating_sub(observed_ms) <= PEER_STALE_AFTER_MS
            })
        })
        .filter_map(|peer| peer.last_clock_offset_ms)
        .collect::<Vec<_>>();
    if offsets.is_empty() {
        return None;
    }
    offsets.sort_unstable();
    Some(offsets[offsets.len() / 2])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        adapters::ui_data_store::BlockMetricRow,
        domain::{Block, BurnBundleSection, FinalizerMode, Transaction},
    };

    use super::{
        MempoolCounts, MetricsLeaderboards, NetworkHealthLocalState, metrics_response,
        network_health_at, top_mine_proofs,
    };

    #[test]
    fn stale_metrics_remain_visible_while_the_latest_tip_is_preparing() {
        let row = BlockMetricRow {
            height: 7,
            block_hash: "cached-tip".to_string(),
            timestamp_ms: 1_000,
            block_time_ms: Some(100),
            mine_difficulty_bits: 10,
            circulating_supply: 100,
            known_wallet_addresses: 2,
            transaction_count: 1,
            transfer_count: 0,
            burn_count: 1,
            mine_count: 0,
            burned_amount: 1,
            total_burned_amount: 3,
            fees_amount: 1,
            reward_amount: 1,
            vdf_rounds: 10,
            finalizer_rank: 0,
        };

        let chain_storage_bytes = BTreeMap::from([("cached-tip".to_string(), 12_345)]);
        let response = metrics_response(
            true,
            true,
            vec![row.clone()],
            MetricsLeaderboards::default(),
            &chain_storage_bytes,
            Vec::new(),
        );

        assert!(response.preparing);
        assert_eq!(response.latest, Some(row));
        assert!(response.charts.iter().any(|chart| !chart.points.is_empty()));
        assert_eq!(
            response
                .charts
                .iter()
                .find(|chart| chart.id == "chain-storage-bytes")
                .and_then(|chart| chart.points.first())
                .map(|point| point.value),
            Some(12_345.0)
        );
    }

    #[test]
    fn mine_proofs_are_ranked_by_achieved_bits() {
        let mine = |recipient: &str, signature: &str| Transaction::Mine {
            recipient: recipient.to_string(),
            anchor: "anchor".to_string(),
            salt: 0,
            nonce: 0,
            difficulty_bits: 4,
            proof_header: None,
            signature: signature.to_string(),
        };
        let block = |height, transactions| Block {
            height,
            prev_hash: "0".repeat(64),
            timestamp_ms: 0,
            miner: "finalizer".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: 1,
            vdf_output: String::new(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions,
            transactions_v2: Vec::new(),
            hash: format!("block-{height}"),
        };

        let proofs = top_mine_proofs(
            &[
                block(
                    7,
                    vec![mine("four-bits", "0fff"), mine("late-twelve-bits", "000f")],
                ),
                block(
                    3,
                    vec![
                        mine("early-twelve-bits", "000a"),
                        mine("eight-bits", "00ff"),
                    ],
                ),
            ],
            3,
        );

        assert_eq!(proofs.len(), 3);
        assert_eq!(proofs[0].address, "early-twelve-bits");
        assert_eq!(proofs[0].proof_bits, 12);
        assert_eq!(proofs[0].height, 3);
        assert_eq!(proofs[1].address, "late-twelve-bits");
        assert_eq!(proofs[1].proof_bits, 12);
        assert_eq!(proofs[1].height, 7);
        assert_eq!(proofs[2].address, "eight-bits");
        assert_eq!(proofs[2].proof_bits, 8);
    }

    #[test]
    fn network_health_exposes_operator_chain_and_rejection_context() {
        let local = NetworkHealthLocalState {
            height: 42,
            tip_hash: "tip-hash".to_string(),
            finalized_height: Some(40),
            finalized_hash: Some("finalized-hash".to_string()),
            tip_timestamp_ms: Some(1_000),
            sync_start_height: Some(42),
            sync_validated_height: Some(47),
            sync_target_height: Some(60),
            pending_transactions: 3,
            last_finalizer_mode: Some("ticket".to_string()),
            last_finalizer_rank: Some(1),
            last_block_finalizer: Some("last-finalizer".to_string()),
            current_leader: Some("next-leader".to_string()),
            wallet_is_current_leader: true,
            last_auto_finalization_status: Some("waiting for VDF".to_string()),
            vdf_rounds: 67_000_000,
            vdf_target_block_ms: 120_000,
            rejected_blocks: 2,
            rejected_block_batches: 3,
            rejected_snapshots: 5,
            last_chain_payload_error: Some("snapshot: invalid block".to_string()),
        };

        let health = network_health_at(
            local,
            &[],
            MempoolCounts {
                plain_transactions: 3,
                v2_transactions: 0,
            },
            2_500,
        );

        assert_eq!(health.local_height, 42);
        assert_eq!(health.best_known_height, 60);
        assert_eq!(health.sync_start_height, Some(42));
        assert_eq!(health.sync_validated_height, Some(47));
        assert_eq!(health.sync_target_height, Some(60));
        assert_eq!(health.state, "syncing");
        assert_eq!(health.local_tip_hash, "tip-hash");
        assert_eq!(health.finalized_height, Some(40));
        assert_eq!(health.finalized_hash.as_deref(), Some("finalized-hash"));
        assert_eq!(health.last_block_age_ms, Some(1_500));
        assert_eq!(health.last_finalizer_mode.as_deref(), Some("ticket"));
        assert_eq!(health.last_finalizer_rank, Some(1));
        assert_eq!(
            health.last_block_finalizer.as_deref(),
            Some("last-finalizer")
        );
        assert_eq!(health.current_leader.as_deref(), Some("next-leader"));
        assert!(health.wallet_is_current_leader);
        assert_eq!(
            health.last_auto_finalization_status.as_deref(),
            Some("waiting for VDF")
        );
        assert_eq!(health.vdf_rounds, 67_000_000);
        assert_eq!(health.vdf_target_block_ms, 120_000);
        assert_eq!(health.rejected_blocks, 2);
        assert_eq!(health.rejected_block_batches, 3);
        assert_eq!(health.rejected_snapshots, 5);
        assert_eq!(health.rejected_chain_payloads, 10);
        assert_eq!(
            health.last_chain_payload_error.as_deref(),
            Some("snapshot: invalid block")
        );
    }
}
