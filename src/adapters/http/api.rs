use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use axum::{
    Json,
    extract::{Query, State},
};

#[cfg(test)]
use crate::domain::Ledger;
use crate::{
    adapters::p2p::P2pMetrics,
    app::{NodeStatus, PeerInfo},
    domain::{BlindedTransaction, OutPoint, Transaction, TxOutput},
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
    add_pending_outputs, cached_chain_view, cached_ui_blocks_for_tip, metrics_response,
    network_health, ui_blinded_reveal, ui_blinded_transaction, ui_blocks_from_indexes,
    ui_pending_revealed_transaction, ui_transaction, wallet_transaction_row,
    wallet_transaction_rows,
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
    if let Some(blocks) = cached_ui_blocks_for_tip(&state, Some(tip_hash.as_str()), blocks).await {
        return Json(blocks);
    }

    let (snapshot, blocks) = {
        let node = state.node.lock().await;
        let snapshot = node.chain_snapshot();
        let blocks = match query.before_height {
            Some(before_height) => node.blocks_before(before_height, limit),
            None => node.recent_blocks(limit),
        };
        (snapshot, blocks)
    };
    let view = cached_chain_view(&state, &snapshot)
        .await
        .unwrap_or_default();
    Json(ui_blocks_from_indexes(
        blocks,
        &view.outputs,
        &view.revealed_by_height,
        &view.burn_leader_ranks_by_hash,
    ))
}

pub(super) async fn api_config(State(state): State<HttpState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        config: state.ui_config.lock().await.clone(),
        p2p_inbound_runtime_active: state.gossip.accepts_inbound().await,
        p2p_runtime_bind_addr: state.gossip.listen_addr().to_string(),
    })
}

pub(super) async fn api_mempool(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<UiTransaction>> {
    let ui_data_ready = ensure_ui_data_current(&state).await.is_ok();
    let (pending, pending_blinded, pending_reveals, pending_revealed) = {
        let node = state.node.lock().await;
        let pending = node.pending_transactions();
        let pending_blinded = node.pending_blinded_transactions();
        let pending_reveals = node.pending_blinded_reveals();
        let pending_revealed = node
            .pending_revealed_blinded_transactions()
            .into_iter()
            .map(|revealed| (revealed.commitment.clone(), revealed))
            .collect::<BTreeMap<_, _>>();
        (pending, pending_blinded, pending_reveals, pending_revealed)
    };
    let mut required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(pending.iter(), &mut required_outputs);
    collect_blinded_input_outpoints(pending_blinded.iter(), &mut required_outputs);
    collect_transaction_input_outpoints(
        pending_revealed
            .values()
            .map(|revealed| &revealed.transaction),
        &mut required_outputs,
    );
    let mut outputs = if ui_data_ready {
        load_outputs_for_outpoints(&state, required_outputs)
            .await
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    add_pending_outputs(&mut outputs, &pending);
    let mut items = pending
        .iter()
        .map(|tx| ui_transaction(tx, &outputs))
        .collect::<Vec<_>>();
    items.extend(
        pending_blinded
            .iter()
            .map(|transaction| ui_blinded_transaction(transaction, &outputs)),
    );
    items.extend(pending_reveals.iter().map(|reveal| {
        pending_revealed
            .get(&reveal.commitment)
            .map(|revealed| ui_pending_revealed_transaction(revealed, &outputs))
            .unwrap_or_else(|| ui_blinded_reveal(reveal))
    }));
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
    if ensure_ui_data_current(&state).await.is_err() {
        return Json(Page {
            items: Vec::new(),
            offset,
            limit,
            total: 0,
            has_more: false,
            next_offset: None,
        });
    }
    let (wallet, pending, owned_blinded) = {
        let node = state.node.lock().await;
        (
            node.wallet_address().to_string(),
            node.pending_transactions(),
            node.owned_blinded_payloads(),
        )
    };
    let mut pending_required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(pending.iter(), &mut pending_required_outputs);
    collect_transaction_input_outpoints(owned_blinded.iter(), &mut pending_required_outputs);
    let mut pending_outputs = load_outputs_for_outpoints(&state, pending_required_outputs)
        .await
        .unwrap_or_default();
    add_pending_outputs(&mut pending_outputs, &pending);
    let pending_rows = wallet_transaction_rows(
        &wallet,
        pending.clone(),
        owned_blinded.clone(),
        &[],
        &BTreeMap::new(),
        &pending_outputs,
        filters,
    );
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
                blinded: row.blinded,
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

async fn ensure_ui_data_current(state: &HttpState) -> Result<()> {
    let Some(tip_hash) = current_real_chain_tip(state).await else {
        return Ok(());
    };
    if ui_data_matches_tip(state, tip_hash).await? {
        return Ok(());
    }

    let _refresh_guard = state.ui_data_refresh.lock().await;
    let Some(tip_hash) = current_real_chain_tip(state).await else {
        return Ok(());
    };
    if ui_data_matches_tip(state, tip_hash).await? {
        return Ok(());
    }

    let (snapshot, tip_hash) = {
        let node = state.node.lock().await;
        if !node.has_real_chain() {
            return Ok(());
        }
        (node.chain_snapshot(), node.chain_tip_hash())
    };
    let keep_metrics = state.ui_config.lock().await.keep_track_of_metrics;
    let chain_store = state.chain_store.clone();
    let ui_data_store = state.ui_data_store.clone();
    tokio::task::spawn_blocking(move || {
        chain_store
            .save(&snapshot)
            .context("failed to persist chain before UI data catch-up")?;
        ui_data_store
            .project_snapshot(&snapshot, keep_metrics)
            .context("failed to project UI data catch-up")?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("UI data catch-up worker failed")??;

    ui_data_matches_tip(state, tip_hash)
        .await?
        .then_some(())
        .context("UI data catch-up completed but projection tip does not match the chain tip")
}

async fn current_real_chain_tip(state: &HttpState) -> Option<String> {
    let node = state.node.lock().await;
    node.has_real_chain().then(|| node.chain_tip_hash())
}

async fn ui_data_matches_tip(state: &HttpState, tip_hash: String) -> Result<bool> {
    let store = state.ui_data_store.clone();
    tokio::task::spawn_blocking(move || store.is_projected_to(&tip_hash))
        .await
        .context("UI data projection metadata worker failed")?
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

fn collect_blinded_input_outpoints<'a>(
    transactions: impl IntoIterator<Item = &'a BlindedTransaction>,
    outpoints: &mut BTreeSet<OutPoint>,
) {
    for transaction in transactions {
        outpoints.extend(
            transaction
                .inputs
                .iter()
                .map(|input| input.outpoint.clone()),
        );
    }
}

pub(super) async fn api_wallet_utxos(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<WalletUtxoRow>> {
    if ensure_ui_data_current(&state).await.is_err() {
        return Json(page_items(Vec::new(), query));
    }
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
    if ensure_ui_data_current(&state).await.is_err() {
        return Json(Vec::new());
    }
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

#[cfg(test)]
pub(super) fn wallet_utxo_rows(ledger: &Ledger, wallet: &str) -> Vec<WalletUtxoRow> {
    let spendable_outpoints = ledger
        .available_utxos_for_address(wallet)
        .unwrap_or_default()
        .into_iter()
        .map(|(outpoint, _)| outpoint)
        .collect::<BTreeSet<_>>();
    let mut utxos = ledger
        .utxos_for_address(wallet)
        .into_iter()
        .map(|(outpoint, output)| {
            let spendable = spendable_outpoints.contains(&outpoint);
            WalletUtxoRow {
                outpoint,
                address: output.address,
                amount: output.amount,
                spendable,
            }
        })
        .collect::<Vec<_>>();
    utxos.sort_by(|left, right| {
        right
            .amount
            .cmp(&left.amount)
            .then_with(|| left.outpoint.txid.cmp(&right.outpoint.txid))
            .then_with(|| left.outpoint.index.cmp(&right.outpoint.index))
    });
    utxos
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

#[cfg(test)]
pub(super) fn selectable_wallet_utxo_rows(ledger: &Ledger, wallet: &str) -> Vec<WalletUtxoRow> {
    wallet_utxo_rows(ledger, wallet)
        .into_iter()
        .filter(|utxo| utxo.spendable)
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
        return Json(empty_metrics_response(enabled));
    }
    if ensure_ui_data_current(&state).await.is_err() {
        return Json(empty_metrics_response(enabled));
    }
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
    Json(metrics_response(enabled, rows, leaderboards))
}

fn empty_metrics_response(enabled: bool) -> MetricsResponse {
    MetricsResponse {
        enabled,
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
    let (local, mempool) = {
        let node = state.node.lock().await;
        let mempool = MempoolCounts {
            plain_transactions: node.pending_transactions().len(),
            blinded_transactions: node.pending_blinded_transactions().len(),
            blinded_reveals: node.pending_blinded_reveals().len(),
        };
        (
            NetworkHealthLocalState {
                height: node.chain_height(),
                pending_transactions: mempool.total(),
            },
            mempool,
        )
    };
    let peers = state.peers.lock().await.list();
    Json(network_health(local, &peers, mempool))
}
