use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
};

use crate::{
    adapters::p2p::P2pMetrics,
    app::{NodeStatus, PeerInfo},
    domain::{OutPoint, Transaction, TxOutput},
};

use super::types::{LeaderboardEntry, MetricsLeaderboards};
use super::{
    BlocksQuery, ConfigResponse, MempoolCounts, MetricsQuery, MetricsResponse,
    NetworkHealthLocalState, NetworkHealthResponse, Page, PageQuery, UiBlock, UiTransaction,
    WalletTransactionContext, WalletTransactionFilters, WalletTransactionRow,
    WalletTransactionsQuery, WalletUtxoRow,
};
use super::{
    DATASET_LIMIT, DATASET_PAGE_LIMIT, EXPLORER_LIMIT, EXPLORER_PAGE_LIMIT, HttpState,
    add_pending_outputs, metrics_response, network_health, ui_blocks_from_indexes, ui_transaction,
    wallet_transaction_row, wallet_transaction_rows,
};

pub(super) async fn api_status(State(state): State<HttpState>) -> Json<NodeStatus> {
    let mut status = state.node.lock().await.status();
    status.stratum = state.stratum.clone();
    Json(status)
}

pub(super) async fn api_blocks(
    State(state): State<HttpState>,
    Query(query): Query<BlocksQuery>,
) -> Json<Vec<UiBlock>> {
    let limit = query
        .limit
        .unwrap_or(EXPLORER_PAGE_LIMIT)
        .min(EXPLORER_LIMIT);
    let (tip_hash, blocks) = {
        let node = state.node.lock().await;
        let blocks = match query.before_height {
            Some(before_height) => node.blocks_before(before_height, limit),
            None => node.recent_blocks(limit),
        };
        (node.chain_tip_hash(), blocks)
    };
    let store = state.ui_data_store.clone();
    let view = tokio::task::spawn_blocking(move || store.load_ui_chain_index(&tip_hash))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .unwrap_or_default();
    Json(ui_blocks_from_indexes(
        blocks,
        &view.outputs,
        &view.burn_leader_ranks_by_hash,
    ))
}

pub(super) async fn api_config(State(state): State<HttpState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        config: state.ui_config.lock().await.clone(),
        p2p_inbound_runtime_active: state.gossip.accepts_inbound().await,
        p2p_runtime_bind_addr: state.gossip.listen_addr().to_string(),
        stratum_runtime_enabled: state.stratum.enabled,
        stratum_runtime_listen_addr: state.stratum.listen_addr.clone(),
    })
}

pub(super) async fn api_mempool(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<UiTransaction>> {
    let pending = {
        let node = state.node.lock().await;
        node.pending_transactions()
    };
    let mut required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(pending.iter(), &mut required_outputs);
    let mut outputs = load_outputs_for_outpoints(&state, required_outputs)
        .await
        .unwrap_or_default();
    add_pending_outputs(&mut outputs, &pending);
    let mut items = pending
        .iter()
        .map(|tx| ui_transaction(tx, &outputs))
        .collect::<Vec<_>>();
    items.reverse();
    Json(page_items(items, query))
}

pub(super) async fn api_wallet_transactions(
    State(state): State<HttpState>,
    Query(query): Query<WalletTransactionsQuery>,
) -> Json<Page<WalletTransactionRow>> {
    let page_query = query.page();
    let offset = page_query.offset.unwrap_or(0);
    let limit = page_query
        .limit
        .unwrap_or(DATASET_PAGE_LIMIT)
        .clamp(1, DATASET_LIMIT);
    let filters = WalletTransactionFilters::from_query(query);
    let (wallet, pending) = {
        let node = state.node.lock().await;
        (
            node.wallet_address().to_string(),
            node.pending_transactions(),
        )
    };
    let mut pending_required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(pending.iter(), &mut pending_required_outputs);
    let mut pending_outputs = load_outputs_for_outpoints(&state, pending_required_outputs)
        .await
        .unwrap_or_default();
    add_pending_outputs(&mut pending_outputs, &pending);
    let pending_rows =
        wallet_transaction_rows(&wallet, pending.clone(), &[], &pending_outputs, filters);
    let pending_total = pending_rows.len();
    let mut items = pending_rows
        .into_iter()
        .skip(offset.min(pending_total))
        .take(limit)
        .collect::<Vec<_>>();

    let confirmed_offset = offset.saturating_sub(pending_total);
    let remaining_limit = limit.saturating_sub(items.len());
    let kinds = wallet_transaction_filter_kinds(filters);
    let store = state.ui_data_store.clone();
    let wallet_for_query = wallet.clone();
    let (confirmed_rows, confirmed_total) = if remaining_limit == 0 {
        (Vec::new(), 0)
    } else {
        tokio::task::spawn_blocking(move || {
            store.load_wallet_transactions(
                &wallet_for_query,
                &kinds,
                confirmed_offset,
                remaining_limit,
            )
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
    };
    let mut confirmed_required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(
        confirmed_rows.iter().map(|row| &row.transaction),
        &mut confirmed_required_outputs,
    );
    let confirmed_outputs = load_outputs_for_outpoints(&state, confirmed_required_outputs)
        .await
        .unwrap_or_default();
    items.extend(confirmed_rows.into_iter().filter_map(|row| {
        wallet_transaction_row(
            &wallet,
            &row.transaction,
            &confirmed_outputs,
            &WalletTransactionContext {
                status: "confirmed",
                block_height: Some(row.block_height),
                timestamp_ms: Some(row.timestamp_ms),
                block_finalizer: Some(row.block_finalizer),
            },
        )
    }));
    let total = pending_total + confirmed_total;
    let next_offset = offset + items.len();
    Json(Page {
        items,
        offset: offset.min(total),
        limit,
        total,
        has_more: next_offset < total,
        next_offset: (next_offset < total).then_some(next_offset),
    })
}

async fn load_outputs_for_outpoints(
    state: &HttpState,
    outpoints: BTreeSet<OutPoint>,
) -> Result<BTreeMap<OutPoint, TxOutput>> {
    if outpoints.is_empty() {
        return Ok(BTreeMap::new());
    }
    let store = state.ui_data_store.clone();
    tokio::task::spawn_blocking(move || store.load_outputs(&outpoints))
        .await
        .unwrap_or_else(|_| Ok(BTreeMap::new()))
}

async fn current_real_chain_tip(state: &HttpState) -> Option<String> {
    let node = state.node.lock().await;
    node.has_real_chain().then(|| node.chain_tip_hash())
}

fn collect_transaction_input_outpoints<'a>(
    transactions: impl IntoIterator<Item = &'a Transaction>,
    outpoints: &mut BTreeSet<OutPoint>,
) {
    for transaction in transactions {
        match transaction {
            Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => {
                outpoints.extend(inputs.iter().map(|input| input.outpoint.clone()));
            }
            Transaction::Mine { .. } => {}
        }
    }
}

pub(super) async fn api_wallet_utxos(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<WalletUtxoRow>> {
    let (wallet, pending_spent) = {
        let node = state.node.lock().await;
        (
            node.wallet_address().to_string(),
            node.wallet_pending_spent_outpoints(),
        )
    };
    let store = state.ui_data_store.clone();
    let utxos = tokio::task::spawn_blocking(move || store.load_wallet_utxos(&wallet))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Json(page_items(
        wallet_utxo_rows_from_ui_data(utxos, &pending_spent),
        query,
    ))
}

pub(super) async fn api_wallet_selectable_utxos(
    State(state): State<HttpState>,
) -> Json<Vec<WalletUtxoRow>> {
    let (wallet, pending_spent) = {
        let node = state.node.lock().await;
        (
            node.wallet_address().to_string(),
            node.wallet_pending_spent_outpoints(),
        )
    };
    let store = state.ui_data_store.clone();
    let utxos = tokio::task::spawn_blocking(move || store.load_wallet_utxos(&wallet))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Json(
        wallet_utxo_rows_from_ui_data(utxos, &pending_spent)
            .into_iter()
            .filter(|utxo| utxo.spendable)
            .collect(),
    )
}

pub(super) fn page_items<T>(items: Vec<T>, query: PageQuery) -> Page<T> {
    let total = items.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query
        .limit
        .unwrap_or(DATASET_PAGE_LIMIT)
        .clamp(1, DATASET_LIMIT);
    let page_items = items
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next_offset = offset + page_items.len();
    Page {
        items: page_items,
        offset,
        limit,
        total,
        has_more: next_offset < total,
        next_offset: (next_offset < total).then_some(next_offset),
    }
}

fn wallet_transaction_filter_kinds(filters: WalletTransactionFilters) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    if filters.transfer {
        kinds.push("transfer");
    }
    if filters.mine {
        kinds.push("mine");
    }
    if filters.burn {
        kinds.push("burn");
    }
    kinds
}

fn wallet_utxo_rows_from_ui_data(
    utxos: Vec<(crate::domain::OutPoint, crate::domain::TxOutput)>,
    pending_spent: &BTreeSet<crate::domain::OutPoint>,
) -> Vec<WalletUtxoRow> {
    utxos
        .into_iter()
        .map(|(outpoint, output)| {
            let spendable = !pending_spent.contains(&outpoint);
            WalletUtxoRow {
                outpoint,
                address: output.address,
                amount: output.amount,
                spendable,
            }
        })
        .collect()
}

pub(super) async fn api_peers(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<PeerInfo>> {
    Json(page_items(state.peers.lock().await.list(), query))
}

pub(super) async fn api_p2p_metrics(State(state): State<HttpState>) -> Json<P2pMetrics> {
    Json(state.gossip.metrics())
}

pub(super) async fn api_metrics(
    State(state): State<HttpState>,
    Query(query): Query<MetricsQuery>,
) -> Json<MetricsResponse> {
    let enabled = state.ui_config.lock().await.keep_track_of_metrics;
    if !enabled {
        return Json(empty_metrics_response(enabled, false));
    }
    let Some(tip_hash) = current_real_chain_tip(&state).await else {
        return Json(empty_metrics_response(enabled, false));
    };
    let store = state.ui_data_store.clone();
    let ready = tokio::task::spawn_blocking(move || store.metrics_are_projected_to(&tip_hash))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);
    let store = state.ui_data_store.clone();
    let rows = tokio::task::spawn_blocking(move || match query.limit {
        Some(limit) => store.load_recent_metrics(limit.clamp(1, DATASET_LIMIT)),
        None => store.load_metrics(),
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    let store = state.ui_data_store.clone();
    let leaderboards = tokio::task::spawn_blocking(move || store.load_leaderboards(10))
        .await
        .ok()
        .and_then(Result::ok)
        .map(metrics_leaderboards)
        .unwrap_or_default();
    if rows.is_empty() {
        return Json(empty_metrics_response(enabled, !ready));
    }
    Json(metrics_response(enabled, !ready, rows, leaderboards))
}

fn empty_metrics_response(enabled: bool, preparing: bool) -> MetricsResponse {
    MetricsResponse {
        enabled,
        preparing,
        latest: None,
        charts: Vec::new(),
        leaderboards: MetricsLeaderboards::default(),
    }
}

fn metrics_leaderboards(
    leaderboards: crate::adapters::ui_data_store::UiLeaderboards,
) -> MetricsLeaderboards {
    MetricsLeaderboards {
        balances: leaderboards
            .balances
            .into_iter()
            .map(metrics_leaderboard_entry)
            .collect(),
        miners: leaderboards
            .miners
            .into_iter()
            .map(metrics_leaderboard_entry)
            .collect(),
        burners: leaderboards
            .burners
            .into_iter()
            .map(metrics_leaderboard_entry)
            .collect(),
    }
}

fn metrics_leaderboard_entry(
    entry: crate::adapters::ui_data_store::UiLeaderboardEntry,
) -> LeaderboardEntry {
    LeaderboardEntry {
        address: entry.address,
        amount: entry.amount,
        count: entry.count,
    }
}

pub(super) async fn api_network_health(
    State(state): State<HttpState>,
) -> Json<NetworkHealthResponse> {
    let sync_progress = state.gossip.sync_progress();
    let (local, mempool) = {
        let node = state.node.lock().await;
        let status = node.status();
        let tip = node.chain().last().cloned();
        let p2p_metrics = state.gossip.metrics();
        let tip_timestamp_ms = tip
            .as_ref()
            .filter(|block| block.height > 0)
            .map(|block| block.timestamp_ms);
        let mempool = MempoolCounts {
            plain_transactions: node.pending_transactions().len(),
        };
        (
            NetworkHealthLocalState {
                height: status.chain.height,
                tip_hash: status.chain.tip_hash,
                tip_timestamp_ms,
                sync_start_height: sync_progress.map(|progress| progress.start_height),
                sync_validated_height: sync_progress.map(|progress| progress.validated_height),
                sync_target_height: sync_progress.map(|progress| progress.target_height),
                pending_transactions: mempool.total(),
                last_finalizer_mode: tip.as_ref().map(|block| match block.finalizer_mode {
                    crate::domain::FinalizerMode::Ticket => "ticket".to_string(),
                    crate::domain::FinalizerMode::Recovery => "recovery".to_string(),
                }),
                last_finalizer_rank: tip.as_ref().map(|block| block.finalizer_rank),
                last_block_finalizer: tip.as_ref().map(|block| block.miner.clone()),
                current_leader: status.mining.current_leader,
                wallet_is_current_leader: status.mining.wallet_is_current_leader,
                last_auto_finalization_status: status.mining.last_auto_finalization_status,
                vdf_rounds: status.mining.vdf_rounds,
                vdf_target_block_ms: status.mining.vdf_target_block_ms,
                rejected_blocks: p2p_metrics.rejected_blocks,
                rejected_block_batches: p2p_metrics.rejected_block_batches,
                rejected_snapshots: p2p_metrics.rejected_snapshots,
                last_chain_payload_error: p2p_metrics.last_chain_payload_error,
            },
            mempool,
        )
    };
    let peers = state.peers.lock().await.list();
    Json(network_health(local, &peers, mempool))
}
