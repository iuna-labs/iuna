use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
};

use crate::{
    adapters::p2p::P2pMetrics,
    app::{NodeStatus, PeerInfo},
    domain::Ledger,
};

use super::{
    BlocksQuery, ConfigResponse, MempoolCounts, MetricsQuery, MetricsResponse,
    NetworkHealthLocalState, NetworkHealthResponse, Page, PageQuery, UiBlock, UiTransaction,
    WalletTransactionFilters, WalletTransactionRow, WalletTransactionsQuery, WalletUtxoRow,
};
use super::{
    DATASET_LIMIT, DATASET_PAGE_LIMIT, EXPLORER_LIMIT, EXPLORER_PAGE_LIMIT, HttpState,
    add_pending_outputs, burn_leader_ranks_for_blocks, cached_chain_view, metrics_response,
    network_health, ui_blinded_reveal, ui_blinded_transaction, ui_blocks_from_indexes,
    ui_pending_revealed_transaction, ui_transaction, wallet_transaction_rows,
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
    let (snapshot, pending, blocks) = {
        let node = state.node.lock().await;
        let snapshot = node.chain_snapshot();
        let pending = node.pending_transactions();
        let blocks = match query.before_height {
            Some(before_height) => node.blocks_before(before_height, limit),
            None => node.recent_blocks(limit),
        };
        (snapshot, pending, blocks)
    };
    let view = cached_chain_view(&state, &snapshot)
        .await
        .unwrap_or_default();
    let burn_leader_ranks = burn_leader_ranks_for_blocks(&snapshot, &blocks);
    let mut outputs = view.outputs;
    add_pending_outputs(&mut outputs, &pending);
    Json(ui_blocks_from_indexes(
        blocks,
        &outputs,
        &view.revealed_by_height,
        &burn_leader_ranks,
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
    let (snapshot, pending, pending_blinded, pending_reveals, pending_revealed) = {
        let node = state.node.lock().await;
        let snapshot = node.chain_snapshot();
        let pending = node.pending_transactions();
        let pending_blinded = node.pending_blinded_transactions();
        let pending_reveals = node.pending_blinded_reveals();
        let pending_revealed = node
            .pending_revealed_blinded_transactions()
            .into_iter()
            .map(|revealed| (revealed.commitment.clone(), revealed))
            .collect::<BTreeMap<_, _>>();
        (
            snapshot,
            pending,
            pending_blinded,
            pending_reveals,
            pending_revealed,
        )
    };
    let view = cached_chain_view(&state, &snapshot)
        .await
        .unwrap_or_default();
    let mut outputs = view.outputs;
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
    let (wallet, snapshot, pending, owned_blinded) = {
        let node = state.node.lock().await;
        (
            node.wallet_address().to_string(),
            node.chain_snapshot(),
            node.pending_transactions(),
            node.owned_blinded_payloads(),
        )
    };
    let view = cached_chain_view(&state, &snapshot)
        .await
        .unwrap_or_default();
    let mut outputs = view.outputs;
    add_pending_outputs(&mut outputs, &pending);
    let page_query = query.page();
    let filters = WalletTransactionFilters::from_query(query);
    Json(page_items(
        wallet_transaction_rows(
            &wallet,
            pending,
            owned_blinded,
            &snapshot.blocks,
            &view.revealed_by_height,
            &outputs,
            filters,
        ),
        page_query,
    ))
}

pub(super) async fn api_wallet_utxos(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<WalletUtxoRow>> {
    let (ledger, wallet) = {
        let node = state.node.lock().await;
        (
            node.wallet_view_ledger()
                .unwrap_or_else(|_| node.clone_ledger()),
            node.wallet_address().to_string(),
        )
    };
    Json(page_items(wallet_utxo_rows(&ledger, &wallet), query))
}

pub(super) async fn api_wallet_selectable_utxos(
    State(state): State<HttpState>,
) -> Json<Vec<WalletUtxoRow>> {
    let (ledger, wallet) = {
        let node = state.node.lock().await;
        (
            node.wallet_view_ledger()
                .unwrap_or_else(|_| node.clone_ledger()),
            node.wallet_address().to_string(),
        )
    };
    Json(selectable_wallet_utxo_rows(&ledger, &wallet))
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
        return Json(MetricsResponse {
            enabled,
            latest: None,
            charts: Vec::new(),
        });
    }
    let store = state.chain_store.clone();
    let rows = tokio::task::spawn_blocking(move || match query.limit {
        Some(limit) => store.load_recent_metrics(limit.clamp(1, DATASET_LIMIT)),
        None => store.load_metrics(),
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    Json(metrics_response(enabled, rows))
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
