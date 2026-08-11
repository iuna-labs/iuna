use crate::{
    adapters::ui_data_store::BlockMetricRow,
    app::{PeerDirection, PeerInfo},
    domain::Amount,
};

use super::{
    PEER_STALE_AFTER_MS, now_ms,
    types::{
        MempoolCounts, MetricsChart, MetricsPoint, MetricsResponse, MetricsValueKind,
        NetworkHealthLocalState, NetworkHealthResponse,
    },
};

pub(super) fn network_health(
    local: NetworkHealthLocalState,
    peers: &[PeerInfo],
    mempool: MempoolCounts,
) -> NetworkHealthResponse {
    network_health_at(local, peers, mempool, now_ms())
}

pub(super) fn metrics_response(enabled: bool, rows: Vec<BlockMetricRow>) -> MetricsResponse {
    let latest = rows.last().cloned();
    MetricsResponse {
        enabled,
        latest,
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
    let remote_best_height = peers.iter().filter_map(|peer| peer.last_known_height).max();
    let best_known_height = remote_best_height.unwrap_or(local_height).max(local_height);
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

    let state = if peers.is_empty() {
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
        best_known_height,
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
        pending_blinded_transactions: mempool.blinded_transactions,
        pending_blinded_reveals: mempool.blinded_reveals,
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
