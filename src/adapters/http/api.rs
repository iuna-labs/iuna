use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
};

use crate::{
    adapters::p2p::P2pMetrics,
    app::{NodeStatus, PeerInfo},
    domain::{AddressNetwork, OutPoint, Transaction, TransactionV2, TxOutput, decode_hex},
    ip_geolocation::IpGeolocation,
};

use super::types::{LeaderboardEntry, MetricsLeaderboards};
use super::{
    BlocksQuery, ConfigResponse, MempoolCounts, MetricsQuery, MetricsResponse,
    NetworkHealthLocalState, NetworkHealthResponse, Page, PageQuery, PeerPresentation, UiBlock,
    UiTransaction, WalletTransactionContext, WalletTransactionFilters, WalletTransactionRow,
    WalletTransactionsQuery, WalletUtxoRow,
};
use super::{
    DATASET_LIMIT, DATASET_PAGE_LIMIT, EXPLORER_LIMIT, EXPLORER_PAGE_LIMIT, HttpState,
    add_pending_outputs, add_pending_v2_outputs, metrics_response, network_health,
    populate_wallet_reward_flow, top_mine_proofs, transaction_v2_input_outpoints,
    ui_blocks_from_indexes, ui_transaction, ui_transaction_v2, wallet_transaction_rows,
    wallet_transaction_v2_row, wallet_transaction_v2_rows,
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
    let (tip_hash, blocks, storage_size_breakdowns, transaction_v2_domain, network) = {
        let node = state.node.lock().await;
        let blocks = match query.before_height {
            Some(before_height) => node.blocks_before(before_height, limit),
            None => node.recent_blocks(limit),
        };
        let storage_size_breakdowns = node.block_storage_size_breakdowns(&blocks);
        (
            node.chain_tip_hash(),
            blocks,
            storage_size_breakdowns,
            node.ledger().transaction_v2_domain().ok(),
            AddressNetwork::from_profile_id(&node.ledger().launch_profile().profile_id),
        )
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
        &storage_size_breakdowns,
        transaction_v2_domain.as_ref(),
        network,
    ))
}

pub(super) async fn api_config(State(state): State<HttpState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        config: state.ui_config.lock().await.clone(),
        p2p_inbound_runtime_active: state.gossip.accepts_inbound().await,
        p2p_runtime_bind_addr: state.gossip.listen_addr().to_string(),
        stratum_runtime_enabled: state.stratum.enabled,
        stratum_runtime_listen_addr: state.stratum.listen_addr.clone(),
        wallet_endpoint_runtime_enabled: state.wallet_endpoint_addr.is_some(),
        wallet_endpoint_runtime_listen_addr: state
            .wallet_endpoint_addr
            .map(|addr| addr.to_string()),
    })
}

pub(super) async fn api_mempool(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<UiTransaction>> {
    let (pending, pending_v2, domain, network, confirmed_outputs) = {
        let node = state.node.lock().await;
        let pending_v2 = node.pending_transactions_v2();
        let confirmed_outputs = pending_v2
            .iter()
            .flat_map(transaction_v2_input_outpoints)
            .filter_map(|outpoint| {
                node.ledger()
                    .output_for_outpoint(&outpoint)
                    .map(|output| (outpoint, output))
            })
            .collect::<BTreeMap<_, _>>();
        (
            node.pending_transactions(),
            pending_v2,
            node.ledger().transaction_v2_domain().ok(),
            AddressNetwork::from_profile_id(&node.ledger().launch_profile().profile_id),
            confirmed_outputs,
        )
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
    let mut v2_outputs = confirmed_outputs;
    if let Some(domain) = domain.as_ref() {
        let _ = add_pending_v2_outputs(&mut v2_outputs, &pending_v2, domain, network);
        items.extend(pending_v2.iter().filter_map(|transaction| {
            ui_transaction_v2(transaction, &v2_outputs, domain, network).ok()
        }));
    }
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
    let (wallet_addresses, pending, pending_v2, domain, network, v2_outputs) = {
        let node = state.node.lock().await;
        let status = node.status();
        let wallet = node.wallet_address().to_string();
        let mut wallet_addresses = vec![wallet.clone(), status.wallet_receive_address];
        if let Some(address) = status.quantum_migration.hybrid_address {
            wallet_addresses.push(address);
        }
        let pending_v2 = node.pending_transactions_v2();
        let v2_outputs = pending_v2
            .iter()
            .flat_map(transaction_v2_input_outpoints)
            .filter_map(|outpoint| {
                node.ledger()
                    .output_for_outpoint(&outpoint)
                    .map(|output| (outpoint, output))
            })
            .collect::<BTreeMap<_, _>>();
        (
            wallet_addresses,
            node.pending_transactions(),
            pending_v2,
            node.ledger().transaction_v2_domain().ok(),
            AddressNetwork::from_profile_id(&node.ledger().launch_profile().profile_id),
            v2_outputs,
        )
    };
    let mut pending_required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(pending.iter(), &mut pending_required_outputs);
    let mut pending_outputs = load_outputs_for_outpoints(&state, pending_required_outputs)
        .await
        .unwrap_or_default();
    add_pending_outputs(&mut pending_outputs, &pending);
    let mut pending_rows = wallet_transaction_rows(
        &wallet_addresses,
        pending.clone(),
        &[],
        &pending_outputs,
        filters,
    );
    if let Some(domain) = domain.as_ref() {
        let mut v2_outputs = v2_outputs;
        let _ = add_pending_v2_outputs(&mut v2_outputs, &pending_v2, domain, network);
        let mut v2_rows = wallet_transaction_v2_rows(
            &wallet_addresses,
            &pending_v2,
            &v2_outputs,
            filters,
            domain,
            network,
        );
        v2_rows.append(&mut pending_rows);
        pending_rows = v2_rows;
    }
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
    let wallet_addresses_for_query = wallet_addresses.clone();
    let confirmed_fetch_limit = confirmed_offset.saturating_add(remaining_limit);
    let ((confirmed_rows, confirmed_legacy_total), (confirmed_v2_rows, confirmed_v2_total)) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            Ok((
                store.load_wallet_transactions_for_addresses(
                    &wallet_addresses_for_query,
                    &kinds,
                    0,
                    confirmed_fetch_limit,
                )?,
                store.load_wallet_transactions_v2(
                    &wallet_addresses_for_query,
                    &kinds,
                    0,
                    confirmed_fetch_limit,
                )?,
            ))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let decoded_confirmed_v2 = domain.as_ref().map_or_else(Vec::new, |expected_domain| {
        confirmed_v2_rows
            .into_iter()
            .filter_map(|row| {
                let bytes = decode_hex(&row.envelope).ok()?;
                let (decoded_domain, transaction) = TransactionV2::decode(&bytes).ok()?;
                (decoded_domain == *expected_domain).then_some((row, transaction))
            })
            .collect::<Vec<_>>()
    });
    let mut confirmed_required_outputs = BTreeSet::new();
    collect_transaction_input_outpoints(
        confirmed_rows.iter().map(|row| &row.transaction),
        &mut confirmed_required_outputs,
    );
    for (_, transaction) in &decoded_confirmed_v2 {
        confirmed_required_outputs.extend(transaction_v2_input_outpoints(transaction));
    }
    let confirmed_outputs = load_outputs_for_outpoints(&state, confirmed_required_outputs)
        .await
        .unwrap_or_default();
    let reward_blocks = {
        let node = state.node.lock().await;
        confirmed_rows
            .iter()
            .filter(|row| row.kind == "reward")
            .filter_map(|row| {
                let block = node.chain().get(row.block_height as usize)?.clone();
                Some((row.block_height, block))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let mut confirmed_items = confirmed_rows
        .into_iter()
        .filter_map(|row| {
            let sort_key = row.sort_key;
            let is_reward = row.kind == "reward";
            let mut item = super::ui::wallet_transaction_row_for_addresses(
                &wallet_addresses,
                &row.transaction,
                &confirmed_outputs,
                &WalletTransactionContext {
                    status: "confirmed",
                    block_height: Some(row.block_height),
                    timestamp_ms: Some(row.timestamp_ms),
                    block_finalizer: Some(row.block_finalizer),
                },
            )?;
            if is_reward {
                item.kind = "reward";
                item.from = "fees".to_string();
                item.direction = "reward";
                if let Some(block) = reward_blocks.get(&row.block_height) {
                    populate_wallet_reward_flow(&mut item, block);
                }
            }
            Some((sort_key, item))
        })
        .collect::<Vec<_>>();
    if let Some(domain) = domain.as_ref() {
        confirmed_items.extend(decoded_confirmed_v2.into_iter().filter_map(
            |(row, transaction)| {
                let sort_key = row.sort_key;
                let context = WalletTransactionContext {
                    status: "confirmed",
                    block_height: Some(row.block_height),
                    timestamp_ms: Some(row.timestamp_ms),
                    block_finalizer: Some(row.block_finalizer),
                };
                wallet_transaction_v2_row(
                    &wallet_addresses,
                    &transaction,
                    &confirmed_outputs,
                    domain,
                    network,
                    &context,
                )
                .ok()
                .flatten()
                .map(|item| (sort_key, item))
            },
        ));
    }
    confirmed_items.sort_by(|left, right| right.0.cmp(&left.0));
    items.extend(
        confirmed_items
            .into_iter()
            .skip(confirmed_offset)
            .take(remaining_limit)
            .map(|(_, item)| item),
    );
    let confirmed_total = confirmed_legacy_total.saturating_add(confirmed_v2_total);
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
    if filters.reward {
        kinds.push("reward");
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
) -> Json<Page<PeerPresentation>> {
    let peers = peer_presentations(state.peers.lock().await.list(), IpGeolocation::bundled());
    Json(page_items(peers, query))
}

fn peer_presentations(peers: Vec<PeerInfo>, geolocation: &IpGeolocation) -> Vec<PeerPresentation> {
    peers
        .into_iter()
        .map(|peer| {
            let country_code = peer
                .address
                .parse::<SocketAddr>()
                .ok()
                .and_then(|address| geolocation.country_for_ip(address.ip()));
            PeerPresentation { peer, country_code }
        })
        .collect()
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
    let (chain_storage_bytes, top_mine_proofs) = {
        let node = state.node.lock().await;
        (
            node.chain_storage_bytes_by_hash().unwrap_or_default(),
            top_mine_proofs(node.chain(), 10),
        )
    };
    if rows.is_empty() {
        let mut response = empty_metrics_response(enabled, !ready);
        response.top_mine_proofs = top_mine_proofs;
        return Json(response);
    }
    Json(metrics_response(
        enabled,
        !ready,
        rows,
        leaderboards,
        &chain_storage_bytes,
        top_mine_proofs,
    ))
}

fn empty_metrics_response(enabled: bool, preparing: bool) -> MetricsResponse {
    MetricsResponse {
        enabled,
        preparing,
        latest: None,
        charts: Vec::new(),
        leaderboards: MetricsLeaderboards::default(),
        top_mine_proofs: Vec::new(),
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
            v2_transactions: node.pending_transactions_v2().len(),
        };
        (
            NetworkHealthLocalState {
                height: status.chain.height,
                tip_hash: status.chain.tip_hash,
                finalized_height: status.chain.finalized_height,
                finalized_hash: status.chain.finalized_hash,
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

#[cfg(test)]
mod tests {
    use super::peer_presentations;
    use crate::{app::PeerBook, ip_geolocation::IpGeolocation};

    #[test]
    fn presents_multiple_peer_countries_without_guessing_hostnames() {
        let peers = PeerBook::from_addresses(vec![
            "8.8.8.8:9444".to_string(),
            "[2001:4860:4860::8888]:9444".to_string(),
            "seed.example:9444".to_string(),
        ])
        .list();
        let geolocation = IpGeolocation::from_entries(&[
            ("8.8.8.0".parse().unwrap(), 24, "NL"),
            ("2001:4860::".parse().unwrap(), 32, "US"),
        ]);

        let presented = peer_presentations(peers, &geolocation);
        assert_eq!(presented[0].country_code.unwrap().as_str(), "NL");
        assert_eq!(presented[1].country_code.unwrap().as_str(), "US");
        assert_eq!(presented[2].country_code, None);

        let json = serde_json::to_value(presented).unwrap();
        assert_eq!(json[0]["country_code"], "NL");
        assert_eq!(json[1]["country_code"], "US");
        assert!(json[2].get("country_code").is_none());
    }
}
