use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::Semaphore};

use crate::{
    adapters::{
        http::{
            types::{WalletTransactionContext, WalletTransactionFilters, WalletTransactionRow},
            ui::{
                add_pending_outputs, add_pending_v2_outputs, transaction_v2_input_outpoints,
                wallet_transaction_row_for_addresses, wallet_transaction_rows,
                wallet_transaction_v2_row, wallet_transaction_v2_rows,
            },
        },
        p2p::GossipNetwork,
        ui_data_store::SqliteUiDataStore,
    },
    app::{NETWORK_ID, PROTOCOL_VERSION, SharedNode},
    domain::{
        AddressNetwork, AddressVersion, Amount, DEFAULT_FEE_PER_BYTE,
        HYBRID_EXTERNAL_ADDRESS_GAP_LIMIT, MAX_BLOCK_BYTES, MICRO_IUNA, OutPoint,
        TRANSACTION_SIGNING_FORMAT_VERSION, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT,
        TRANSACTION_V2_ACTIVATION_HEIGHT, TRANSACTION_V2_WIRE_VERSION, Transaction,
        TransactionSubmitOutcome, TransactionV2, TxOutput, decode_hex, encode_versioned_address,
        hex_encode, transaction_v2_is_active,
    },
};

// A canonical v2 envelope is hexadecimal inside JSON, so its HTTP representation can be a little
// over twice the consensus block budget.
const MAX_TRANSACTION_BODY_BYTES: usize = MAX_BLOCK_BYTES * 2 + 4 * 1024;
const MAX_CONCURRENT_TRANSACTION_SUBMISSIONS: usize = 32;
const MAX_WALLET_ADDRESSES: usize = 10_001;

#[derive(Clone)]
struct WalletEndpointState {
    node: SharedNode,
    gossip: GossipNetwork,
    transaction_slots: Arc<Semaphore>,
    ui_data_store: SqliteUiDataStore,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug, Serialize)]
struct WalletEndpointStatus {
    api_version: u16,
    ready: bool,
    network_migration_required: bool,
    network_id: String,
    protocol_version: u32,
    chain_id: String,
    genesis_hash: String,
    height: u64,
    tip_hash: String,
    transaction_signing_format_version: u16,
    transaction_signing_v1_activation_height: u64,
    default_fee_per_byte: Amount,
    amount_unit: &'static str,
    microiuna_per_iuna: Amount,
    transaction_v2_wire_version: u16,
    transaction_v2_activation_height: Option<u64>,
    transaction_v2_active: bool,
    hybrid_address_gap_limit: u32,
}

#[derive(Debug, Deserialize)]
struct WalletSnapshotRequest {
    addresses: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WalletSnapshotResponse {
    addresses: Vec<WalletAddressSnapshot>,
    confirmed: Amount,
    spendable: Amount,
    pending_outgoing: Amount,
    pending_incoming: Amount,
    height: u64,
    tip_hash: String,
    utxos: Vec<WalletSnapshotUtxo>,
}

#[derive(Debug, Serialize)]
struct WalletAddressSnapshot {
    address: String,
    version: u8,
    used: bool,
    confirmed: Amount,
    spendable: Amount,
}

#[derive(Debug, Serialize)]
struct WalletSnapshotUtxo {
    address: String,
    outpoint: OutPoint,
    output: TxOutput,
}

#[derive(Debug, Deserialize)]
struct WalletTransactionsRequest {
    addresses: Vec<String>,
    #[serde(default = "default_true")]
    transfer: bool,
    #[serde(default = "default_true")]
    mine: bool,
    #[serde(default = "default_true")]
    burn: bool,
    #[serde(default = "default_true")]
    reward: bool,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct WalletTransactionsResponse {
    items: Vec<WalletTransactionRow>,
    offset: usize,
    limit: usize,
    total: usize,
    has_more: bool,
    next_offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct BalanceResponse {
    address: String,
    public_key: String,
    confirmed: Amount,
    spendable: Amount,
    pending_outgoing: Amount,
    pending_incoming: Amount,
    height: u64,
    tip_hash: String,
}

#[derive(Debug, Serialize)]
struct UtxoResponse {
    address: String,
    public_key: String,
    height: u64,
    tip_hash: String,
    utxos: Vec<WalletUtxo>,
}

#[derive(Debug, Serialize)]
struct WalletUtxo {
    outpoint: OutPoint,
    output: TxOutput,
}

#[derive(Debug, Serialize)]
struct SubmitResponse {
    transaction_id: String,
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct SubmitV2Request {
    envelope: String,
}

#[derive(Debug, Serialize)]
struct TransactionResponse {
    transaction_id: String,
    status: &'static str,
    transaction: Transaction,
}

#[derive(Debug, Default, Deserialize)]
struct TransactionsQuery {
    tx: Option<bool>,
    mine: Option<bool>,
    burn: Option<bool>,
    reward: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Clone, Copy)]
struct TransactionFilters {
    transfer: bool,
    mine: bool,
    burn: bool,
    reward: bool,
}

impl TransactionFilters {
    fn from_query(query: &TransactionsQuery) -> Self {
        Self {
            transfer: query.tx.unwrap_or(true),
            // Keep the public endpoint's unfiltered default backwards compatible.
            // The wallet sends all four choices explicitly.
            mine: query.mine.unwrap_or(true),
            burn: query.burn.unwrap_or(true),
            reward: query.reward.unwrap_or(true),
        }
    }

    fn allows(self, transaction: &Transaction) -> bool {
        match transaction {
            Transaction::Transfer { .. } => self.transfer,
            Transaction::Mine { .. } => self.mine,
            Transaction::Burn { .. } => self.burn,
        }
    }

    fn kinds(self) -> Vec<&'static str> {
        let mut kinds = Vec::with_capacity(4);
        if self.transfer {
            kinds.push("transfer");
        }
        if self.mine {
            kinds.push("mine");
        }
        if self.burn {
            kinds.push("burn");
        }
        if self.reward {
            kinds.push("reward");
        }
        kinds
    }
}

#[derive(Debug, Serialize)]
struct TransactionsResponse {
    items: Vec<AddressTransaction>,
    offset: usize,
    limit: usize,
    total: usize,
    has_more: bool,
    next_offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct AddressTransaction {
    kind: String,
    status: &'static str,
    block_height: Option<u64>,
    timestamp_ms: Option<u64>,
    transaction: Transaction,
}

pub async fn serve(
    node: SharedNode,
    gossip: GossipNetwork,
    ui_data_store: SqliteUiDataStore,
    addr: SocketAddr,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding public wallet endpoint on {addr}"))?;
    println!("wallet endpoint: http://{}", listener.local_addr()?);
    axum::serve(listener, router(node, gossip, ui_data_store))
        .await
        .context("serving public wallet endpoint")
}

fn router(node: SharedNode, gossip: GossipNetwork, ui_data_store: SqliteUiDataStore) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/wallets/snapshot", post(wallet_snapshot))
        .route("/v1/wallets/transactions", post(wallet_transactions))
        .route("/v1/addresses/{address}/balance", get(balance))
        .route("/v1/addresses/{address}/utxos", get(utxos))
        .route(
            "/v1/addresses/{address}/transactions",
            get(address_transactions),
        )
        .route("/v1/transactions/{transaction_id}", get(transaction))
        .route("/v1/transactions", post(submit_transaction))
        .route("/v1/transactions-v2", post(submit_transaction_v2))
        .layer(DefaultBodyLimit::max(MAX_TRANSACTION_BODY_BYTES))
        .layer(middleware::from_fn(cors))
        .with_state(WalletEndpointState {
            node,
            gossip,
            transaction_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSACTION_SUBMISSIONS)),
            ui_data_store,
        })
}

async fn status(State(state): State<WalletEndpointState>) -> Json<WalletEndpointStatus> {
    let node = state.node.lock().await;
    let ledger = node.ledger();
    Json(WalletEndpointStatus {
        api_version: 2,
        ready: node.has_real_chain() && node.network_migration_from().is_none(),
        network_migration_required: node.network_migration_from().is_some(),
        network_id: NETWORK_ID.to_string(),
        protocol_version: PROTOCOL_VERSION,
        chain_id: ledger.launch_profile().profile_id.clone(),
        genesis_hash: ledger.genesis_hash().to_string(),
        height: ledger.height(),
        tip_hash: ledger.tip_hash().to_string(),
        transaction_signing_format_version: TRANSACTION_SIGNING_FORMAT_VERSION,
        transaction_signing_v1_activation_height: TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT,
        default_fee_per_byte: DEFAULT_FEE_PER_BYTE,
        amount_unit: "microiuna",
        microiuna_per_iuna: MICRO_IUNA,
        transaction_v2_wire_version: TRANSACTION_V2_WIRE_VERSION,
        transaction_v2_activation_height: TRANSACTION_V2_ACTIVATION_HEIGHT,
        transaction_v2_active: transaction_v2_is_active(ledger.height().saturating_add(1)),
        hybrid_address_gap_limit: HYBRID_EXTERNAL_ADDRESS_GAP_LIMIT,
    })
}

async fn wallet_snapshot(
    State(state): State<WalletEndpointState>,
    Json(request): Json<WalletSnapshotRequest>,
) -> Result<Json<WalletSnapshotResponse>, (StatusCode, Json<ApiError>)> {
    if request.addresses.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "at least one address is required",
        ));
    }
    if request.addresses.len() > MAX_WALLET_ADDRESSES {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("a wallet snapshot accepts at most {MAX_WALLET_ADDRESSES} addresses"),
        ));
    }

    let node = state.node.lock().await;
    let ledger = node.ledger();
    let network =
        crate::domain::AddressNetwork::from_profile_id(&ledger.launch_profile().profile_id);
    let used_hybrid = ledger
        .used_hybrid_encoded_addresses()
        .map_err(internal_error)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut requested_addresses = Vec::new();
    for requested in request.addresses {
        let decoded = node.decode_user_address(&requested).map_err(bad_request)?;
        let display = encode_versioned_address(decoded, network).map_err(bad_request)?;
        if !seen.insert(display.clone()) {
            continue;
        }
        let ledger_address = match decoded.version {
            AddressVersion::Ed25519PublicKey => hex_encode(decoded.payload),
            AddressVersion::HybridKeyCommitment => display.clone(),
        };
        requested_addresses.push((display, decoded.version, ledger_address));
    }
    let ledger_addresses = requested_addresses
        .iter()
        .map(|(_, _, address)| address.clone())
        .collect::<BTreeSet<_>>();
    let display_by_ledger_address = requested_addresses
        .iter()
        .map(|(display, _, ledger_address)| (ledger_address.clone(), display.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut confirmed_by_address = BTreeMap::<String, Amount>::new();
    for (_, output) in ledger.utxos_for_addresses(&ledger_addresses) {
        let balance = confirmed_by_address.entry(output.address).or_default();
        *balance = balance.checked_add(output.amount).ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wallet balance overflows",
            )
        })?;
    }
    let available = ledger
        .available_utxos_for_addresses(&ledger_addresses)
        .map_err(internal_error)?;
    let mut spendable_by_address = BTreeMap::<String, Amount>::new();
    for (_, output) in &available {
        let balance = spendable_by_address
            .entry(output.address.clone())
            .or_default();
        *balance = balance.checked_add(output.amount).ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wallet balance overflows",
            )
        })?;
    }
    let mut confirmed = 0_u64;
    let mut spendable = 0_u64;
    let addresses = requested_addresses
        .into_iter()
        .map(|(display, version, ledger_address)| {
            let address_confirmed = confirmed_by_address
                .get(&ledger_address)
                .copied()
                .unwrap_or_default();
            let address_spendable = spendable_by_address
                .get(&ledger_address)
                .copied()
                .unwrap_or_default();
            confirmed = confirmed.checked_add(address_confirmed).ok_or_else(|| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wallet balance overflows",
                )
            })?;
            spendable = spendable.checked_add(address_spendable).ok_or_else(|| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wallet balance overflows",
                )
            })?;
            Ok(WalletAddressSnapshot {
                address: display.clone(),
                version: version.wire_id(),
                used: version == AddressVersion::HybridKeyCommitment
                    && used_hybrid.contains(&display),
                confirmed: address_confirmed,
                spendable: address_spendable,
            })
        })
        .collect::<Result<Vec<_>, (StatusCode, Json<ApiError>)>>()?;
    let mut utxos = available
        .into_iter()
        .filter_map(|(outpoint, output)| {
            display_by_ledger_address
                .get(&output.address)
                .cloned()
                .map(|address| WalletSnapshotUtxo {
                    address,
                    outpoint,
                    output,
                })
        })
        .collect::<Vec<_>>();
    utxos.sort_by(|left, right| {
        right
            .output
            .amount
            .cmp(&left.output.amount)
            .then_with(|| left.outpoint.cmp(&right.outpoint))
    });
    Ok(Json(WalletSnapshotResponse {
        addresses,
        confirmed,
        spendable,
        pending_outgoing: confirmed.saturating_sub(spendable),
        pending_incoming: spendable.saturating_sub(confirmed),
        height: ledger.height(),
        tip_hash: ledger.tip_hash().to_string(),
        utxos,
    }))
}

async fn wallet_transactions(
    State(state): State<WalletEndpointState>,
    Json(request): Json<WalletTransactionsRequest>,
) -> Result<Json<WalletTransactionsResponse>, (StatusCode, Json<ApiError>)> {
    if request.addresses.is_empty() || request.addresses.len() > MAX_WALLET_ADDRESSES {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("provide between 1 and {MAX_WALLET_ADDRESSES} wallet addresses"),
        ));
    }
    let offset = request.offset;
    let limit = request.limit.unwrap_or(25).clamp(1, 100);
    let filters = WalletTransactionFilters {
        transfer: request.transfer,
        mine: request.mine,
        burn: request.burn,
        reward: request.reward,
    };
    let kinds = wallet_transaction_kinds(filters);
    let (addresses, pending, pending_v2, domain, network, mut pending_outputs) = {
        let node = state.node.lock().await;
        let ledger = node.ledger();
        let network = AddressNetwork::from_profile_id(&ledger.launch_profile().profile_id);
        let mut addresses = Vec::new();
        for requested in request.addresses {
            let decoded = node.decode_user_address(&requested).map_err(bad_request)?;
            let address = match decoded.version {
                AddressVersion::Ed25519PublicKey => hex_encode(decoded.payload),
                AddressVersion::HybridKeyCommitment => {
                    encode_versioned_address(decoded, network).map_err(bad_request)?
                }
            };
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        let pending_v2 = node.pending_transactions_v2();
        let pending_outputs = pending_v2
            .iter()
            .flat_map(transaction_v2_input_outpoints)
            .filter_map(|outpoint| {
                ledger
                    .output_for_outpoint(&outpoint)
                    .map(|output| (outpoint, output))
            })
            .collect::<BTreeMap<_, _>>();
        (
            addresses,
            node.pending_transactions(),
            pending_v2,
            ledger.transaction_v2_domain().map_err(internal_error)?,
            network,
            pending_outputs,
        )
    };

    let mut pending_outpoints = BTreeSet::new();
    collect_legacy_input_outpoints(pending.iter(), &mut pending_outpoints);
    pending_outpoints.extend(pending_v2.iter().flat_map(transaction_v2_input_outpoints));
    if !pending_outpoints.is_empty() {
        let store = state.ui_data_store.clone();
        let stored = tokio::task::spawn_blocking(move || store.load_outputs(&pending_outpoints))
            .await
            .map_err(internal_error)?
            .map_err(internal_error)?;
        pending_outputs.extend(stored);
    }
    add_pending_outputs(&mut pending_outputs, &pending);
    add_pending_v2_outputs(&mut pending_outputs, &pending_v2, &domain, network)
        .map_err(internal_error)?;
    let mut pending_rows = wallet_transaction_v2_rows(
        &addresses,
        &pending_v2,
        &pending_outputs,
        filters,
        &domain,
        network,
    );
    pending_rows.extend(wallet_transaction_rows(
        &addresses,
        pending,
        &[],
        &pending_outputs,
        filters,
    ));
    let pending_total = pending_rows.len();
    let mut items = pending_rows
        .into_iter()
        .skip(offset.min(pending_total))
        .take(limit)
        .collect::<Vec<_>>();

    let confirmed_offset = offset.saturating_sub(pending_total);
    let remaining = limit.saturating_sub(items.len());
    let fetch_limit = confirmed_offset.saturating_add(remaining);
    let store = state.ui_data_store.clone();
    let query_addresses = addresses.clone();
    let ((legacy_rows, legacy_total), (v2_rows, v2_total)) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            Ok((
                store.load_wallet_transactions_for_addresses(
                    &query_addresses,
                    &kinds,
                    0,
                    fetch_limit,
                )?,
                store.load_wallet_transactions_v2(&query_addresses, &kinds, 0, fetch_limit)?,
            ))
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;

    let decoded_v2 = v2_rows
        .into_iter()
        .filter_map(|row| {
            let bytes = decode_hex(&row.envelope).ok()?;
            let (decoded_domain, transaction) = TransactionV2::decode(&bytes).ok()?;
            (decoded_domain == domain).then_some((row, transaction))
        })
        .collect::<Vec<_>>();
    let mut confirmed_outpoints = BTreeSet::new();
    collect_legacy_input_outpoints(
        legacy_rows.iter().map(|row| &row.transaction),
        &mut confirmed_outpoints,
    );
    confirmed_outpoints.extend(
        decoded_v2
            .iter()
            .flat_map(|(_, transaction)| transaction_v2_input_outpoints(transaction)),
    );
    let confirmed_outputs = if confirmed_outpoints.is_empty() {
        BTreeMap::new()
    } else {
        let store = state.ui_data_store.clone();
        tokio::task::spawn_blocking(move || store.load_outputs(&confirmed_outpoints))
            .await
            .map_err(internal_error)?
            .map_err(internal_error)?
    };
    let mut confirmed = legacy_rows
        .into_iter()
        .filter_map(|row| {
            wallet_transaction_row_for_addresses(
                &addresses,
                &row.transaction,
                &confirmed_outputs,
                &WalletTransactionContext {
                    status: "confirmed",
                    block_height: Some(row.block_height),
                    timestamp_ms: Some(row.timestamp_ms),
                    block_finalizer: Some(row.block_finalizer),
                },
            )
            .map(|item| (row.sort_key, item))
        })
        .collect::<Vec<_>>();
    confirmed.extend(decoded_v2.into_iter().filter_map(|(row, transaction)| {
        wallet_transaction_v2_row(
            &addresses,
            &transaction,
            &confirmed_outputs,
            &domain,
            network,
            &WalletTransactionContext {
                status: "confirmed",
                block_height: Some(row.block_height),
                timestamp_ms: Some(row.timestamp_ms),
                block_finalizer: Some(row.block_finalizer),
            },
        )
        .ok()
        .flatten()
        .map(|item| (row.sort_key, item))
    }));
    confirmed.sort_by(|left, right| right.0.cmp(&left.0));
    items.extend(
        confirmed
            .into_iter()
            .skip(confirmed_offset)
            .take(remaining)
            .map(|(_, item)| item),
    );
    let total = pending_total
        .saturating_add(legacy_total)
        .saturating_add(v2_total);
    let next_offset = offset.saturating_add(items.len());
    Ok(Json(WalletTransactionsResponse {
        items,
        offset: offset.min(total),
        limit,
        total,
        has_more: next_offset < total,
        next_offset: (next_offset < total).then_some(next_offset),
    }))
}

fn default_true() -> bool {
    true
}

fn wallet_transaction_kinds(filters: WalletTransactionFilters) -> Vec<&'static str> {
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

fn collect_legacy_input_outpoints<'a>(
    transactions: impl IntoIterator<Item = &'a Transaction>,
    outpoints: &mut BTreeSet<OutPoint>,
) {
    for transaction in transactions {
        if let Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } = transaction
        {
            outpoints.extend(inputs.iter().map(|input| input.outpoint.clone()));
        }
    }
}

async fn balance(
    State(state): State<WalletEndpointState>,
    Path(address): Path<String>,
) -> Result<Json<BalanceResponse>, (StatusCode, Json<ApiError>)> {
    let node = state.node.lock().await;
    let public_key = node.normalize_user_address(&address).map_err(bad_request)?;
    let ledger = node.ledger();
    let confirmed = ledger.balance_of(&public_key);
    let spendable = ledger
        .available_utxos_for_address(&public_key)
        .map_err(internal_error)?
        .iter()
        .map(|(_, output)| output.amount)
        .sum();
    Ok(Json(BalanceResponse {
        address: address.trim().to_ascii_lowercase(),
        public_key,
        confirmed,
        spendable,
        pending_outgoing: confirmed.saturating_sub(spendable),
        pending_incoming: spendable.saturating_sub(confirmed),
        height: ledger.height(),
        tip_hash: ledger.tip_hash().to_string(),
    }))
}

async fn utxos(
    State(state): State<WalletEndpointState>,
    Path(address): Path<String>,
) -> Result<Json<UtxoResponse>, (StatusCode, Json<ApiError>)> {
    let node = state.node.lock().await;
    let public_key = node.normalize_user_address(&address).map_err(bad_request)?;
    let ledger = node.ledger();
    let utxos = ledger
        .available_utxos_for_address(&public_key)
        .map_err(internal_error)?
        .into_iter()
        .map(|(outpoint, output)| WalletUtxo { outpoint, output })
        .collect();
    Ok(Json(UtxoResponse {
        address: address.trim().to_ascii_lowercase(),
        public_key,
        height: ledger.height(),
        tip_hash: ledger.tip_hash().to_string(),
        utxos,
    }))
}

async fn transaction(
    State(state): State<WalletEndpointState>,
    Path(transaction_id): Path<String>,
) -> Result<Json<TransactionResponse>, (StatusCode, Json<ApiError>)> {
    let pending = {
        let node = state.node.lock().await;
        let ledger = node.ledger();
        ledger
            .pending()
            .iter()
            .find(|item| item.signature() == transaction_id)
            .cloned()
            .map(|transaction| ("pending", transaction))
            .or_else(|| {
                ledger
                    .orphan_transactions()
                    .iter()
                    .find(|item| item.signature() == transaction_id)
                    .cloned()
                    .map(|transaction| ("orphan", transaction))
            })
    };
    let (status, transaction) = match pending {
        Some(transaction) => transaction,
        None => {
            let store = state.ui_data_store.clone();
            let lookup_id = transaction_id.clone();
            let transaction = tokio::task::spawn_blocking(move || {
                store.load_wallet_transaction_by_signature(&lookup_id)
            })
            .await
            .map_err(internal_error)?
            .map_err(internal_error)?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "transaction not found"))?;
            ("confirmed", transaction.transaction)
        }
    };
    Ok(Json(TransactionResponse {
        transaction_id,
        status,
        transaction,
    }))
}

async fn address_transactions(
    State(state): State<WalletEndpointState>,
    Path(address): Path<String>,
    Query(query): Query<TransactionsQuery>,
) -> Result<Json<TransactionsResponse>, (StatusCode, Json<ApiError>)> {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let filters = TransactionFilters::from_query(&query);
    let (public_key, pending) = {
        let node = state.node.lock().await;
        let public_key = node.normalize_user_address(&address).map_err(bad_request)?;
        let pending = node
            .pending_transactions()
            .into_iter()
            .rev()
            .filter(|transaction| transaction_mentions_address(transaction, &public_key))
            .filter(|transaction| filters.allows(transaction))
            .collect::<Vec<_>>();
        (public_key, pending)
    };
    let pending_total = pending.len();
    let mut items = pending
        .into_iter()
        .skip(offset.min(pending_total))
        .take(limit)
        .map(|transaction| AddressTransaction {
            kind: transaction_kind(&transaction).to_string(),
            status: "pending",
            block_height: None,
            timestamp_ms: None,
            transaction,
        })
        .collect::<Vec<_>>();

    let confirmed_offset = offset.saturating_sub(pending_total);
    let remaining = limit.saturating_sub(items.len());
    let kinds = filters.kinds();
    let confirmed = if kinds.is_empty() {
        (Vec::new(), 0)
    } else {
        let store = state.ui_data_store.clone();
        tokio::task::spawn_blocking(move || {
            store.load_wallet_transactions(&public_key, &kinds, confirmed_offset, remaining)
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?
    };
    let confirmed_total = confirmed.1;
    items.extend(confirmed.0.into_iter().map(|row| AddressTransaction {
        kind: row.kind,
        status: "confirmed",
        block_height: Some(row.block_height),
        timestamp_ms: Some(row.timestamp_ms),
        transaction: row.transaction,
    }));
    let total = pending_total + confirmed_total;
    let next_offset = offset.saturating_add(items.len());
    Ok(Json(TransactionsResponse {
        items,
        offset: offset.min(total),
        limit,
        total,
        has_more: next_offset < total,
        next_offset: (next_offset < total).then_some(next_offset),
    }))
}

fn transaction_mentions_address(transaction: &Transaction, address: &str) -> bool {
    match transaction {
        Transaction::Transfer {
            inputs, outputs, ..
        } => {
            inputs.iter().any(|input| input.owner == address)
                || outputs.iter().any(|output| output.address == address)
        }
        Transaction::Burn { inputs, change, .. } => {
            inputs.iter().any(|input| input.owner == address)
                || change.iter().any(|output| output.address == address)
        }
        Transaction::Mine { recipient, .. } => recipient == address,
    }
}

fn transaction_kind(transaction: &Transaction) -> &'static str {
    match transaction {
        Transaction::Transfer { .. } => "transfer",
        Transaction::Burn { .. } => "burn",
        Transaction::Mine { .. } => "mine",
    }
}

async fn submit_transaction(
    State(state): State<WalletEndpointState>,
    Json(transaction): Json<Transaction>,
) -> Result<(StatusCode, Json<SubmitResponse>), (StatusCode, Json<ApiError>)> {
    let _permit = state
        .transaction_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many transaction submissions",
            )
        })?;
    let transaction_id = transaction.signature().to_string();
    let (outcome, outbox) = {
        let mut node = state.node.lock().await;
        if !node.has_real_chain() {
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "node has not joined a chain yet",
            ));
        }
        if node.network_migration_from().is_some() {
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "node requires a network migration reset",
            ));
        }
        let outcome = node
            .submit_external_wallet_transaction(transaction)
            .map_err(bad_request)?;
        (outcome, node.drain_outbox())
    };
    if outcome.added() {
        state
            .gossip
            .broadcast(outbox)
            .await
            .map_err(internal_error)?;
    }
    let (code, status) = match outcome {
        TransactionSubmitOutcome::Added => (StatusCode::ACCEPTED, "accepted"),
        TransactionSubmitOutcome::AlreadyKnown => (StatusCode::OK, "already_known"),
        TransactionSubmitOutcome::ConflictsWithPending => (StatusCode::CONFLICT, "conflict"),
    };
    Ok((
        code,
        Json(SubmitResponse {
            transaction_id,
            status,
        }),
    ))
}

async fn submit_transaction_v2(
    State(state): State<WalletEndpointState>,
    Json(request): Json<SubmitV2Request>,
) -> Result<(StatusCode, Json<SubmitResponse>), (StatusCode, Json<ApiError>)> {
    let _permit = state
        .transaction_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many transaction submissions",
            )
        })?;
    let (transaction_id, outcome, outbox) = {
        let mut node = state.node.lock().await;
        if !node.has_real_chain() {
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "node has not joined a chain yet",
            ));
        }
        if node.network_migration_from().is_some() {
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "node requires a network migration reset",
            ));
        }
        let (transaction_id, outcome) = node
            .submit_external_wallet_transaction_v2(&request.envelope)
            .map_err(bad_request)?;
        (transaction_id, outcome, node.drain_outbox())
    };
    if outcome.added() {
        state
            .gossip
            .broadcast(outbox)
            .await
            .map_err(internal_error)?;
    }
    let (code, status) = match outcome {
        TransactionSubmitOutcome::Added => (StatusCode::ACCEPTED, "accepted"),
        TransactionSubmitOutcome::AlreadyKnown => (StatusCode::OK, "already_known"),
        TransactionSubmitOutcome::ConflictsWithPending => (StatusCode::CONFLICT, "conflict"),
    };
    Ok((
        code,
        Json(SubmitResponse {
            transaction_id,
            status,
        }),
    ))
}

fn bad_request(error: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    api_error(StatusCode::BAD_REQUEST, error)
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, error)
}

fn api_error(status: StatusCode, error: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: error.to_string(),
        }),
    )
}

async fn cors(request: Request<Body>, next: Next) -> Response {
    if request.method() == Method::OPTIONS {
        return cors_headers(StatusCode::NO_CONTENT.into_response());
    }
    cors_headers(next.run(request).await)
}

fn cors_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr},
        sync::Arc,
    };

    use axum::{body::to_bytes, http::Request};
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    use crate::{
        adapters::p2p::GossipNetwork,
        app::{NodeCore, PeerBook},
        domain::{AddressNetwork, Ledger, MICRO_IUNA, Wallet, encode_address},
    };

    use super::*;

    async fn test_app() -> (Router, Wallet, Ledger, tempfile::TempDir) {
        let wallet = Wallet::from_seed("public-wallet-endpoint-test");
        let ledger = Ledger::new(
            BTreeMap::from([(wallet.address().to_string(), 5 * MICRO_IUNA)]),
            1,
        );
        let node = Arc::new(Mutex::new(NodeCore::from_ledger(
            wallet.clone(),
            ledger.clone(),
            0,
        )));
        let peers = Arc::new(Mutex::new(PeerBook::default()));
        let gossip = GossipNetwork::start(
            node.clone(),
            peers,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            false,
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ui_data_store = SqliteUiDataStore::open(dir.path().join("ui.sqlite3")).unwrap();
        ui_data_store
            .project_snapshot(&ledger.snapshot(), false)
            .unwrap();
        (router(node, gossip, ui_data_store), wallet, ledger, dir)
    }

    #[tokio::test]
    async fn public_router_exposes_wallet_data_but_not_management_api() {
        let (app, wallet, _, _dir) = test_app().await;
        let receive_address = encode_address(wallet.address(), AddressNetwork::Mainnet).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/addresses/{receive_address}/balance"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["confirmed"], 5 * MICRO_IUNA);
        assert_eq!(value["spendable"], 5 * MICRO_IUNA);
        assert_eq!(value["address"], receive_address);
        assert_eq!(value["public_key"], wallet.address());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/wallets/snapshot")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "addresses": [receive_address.clone()]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["confirmed"], 5 * MICRO_IUNA);
        assert_eq!(value["spendable"], 5 * MICRO_IUNA);
        assert_eq!(value["addresses"][0]["version"], 0);
        assert_eq!(value["addresses"][0]["used"], false);
        assert_eq!(value["utxos"].as_array().unwrap().len(), 1);

        let wrong_network = encode_address(wallet.address(), AddressNetwork::Testnet).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/addresses/{wrong_network}/balance"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn signed_transfer_is_accepted_and_available_by_signature() {
        let (app, wallet, ledger, _dir) = test_app().await;
        let receive_address = encode_address(wallet.address(), AddressNetwork::Mainnet).unwrap();
        let recipient = Wallet::from_seed("public-wallet-endpoint-recipient");
        let transaction = ledger
            .build_transfer(&wallet, recipient.address(), MICRO_IUNA, 1)
            .unwrap();
        let transaction_id = transaction.signature().to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/transactions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&transaction).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/transactions/{transaction_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["status"], "pending");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/addresses/{receive_address}/transactions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["items"][0]["status"], "pending");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/wallets/transactions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "addresses": [receive_address.clone()],
                            "limit": 10
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["items"][0]["status"], "pending");
        assert_eq!(value["items"][0]["direction"], "sent");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/addresses/{receive_address}/transactions?tx=false&mine=true&burn=false&reward=false"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["total"], 0);
        assert_eq!(value["has_more"], false);
        assert_eq!(value["items"].as_array().unwrap().len(), 0);
    }
}
