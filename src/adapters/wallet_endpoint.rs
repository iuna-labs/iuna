use std::{net::SocketAddr, sync::Arc};

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
    adapters::{p2p::GossipNetwork, ui_data_store::SqliteUiDataStore},
    app::{NETWORK_ID, PROTOCOL_VERSION, SharedNode},
    domain::{
        Amount, DEFAULT_FEE_PER_BYTE, MICRO_IUNA, OutPoint, TRANSACTION_SIGNING_FORMAT_VERSION,
        TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT, Transaction, TransactionSubmitOutcome, TxOutput,
    },
};

const MAX_TRANSACTION_BODY_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_TRANSACTION_SUBMISSIONS: usize = 32;

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
        .route("/v1/addresses/{address}/balance", get(balance))
        .route("/v1/addresses/{address}/utxos", get(utxos))
        .route(
            "/v1/addresses/{address}/transactions",
            get(address_transactions),
        )
        .route("/v1/transactions/{transaction_id}", get(transaction))
        .route("/v1/transactions", post(submit_transaction))
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
        api_version: 1,
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
    })
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
