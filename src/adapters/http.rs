use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, Extension, Query, State},
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use getrandom::getrandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    adapters::{
        chain_store::{BlockMetricRow, SqliteChainStore},
        config_store,
        config_store::UiConfig,
        p2p::{GossipNetwork, P2pMetrics},
        wallet_store,
    },
    app::{
        FeeEstimate, NodeStatus, PeerDirection, PeerInfo, SharedNode, SharedPeerBook, StratumStatus,
    },
    domain::{
        Amount, BlindedReveal, BlindedTransaction, Block, BurnLeaderRank, ChainSnapshot, Ledger,
        MINE_FINALIZER_FEE, MINE_REWARD, OutPoint, Transaction, TxInput, TxOutput, hex_hash,
        revealed_blinded_transactions,
    },
};

const EXPLORER_LIMIT: usize = 50;
const EXPLORER_PAGE_LIMIT: usize = 20;
const DATASET_LIMIT: usize = 1_000;
const DATASET_PAGE_LIMIT: usize = 25;
const AUTH_COOKIE_NAME: &str = "iuna_session";
const AUTH_SESSION_TTL_MS: u64 = 12 * 60 * 60 * 1_000;
const AUTH_MAX_FAILED_ATTEMPTS: u32 = 5;
const AUTH_LOCKOUT_MS: u64 = 60 * 1_000;
const UNKNOWN_CLIENT_KEY: &str = "unknown";
const PASSWORD_KDF_ALGORITHM: &str = "pbkdf2-sha256";
const PASSWORD_KDF_ITERATIONS: u32 = 120_000;
const PEER_STALE_AFTER_MS: u64 = 20 * 60 * 1_000;

#[derive(Clone)]
struct HttpState {
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    ui_config: Arc<Mutex<UiConfig>>,
    config_path: PathBuf,
    chain_store: SqliteChainStore,
    wallet_path: PathBuf,
    stratum: StratumStatus,
    auth_sessions: Arc<Mutex<BTreeMap<String, AuthSession>>>,
    auth_backoff: Arc<Mutex<BTreeMap<String, AuthBackoff>>>,
}

#[derive(Clone)]
struct AuthSession {
    expires_at: u64,
    wallet_password: String,
}

#[derive(Clone, Debug)]
struct AuthClientKey(String);

#[derive(Clone, Debug, Default)]
struct AuthBackoff {
    failed_attempts: u32,
    locked_until_ms: Option<u64>,
}

pub struct ServeOptions {
    pub config_path: PathBuf,
    pub chain_store: SqliteChainStore,
    pub wallet_path: PathBuf,
    pub stratum: StratumStatus,
    pub addr: SocketAddr,
}

#[derive(Debug, Deserialize)]
struct AuthForm {
    password: String,
}

#[derive(Debug, Deserialize)]
struct ChangePasswordForm {
    old_password: String,
    new_password: String,
}

#[derive(Debug, Serialize)]
struct AuthStatusResponse {
    configured: bool,
    authenticated: bool,
}

#[derive(Debug, Serialize)]
struct NetworkHealthResponse {
    ok: bool,
    state: String,
    local_height: u64,
    best_known_height: u64,
    shared_height: u64,
    lag_blocks: u64,
    outbound_peers: usize,
    inbound_peers: usize,
    healthy_peers: usize,
    failed_peers: usize,
    stale_peers: usize,
    banned_peers: usize,
    pending_transactions: usize,
    network_time_offset_ms: Option<i64>,
    bad_clock_peers: usize,
    last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BurnSettingsForm {
    enabled: Option<bool>,
    amount: Amount,
    fee_per_byte: Option<Amount>,
}

#[derive(Debug, Deserialize)]
struct PowMiningForm {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct MetricsSettingsForm {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct P2pAnnounceForm {
    addr: String,
}

#[derive(Debug, Deserialize)]
struct P2pInboundForm {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct TransferForm {
    to: String,
    amount: Amount,
    fee_per_byte: Option<Amount>,
    #[serde(default)]
    utxos: String,
}

#[derive(Debug, Deserialize)]
struct PeerForm {
    peer: String,
}

#[derive(Debug, Deserialize)]
struct ConfigForm {
    setup_complete: bool,
    #[serde(default)]
    peer: String,
}

#[derive(Debug, Deserialize)]
struct SeedPhraseForm {
    seed_phrase: String,
}

#[derive(Debug, Deserialize)]
struct BlocksQuery {
    before_height: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct WalletTransactionsQuery {
    tx: Option<bool>,
    mine: Option<bool>,
    burn: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl WalletTransactionsQuery {
    fn page(&self) -> PageQuery {
        PageQuery {
            offset: self.offset,
            limit: self.limit,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WalletTransactionFilters {
    transfer: bool,
    mine: bool,
    burn: bool,
}

impl Default for WalletTransactionFilters {
    fn default() -> Self {
        Self {
            transfer: true,
            mine: false,
            burn: false,
        }
    }
}

impl WalletTransactionFilters {
    fn from_query(query: WalletTransactionsQuery) -> Self {
        Self {
            transfer: query.tx.unwrap_or(true),
            mine: query.mine.unwrap_or(false),
            burn: query.burn.unwrap_or(false),
        }
    }

    fn allows(self, transaction: &Transaction) -> bool {
        match transaction {
            Transaction::Transfer { .. } => self.transfer,
            Transaction::Mine { .. } => self.mine,
            Transaction::Burn { .. } => self.burn,
        }
    }
}

#[derive(Debug, Serialize)]
struct ActionResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    items: Vec<T>,
    offset: usize,
    limit: usize,
    total: usize,
    has_more: bool,
    next_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsResponse {
    enabled: bool,
    latest: Option<BlockMetricRow>,
    charts: Vec<MetricsChart>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsChart {
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    value_kind: MetricsValueKind,
    points: Vec<MetricsPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsPoint {
    height: u64,
    value: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum MetricsValueKind {
    Number,
    Seconds,
    Iuna,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeeEstimateResponse {
    ok: bool,
    error: Option<String>,
    bytes: Option<usize>,
    fee: Option<Amount>,
}

#[derive(Debug, Serialize)]
struct WalletSetupResponse {
    ok: bool,
    error: Option<String>,
    address: Option<String>,
    seed_phrase: Option<String>,
    dev_verify_bypass: bool,
    requires_peer: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WalletTransactionRow {
    kind: &'static str,
    from: String,
    to: Option<String>,
    amount: Amount,
    fee: Amount,
    inputs: Vec<UiTxInput>,
    outputs: Vec<TxOutput>,
    change: Vec<TxOutput>,
    signature: String,
    status: &'static str,
    block_height: Option<u64>,
    block_finalizer: Option<String>,
    direction: &'static str,
    difficulty_bits: Option<u32>,
    proof_bits: Option<u32>,
    proof_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WalletUtxoRow {
    outpoint: OutPoint,
    address: String,
    amount: Amount,
    spendable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UiBlock {
    height: u64,
    prev_hash: String,
    timestamp_ms: u64,
    miner: String,
    finalizer_mode: crate::domain::FinalizerMode,
    finalizer_rank: u32,
    reward: Amount,
    total_fees: Amount,
    vdf_rounds: u64,
    vdf_output: String,
    leader_proof: Option<crate::domain::LeaderProof>,
    burn_leader_ranks: Vec<BurnLeaderRank>,
    transactions: Vec<UiTransaction>,
    revealed_transactions: Vec<UiTransaction>,
    hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UiTransaction {
    kind: &'static str,
    from: String,
    to: Option<String>,
    amount: Amount,
    fee: Amount,
    inputs: Vec<UiTxInput>,
    outputs: Vec<TxOutput>,
    change: Vec<TxOutput>,
    signature: String,
    difficulty_bits: Option<u32>,
    proof_bits: Option<u32>,
    proof_hash: Option<String>,
    commitment: Option<String>,
    encrypted_size: Option<u32>,
    expires_at_height: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UiTxInput {
    outpoint: OutPoint,
    owner: String,
    signature: String,
    amount: Option<Amount>,
    address: Option<String>,
}

pub async fn serve(
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    ui_config: Arc<Mutex<UiConfig>>,
    options: ServeOptions,
) -> Result<()> {
    let addr = options.addr;
    let state = HttpState {
        node,
        peers,
        gossip,
        ui_config,
        config_path: options.config_path,
        chain_store: options.chain_store,
        wallet_path: options.wallet_path,
        stratum: options.stratum,
        auth_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        auth_backoff: Arc::new(Mutex::new(BTreeMap::new())),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/assets/alpine.min.js", get(alpine_js))
        .route("/assets/iuna-ui.js", get(app_js))
        .route("/api/auth/status", get(api_auth_status))
        .route("/api/auth/setup", post(api_auth_setup_form))
        .route("/api/auth/login", post(api_auth_login_form))
        .route("/api/auth/logout", post(api_auth_logout_form))
        .route(
            "/api/auth/change-password",
            post(api_auth_change_password_form),
        )
        .route("/api/status", get(api_status))
        .route("/api/blocks", get(api_blocks))
        .route("/api/config", get(api_config).post(api_config_form))
        .route("/api/wallet/setup", get(api_wallet_setup))
        .route("/api/wallet/generate", post(api_wallet_generate_form))
        .route("/api/wallet/import", post(api_wallet_import_form))
        .route("/api/wallet/transactions", get(api_wallet_transactions))
        .route(
            "/api/wallet/utxos/selectable",
            get(api_wallet_selectable_utxos),
        )
        .route("/api/wallet/utxos", get(api_wallet_utxos))
        .route(
            "/api/fee-estimate/transfer",
            post(api_transfer_fee_estimate_form),
        )
        .route("/api/fee-estimate/burn", post(api_burn_fee_estimate_form))
        .route("/api/fee-estimate/mine", post(api_mine_fee_estimate_form))
        .route("/api/mempool", get(api_mempool))
        .route("/api/metrics", get(api_metrics))
        .route("/api/network/health", get(api_network_health))
        .route(
            "/api/peers",
            get(api_peers)
                .post(api_peer_form)
                .delete(api_peer_delete_form),
        )
        .route("/api/p2p/metrics", get(api_p2p_metrics))
        .route(
            "/api/settings/burn-per-block",
            post(api_burn_per_block_form),
        )
        .route("/api/settings/pow-mining", post(api_pow_mining_form))
        .route("/api/settings/metrics", post(api_metrics_settings_form))
        .route("/api/settings/p2p-inbound", post(api_p2p_inbound_form))
        .route("/api/settings/p2p-announce", post(api_p2p_announce_form))
        .route("/api/transfer", post(api_transfer_form))
        .route("/settings/burn-per-block", post(burn_per_block_form))
        .route("/transfer", post(transfer_form))
        .route("/peers", post(peer_form))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth_middleware,
        ))
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP management UI on {addr}"))?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("serving HTTP management UI")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn favicon() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "public, max-age=86400")],
    )
}

async fn alpine_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../www/assets/alpine.min.js"),
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../www/assets/iuna-ui.js"),
    )
}

async fn require_auth_middleware(
    State(state): State<HttpState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if csrf_required(request.method()) && !same_origin_request(&headers) {
        return csrf_error().into_response();
    }
    let client_key = auth_client_key(
        &headers,
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0),
    );
    request.extensions_mut().insert(AuthClientKey(client_key));
    if auth_exempt_path(&path) {
        return next.run(request).await;
    }
    let configured = state.ui_config.lock().await.auth_password_hash.is_some();
    if !configured {
        return auth_error("authentication setup is required").into_response();
    }
    if request_is_authenticated(&state, &headers).await {
        return next.run(request).await;
    }
    auth_error("authentication required").into_response()
}

fn auth_exempt_path(path: &str) -> bool {
    path == "/"
        || path == "/favicon.ico"
        || path == "/assets/alpine.min.js"
        || path == "/assets/iuna-ui.js"
        || path == "/api/auth/status"
        || path == "/api/auth/setup"
        || path == "/api/auth/login"
}

fn csrf_required(method: &Method) -> bool {
    !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS)
}

fn same_origin_request(headers: &HeaderMap) -> bool {
    let Some(request_host) = request_host(headers) else {
        return false;
    };
    let Some(origin_host) = origin_or_referer_host(headers) else {
        return false;
    };
    normalize_host(&origin_host) == normalize_host(&request_host)
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "x-forwarded-host").or_else(|| header_string(headers, "host"))
}

fn origin_or_referer_host(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "origin")
        .and_then(|origin| url_host(&origin))
        .or_else(|| header_string(headers, "referer").and_then(|referer| url_host(&referer)))
}

fn header_string(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn url_host(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("://")?;
    rest.split(['/', '?', '#'])
        .next()
        .map(str::trim)
        .filter(|authority| !authority.is_empty() && *authority != "null")
        .map(|authority| {
            authority
                .rsplit('@')
                .next()
                .unwrap_or(authority)
                .to_string()
        })
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

async fn request_is_authenticated(state: &HttpState, headers: &HeaderMap) -> bool {
    let Some(token) = auth_cookie(headers) else {
        return false;
    };
    let token_hash = session_token_hash(token);
    let now = now_ms();
    let mut sessions = state.auth_sessions.lock().await;
    sessions.retain(|_, session| session.expires_at > now);
    sessions
        .get(&token_hash)
        .is_some_and(|session| session.expires_at > now)
}

async fn wallet_password_for_request(state: &HttpState, headers: &HeaderMap) -> Option<String> {
    let token = auth_cookie(headers)?;
    let token_hash = session_token_hash(token);
    let now = now_ms();
    let mut sessions = state.auth_sessions.lock().await;
    sessions.retain(|_, session| session.expires_at > now);
    sessions
        .get(&token_hash)
        .filter(|session| session.expires_at > now)
        .map(|session| session.wallet_password.clone())
}

async fn api_auth_status(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<AuthStatusResponse> {
    let configured = state.ui_config.lock().await.auth_password_hash.is_some();
    let authenticated = configured && request_is_authenticated(&state, &headers).await;
    Json(AuthStatusResponse {
        configured,
        authenticated,
    })
}

async fn api_auth_setup_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    Form(form): Form<AuthForm>,
) -> Response {
    match setup_auth_password(&state, &form.password, &client_key.0).await {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
        Err(error) => action_json(Err(error)).into_response(),
    }
}

async fn api_auth_login_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    Form(form): Form<AuthForm>,
) -> Response {
    match login_auth_password(&state, &form.password, &client_key.0).await {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
        Err(error) => action_json(Err(error)).into_response(),
    }
}

async fn api_auth_logout_form(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Some(token) = auth_cookie(&headers) {
        state
            .auth_sessions
            .lock()
            .await
            .remove(&session_token_hash(token));
    }
    (
        [(
            header::SET_COOKIE,
            format!("{AUTH_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
        )],
        action_json(Ok(())),
    )
        .into_response()
}

async fn api_auth_change_password_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    match change_auth_password(
        &state,
        &form.old_password,
        &form.new_password,
        &client_key.0,
    )
    .await
    {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
        Err(error) => action_json(Err(error)).into_response(),
    }
}

async fn api_status(State(state): State<HttpState>) -> Json<NodeStatus> {
    let mut status = state.node.lock().await.status();
    status.stratum = state.stratum.clone();
    Json(status)
}

async fn api_blocks(
    State(state): State<HttpState>,
    Query(query): Query<BlocksQuery>,
) -> Json<Vec<UiBlock>> {
    let limit = query
        .limit
        .unwrap_or(EXPLORER_PAGE_LIMIT)
        .min(EXPLORER_LIMIT);
    let node = state.node.lock().await;
    let snapshot = node.chain_snapshot();
    let pending = node.pending_transactions();
    let blocks = match query.before_height {
        Some(before_height) => node.blocks_before(before_height, limit),
        None => node.recent_blocks(limit),
    };
    let burn_leader_ranks = blocks
        .iter()
        .map(|block| {
            (
                block.hash.clone(),
                node.burn_leader_ranks_for_block(block.height)
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    Json(ui_blocks(blocks, &snapshot, &pending, &burn_leader_ranks))
}

async fn api_config(State(state): State<HttpState>) -> Json<UiConfig> {
    Json(state.ui_config.lock().await.clone())
}

async fn api_wallet_setup(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(wallet_setup_response(&state, &headers).await)
}

async fn api_mempool(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<UiTransaction>> {
    let node = state.node.lock().await;
    let snapshot = node.chain_snapshot();
    let pending = node.pending_transactions();
    let pending_blinded = node.pending_blinded_transactions();
    let pending_reveals = node.pending_blinded_reveals();
    let outputs = known_output_index(&snapshot, &pending);
    let mut items = pending
        .iter()
        .map(|tx| ui_transaction(tx, &outputs))
        .collect::<Vec<_>>();
    items.extend(pending_blinded.iter().map(ui_blinded_transaction));
    items.extend(pending_reveals.iter().map(ui_blinded_reveal));
    Json(page_items(items, query))
}

async fn api_wallet_transactions(
    State(state): State<HttpState>,
    Query(query): Query<WalletTransactionsQuery>,
) -> Json<Page<WalletTransactionRow>> {
    let node = state.node.lock().await;
    let snapshot = node.chain_snapshot();
    let pending = node.pending_transactions();
    let outputs = known_output_index(&snapshot, &pending);
    let page_query = query.page();
    let filters = WalletTransactionFilters::from_query(query);
    Json(page_items(
        wallet_transaction_rows(
            node.wallet_address(),
            pending,
            &snapshot.blocks,
            &outputs,
            filters,
        ),
        page_query,
    ))
}

async fn api_wallet_utxos(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<WalletUtxoRow>> {
    let node = state.node.lock().await;
    Json(page_items(
        wallet_utxo_rows(node.ledger(), node.wallet_address()),
        query,
    ))
}

async fn api_wallet_selectable_utxos(State(state): State<HttpState>) -> Json<Vec<WalletUtxoRow>> {
    let node = state.node.lock().await;
    Json(selectable_wallet_utxo_rows(
        node.ledger(),
        node.wallet_address(),
    ))
}

fn page_items<T>(items: Vec<T>, query: PageQuery) -> Page<T> {
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

fn wallet_utxo_rows(ledger: &Ledger, wallet: &str) -> Vec<WalletUtxoRow> {
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

fn selectable_wallet_utxo_rows(ledger: &Ledger, wallet: &str) -> Vec<WalletUtxoRow> {
    wallet_utxo_rows(ledger, wallet)
        .into_iter()
        .filter(|utxo| utxo.spendable)
        .collect()
}

async fn api_peers(
    State(state): State<HttpState>,
    Query(query): Query<PageQuery>,
) -> Json<Page<PeerInfo>> {
    Json(page_items(state.peers.lock().await.list(), query))
}

async fn api_p2p_metrics(State(state): State<HttpState>) -> Json<P2pMetrics> {
    Json(state.gossip.metrics())
}

async fn api_metrics(State(state): State<HttpState>) -> Json<MetricsResponse> {
    let enabled = state.ui_config.lock().await.keep_track_of_metrics;
    if !enabled {
        return Json(MetricsResponse {
            enabled,
            latest: None,
            charts: Vec::new(),
        });
    }
    let store = state.chain_store.clone();
    let rows = tokio::task::spawn_blocking(move || store.load_metrics())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Json(metrics_response(enabled, rows))
}

async fn api_network_health(State(state): State<HttpState>) -> Json<NetworkHealthResponse> {
    let status = state.node.lock().await.status();
    let peers = state.peers.lock().await.list();
    Json(network_health(&status, &peers))
}

async fn api_config_form(
    State(state): State<HttpState>,
    Form(form): Form<ConfigForm>,
) -> Json<ActionResponse> {
    action_json(apply_config_form(&state, form).await)
}

async fn apply_config_form(state: &HttpState, form: ConfigForm) -> Result<()> {
    let peer = form.peer.trim();
    if !peer.is_empty() {
        add_peer(state, peer.to_string()).await?;
    }
    if form.setup_complete && setup_requires_peer(state).await {
        let has_peer = !state.peers.lock().await.addresses().is_empty();
        if !has_peer {
            bail!("add a bootstrap peer before completing setup");
        }
    }
    let mut config = state.ui_config.lock().await;
    config.setup_complete = form.setup_complete;
    config_store::save(&state.config_path, &config)
}

async fn api_wallet_generate_form(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(replace_setup_wallet_with_generated_seed(&state, &headers).await)
}

async fn api_wallet_import_form(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Form(form): Form<SeedPhraseForm>,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(import_setup_wallet_seed(&state, &headers, &form.seed_phrase).await)
}

async fn api_transfer_fee_estimate_form(
    State(state): State<HttpState>,
    Form(form): Form<TransferForm>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_transfer_fee(&state, form).await)
}

async fn api_burn_fee_estimate_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_burn_fee(&state, form).await)
}

async fn api_mine_fee_estimate_form(
    State(state): State<HttpState>,
    Form(_form): Form<BTreeMap<String, String>>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_mine_fee(&state).await)
}

async fn api_burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Json<ActionResponse> {
    let enabled = form.enabled.unwrap_or(form.amount > 0);
    let result = match required_fee_per_byte_burn(&form) {
        Ok(fee_per_byte) => set_burn_settings(&state, enabled, form.amount, fee_per_byte).await,
        Err(error) => Err(error),
    };
    action_json(result)
}

async fn api_pow_mining_form(
    State(state): State<HttpState>,
    Form(form): Form<PowMiningForm>,
) -> Json<ActionResponse> {
    action_json(set_pow_mining(&state, form.enabled).await)
}

async fn api_metrics_settings_form(
    State(state): State<HttpState>,
    Form(form): Form<MetricsSettingsForm>,
) -> Json<ActionResponse> {
    action_json(set_keep_track_of_metrics(&state, form.enabled).await)
}

async fn api_p2p_announce_form(
    State(state): State<HttpState>,
    Form(form): Form<P2pAnnounceForm>,
) -> Json<ActionResponse> {
    action_json(set_p2p_announce_addr(&state, form.addr).await)
}

async fn api_p2p_inbound_form(
    State(state): State<HttpState>,
    Form(form): Form<P2pInboundForm>,
) -> Json<ActionResponse> {
    action_json(set_p2p_accept_inbound(&state, form.enabled).await)
}

async fn burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Response {
    let enabled = form.enabled.unwrap_or(form.amount > 0);
    let result = match required_fee_per_byte_burn(&form) {
        Ok(fee_per_byte) => set_burn_settings(&state, enabled, form.amount, fee_per_byte).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(_) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

async fn api_transfer_form(
    State(state): State<HttpState>,
    Form(form): Form<TransferForm>,
) -> Json<ActionResponse> {
    let result = transfer(&state, form).await;
    action_json(result)
}

async fn transfer_form(State(state): State<HttpState>, Form(form): Form<TransferForm>) -> Response {
    match transfer(&state, form).await {
        Ok(_) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

async fn api_peer_form(
    State(state): State<HttpState>,
    Form(form): Form<PeerForm>,
) -> Json<ActionResponse> {
    let result = add_peer(&state, form.peer).await;
    action_json(result)
}

async fn api_peer_delete_form(
    State(state): State<HttpState>,
    Form(form): Form<PeerForm>,
) -> Json<ActionResponse> {
    let result = remove_peer(&state, form.peer).await;
    action_json(result)
}

async fn peer_form(State(state): State<HttpState>, Form(form): Form<PeerForm>) -> Response {
    match add_peer(&state, form.peer).await {
        Ok(()) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

async fn set_burn_settings(
    state: &HttpState,
    enabled: bool,
    amount: Amount,
    fee: Amount,
) -> Result<()> {
    if enabled && amount == 0 {
        bail!("IUNA per block must be greater than zero when finalization burns are on");
    }
    let result = {
        let mut node = state.node.lock().await;
        let result = node.set_automatic_burn_settings(enabled, amount, fee);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => {
            persist_burn_settings_config(
                &state.ui_config,
                &state.config_path,
                enabled,
                amount,
                fee,
            )
            .await?;
            state.gossip.broadcast(result.1).await
        }
        Err(error) => Err(error),
    }
}

async fn persist_burn_settings_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    enabled: bool,
    amount: Amount,
    fee: Amount,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.mining_enabled = enabled;
    config.burn_per_block = amount;
    config.burn_fee = fee;
    config_store::save(config_path, &config)
}

async fn set_pow_mining(state: &HttpState, enabled: bool) -> Result<()> {
    {
        let mut node = state.node.lock().await;
        node.set_pow_mining_enabled(enabled);
    }
    persist_pow_mining_config(&state.ui_config, &state.config_path, enabled).await
}

async fn persist_pow_mining_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    enabled: bool,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.pow_mining_enabled = enabled;
    config_store::save(config_path, &config)
}

async fn set_keep_track_of_metrics(state: &HttpState, enabled: bool) -> Result<()> {
    if enabled {
        let snapshot = {
            let node = state.node.lock().await;
            node.has_real_chain().then(|| node.chain_snapshot())
        };
        if let Some(snapshot) = snapshot {
            replace_metrics_for_snapshot(&state.chain_store, snapshot).await?;
        } else {
            clear_metrics(&state.chain_store).await?;
        }
    } else {
        clear_metrics(&state.chain_store).await?;
    }

    let mut config = state.ui_config.lock().await;
    config.keep_track_of_metrics = enabled;
    config_store::save(&state.config_path, &config)
}

async fn set_p2p_announce_addr(state: &HttpState, addr: String) -> Result<()> {
    let trimmed = addr.trim();
    let parsed = if trimmed.is_empty() {
        None
    } else {
        Some(
            trimmed
                .parse::<SocketAddr>()
                .with_context(|| format!("invalid P2P announce address {trimmed}"))?,
        )
    };

    let mut config = state.ui_config.lock().await;
    let mut next_config = config.clone();
    next_config.p2p_announce_addr = parsed.map(|addr| addr.to_string());
    config_store::save(&state.config_path, &next_config)?;
    *config = next_config;
    drop(config);
    state.gossip.set_p2p_announce_addr(parsed).await;
    Ok(())
}

async fn set_p2p_accept_inbound(state: &HttpState, enabled: bool) -> Result<()> {
    let previous = state.gossip.accepts_inbound().await;
    if enabled {
        state.gossip.set_accept_inbound(true).await?;
    }

    let mut config = state.ui_config.lock().await;
    let mut next_config = config.clone();
    next_config.p2p_accept_inbound = enabled;
    if let Err(error) = config_store::save(&state.config_path, &next_config) {
        let _ = state.gossip.set_accept_inbound(previous).await;
        return Err(error);
    }
    *config = next_config;
    drop(config);

    if !enabled {
        state.gossip.set_accept_inbound(false).await?;
    }

    Ok(())
}

async fn replace_metrics_for_snapshot(
    store: &SqliteChainStore,
    snapshot: crate::domain::ChainSnapshot,
) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.replace_metrics_for_snapshot(&snapshot))
        .await
        .context("metrics worker failed")??;
    Ok(())
}

async fn clear_metrics(store: &SqliteChainStore) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.clear_metrics())
        .await
        .context("metrics cleanup worker failed")??;
    Ok(())
}

async fn add_peer(state: &HttpState, peer: String) -> Result<()> {
    let peer = validate_peer_address(peer)?;
    let addresses = {
        let mut peers = state.peers.lock().await;
        peers.add_peer(peer);
        peers.addresses()
    };
    let mut config = state.ui_config.lock().await;
    config.peers = addresses;
    config_store::save(&state.config_path, &config)
}

async fn remove_peer(state: &HttpState, peer: String) -> Result<()> {
    let peer = validate_peer_address(peer)?;
    let addresses = {
        let mut peers = state.peers.lock().await;
        if !peers.remove_peer(&peer) {
            bail!("peer is not configured as an outbound peer");
        }
        peers.addresses()
    };
    let mut config = state.ui_config.lock().await;
    config.peers = addresses;
    config_store::save(&state.config_path, &config)
}

fn validate_peer_address(peer: String) -> Result<String> {
    let peer = peer.trim().to_string();
    if peer.is_empty() {
        bail!("peer address is required");
    }
    Ok(peer)
}

fn network_health(status: &NodeStatus, peers: &[PeerInfo]) -> NetworkHealthResponse {
    network_health_at(status, peers, now_ms())
}

fn metrics_response(enabled: bool, rows: Vec<BlockMetricRow>) -> MetricsResponse {
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

fn network_health_at(
    status: &NodeStatus,
    peers: &[PeerInfo],
    now_ms: u64,
) -> NetworkHealthResponse {
    let local_height = status.chain.height;
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
        pending_transactions: status.chain.pending_transactions,
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

async fn wallet_setup_response(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<WalletSetupResponse> {
    let setup_complete = state.ui_config.lock().await.setup_complete;
    let password = wallet_password_for_request(state, headers).await;
    let seed_phrase = if setup_complete {
        None
    } else {
        wallet_store::setup_seed_phrase_with_password(&state.wallet_path, password.as_deref())?
    };
    let address = state.node.lock().await.wallet_address().to_string();
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

async fn setup_requires_peer(state: &HttpState) -> bool {
    !state.node.lock().await.has_real_chain()
}

fn wallet_transaction_rows(
    wallet: &str,
    pending: Vec<Transaction>,
    chain: &[Block],
    outputs: &BTreeMap<OutPoint, TxOutput>,
    filters: WalletTransactionFilters,
) -> Vec<WalletTransactionRow> {
    let mut rows = Vec::new();

    for (index, tx) in pending.iter().enumerate() {
        if !filters.allows(tx) {
            continue;
        }
        if let Some(row) = wallet_transaction_row(wallet, tx, outputs, "pending", None, None) {
            rows.push((u128::MAX - index as u128, row));
        }
    }

    for block in chain {
        for (index, tx) in block.transactions.iter().rev().enumerate() {
            if !filters.allows(tx) {
                continue;
            }
            if let Some(row) = wallet_transaction_row(
                wallet,
                tx,
                outputs,
                "confirmed",
                Some(block.height),
                Some(block.miner.clone()),
            ) {
                rows.push((block.height as u128 * 10_000 + index as u128, row));
            }
        }
    }

    rows.sort_by(|left, right| right.0.cmp(&left.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

fn wallet_transaction_row(
    wallet: &str,
    tx: &Transaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
    status: &'static str,
    block_height: Option<u64>,
    block_finalizer: Option<String>,
) -> Option<WalletTransactionRow> {
    match tx {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } if tx.sender() == wallet || tx.to() == Some(wallet) => Some(WalletTransactionRow {
            kind: "transfer",
            from: tx.sender().to_string(),
            to: tx.to().map(str::to_string),
            amount: tx.amount(),
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: outputs.clone(),
            change: Vec::new(),
            signature: signature.clone(),
            status,
            block_height,
            block_finalizer,
            direction: if tx.to() == Some(wallet) {
                "received"
            } else {
                "sent"
            },
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } if tx.sender() == wallet => Some(WalletTransactionRow {
            kind: "burn",
            from: tx.sender().to_string(),
            to: None,
            amount: *amount,
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: Vec::new(),
            change: change.clone(),
            signature: signature.clone(),
            status,
            block_height,
            block_finalizer,
            direction: "burned",
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
        }),
        Transaction::Mine {
            recipient,
            difficulty_bits,
            signature,
            ..
        } if recipient == wallet => Some(WalletTransactionRow {
            kind: "mine",
            from: "pow".to_string(),
            to: Some(recipient.clone()),
            amount: MINE_REWARD,
            fee: tx.fee(),
            inputs: Vec::new(),
            outputs: vec![TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            }],
            change: Vec::new(),
            signature: signature.clone(),
            status,
            block_height,
            block_finalizer,
            direction: "received",
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
        }),
        _ => None,
    }
}

fn ui_blocks(
    blocks: Vec<Block>,
    snapshot: &ChainSnapshot,
    pending: &[Transaction],
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
) -> Vec<UiBlock> {
    let outputs = known_output_index(snapshot, pending);
    let revealed = revealed_blinded_transactions(snapshot)
        .unwrap_or_default()
        .into_iter()
        .fold(
            BTreeMap::<u64, Vec<Transaction>>::new(),
            |mut by_height, revealed| {
                by_height
                    .entry(revealed.height)
                    .or_default()
                    .push(revealed.transaction);
                by_height
            },
        );
    blocks
        .into_iter()
        .map(|block| {
            let revealed_transactions = revealed.get(&block.height).cloned().unwrap_or_default();
            ui_block(block, &outputs, burn_leader_ranks, &revealed_transactions)
        })
        .collect()
}

fn ui_block(
    block: Block,
    outputs: &BTreeMap<OutPoint, TxOutput>,
    burn_leader_ranks: &BTreeMap<String, Vec<BurnLeaderRank>>,
    revealed_transactions: &[Transaction],
) -> UiBlock {
    let ranks = burn_leader_ranks
        .get(&block.hash)
        .cloned()
        .unwrap_or_default();
    let revealed_fees = revealed_transactions
        .iter()
        .fold(0_u64, |total, tx| total.saturating_add(tx.fee()));
    UiBlock {
        height: block.height,
        prev_hash: block.prev_hash,
        timestamp_ms: block.timestamp_ms,
        miner: block.miner,
        finalizer_mode: block.finalizer_mode,
        finalizer_rank: block.finalizer_rank,
        reward: block.reward,
        total_fees: block.reward.saturating_add(revealed_fees),
        vdf_rounds: block.vdf_rounds,
        vdf_output: block.vdf_output,
        leader_proof: block.leader_proof,
        burn_leader_ranks: ranks,
        transactions: block
            .transactions
            .iter()
            .map(|tx| ui_transaction(tx, outputs))
            .collect(),
        revealed_transactions: revealed_transactions
            .iter()
            .map(|tx| ui_transaction(tx, outputs))
            .collect(),
        hash: block.hash,
    }
}

fn ui_transaction(
    transaction: &Transaction,
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> UiTransaction {
    match transaction {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } => UiTransaction {
            kind: "transfer",
            from: transaction.sender().to_string(),
            to: transaction.to().map(str::to_string),
            amount: transaction.amount(),
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: outputs.clone(),
            change: Vec::new(),
            signature: signature.clone(),
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
        },
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } => UiTransaction {
            kind: "burn",
            from: transaction.sender().to_string(),
            to: None,
            amount: *amount,
            fee: *fee,
            inputs: ui_inputs(inputs, outputs_by_outpoint),
            outputs: Vec::new(),
            change: change.clone(),
            signature: signature.clone(),
            difficulty_bits: None,
            proof_bits: None,
            proof_hash: None,
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
        },
        Transaction::Mine {
            recipient,
            difficulty_bits,
            signature,
            ..
        } => UiTransaction {
            kind: "mine",
            from: "pow".to_string(),
            to: Some(recipient.clone()),
            amount: MINE_REWARD,
            fee: transaction.fee(),
            inputs: Vec::new(),
            outputs: vec![TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            }],
            change: Vec::new(),
            signature: signature.clone(),
            difficulty_bits: Some(*difficulty_bits),
            proof_bits: Some(proof_bits(signature)),
            proof_hash: Some(signature.clone()),
            commitment: None,
            encrypted_size: None,
            expires_at_height: None,
        },
    }
}

fn ui_blinded_transaction(transaction: &BlindedTransaction) -> UiTransaction {
    UiTransaction {
        kind: "blinded",
        from: "encrypted".to_string(),
        to: None,
        amount: 0,
        fee: transaction.fee,
        inputs: Vec::new(),
        outputs: Vec::new(),
        change: Vec::new(),
        signature: transaction.commitment.clone(),
        difficulty_bits: None,
        proof_bits: None,
        proof_hash: None,
        commitment: Some(transaction.commitment.clone()),
        encrypted_size: Some(transaction.encrypted_size),
        expires_at_height: Some(transaction.expires_at_height),
    }
}

fn ui_blinded_reveal(reveal: &BlindedReveal) -> UiTransaction {
    UiTransaction {
        kind: "reveal",
        from: "encrypted".to_string(),
        to: None,
        amount: 0,
        fee: 0,
        inputs: Vec::new(),
        outputs: Vec::new(),
        change: Vec::new(),
        signature: reveal.commitment.clone(),
        difficulty_bits: None,
        proof_bits: None,
        proof_hash: None,
        commitment: Some(reveal.commitment.clone()),
        encrypted_size: None,
        expires_at_height: None,
    }
}

fn ui_inputs(
    inputs: &[TxInput],
    outputs_by_outpoint: &BTreeMap<OutPoint, TxOutput>,
) -> Vec<UiTxInput> {
    inputs
        .iter()
        .map(|input| {
            let spent_output = outputs_by_outpoint.get(&input.outpoint);
            UiTxInput {
                outpoint: input.outpoint.clone(),
                owner: input.owner.clone(),
                signature: input.signature.clone(),
                amount: spent_output.map(|output| output.amount),
                address: spent_output.map(|output| output.address.clone()),
            }
        })
        .collect()
}

fn proof_bits(hex_hash: &str) -> u32 {
    let mut bits = 0_u32;
    for byte in hex_hash.as_bytes() {
        let Some(nibble) = hex_nibble(*byte) else {
            break;
        };
        if nibble == 0 {
            bits += 4;
            continue;
        }
        bits += nibble.leading_zeros() - 4;
        break;
    }
    bits
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn known_output_index(
    snapshot: &ChainSnapshot,
    pending: &[Transaction],
) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = BTreeMap::new();
    for (address, amount) in &snapshot.genesis_allocations {
        if *amount == 0 {
            continue;
        }
        outputs.insert(
            genesis_allocation_outpoint(address),
            TxOutput {
                address: address.clone(),
                amount: *amount,
            },
        );
    }
    let revealed = revealed_blinded_transactions(snapshot).unwrap_or_default();
    for block in &snapshot.blocks {
        for transaction in &block.transactions {
            index_transaction_outputs(&mut outputs, transaction);
        }
        if block.reward > 0 {
            outputs.insert(
                reward_outpoint(&block.hash),
                TxOutput {
                    address: block.miner.clone(),
                    amount: block.reward,
                },
            );
        }
    }
    for revealed in revealed {
        index_transaction_outputs(&mut outputs, &revealed.transaction);
        if revealed.transaction.fee() > 0 {
            outputs.insert(
                blinded_fee_outpoint(&revealed.commitment),
                TxOutput {
                    address: revealed.included_by,
                    amount: revealed.transaction.fee(),
                },
            );
        }
    }
    for transaction in pending {
        index_transaction_outputs(&mut outputs, transaction);
    }
    outputs
}

fn index_transaction_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &Transaction,
) {
    let created_outputs = match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        }],
    };
    for (index, output) in created_outputs.iter().enumerate() {
        outputs.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
}

fn genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
}

fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
}

fn blinded_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

async fn replace_setup_wallet_with_generated_seed(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let password = wallet_password_for_request(state, headers)
        .await
        .context("wallet password session is required")?;
    let (wallet, seed_phrase) =
        wallet_store::replace_with_generated_seed_phrase_encrypted(&state.wallet_path, &password)?;
    let address = wallet.address().to_string();
    state.node.lock().await.replace_wallet(wallet);
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: Some(seed_phrase),
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

async fn import_setup_wallet_seed(
    state: &HttpState,
    headers: &HeaderMap,
    seed_phrase: &str,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let password = wallet_password_for_request(state, headers)
        .await
        .context("wallet password session is required")?;
    let wallet = wallet_store::replace_with_imported_seed_phrase_encrypted(
        &state.wallet_path,
        seed_phrase,
        &password,
    )?;
    let address = wallet.address().to_string();
    state.node.lock().await.replace_wallet(wallet);
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: None,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

async fn ensure_wallet_setup_open(state: &HttpState) -> Result<()> {
    let setup_complete = state.ui_config.lock().await.setup_complete;
    if setup_complete {
        bail!("wallet setup is already complete");
    }
    Ok(())
}

fn wallet_setup_json(result: Result<WalletSetupResponse>) -> Json<WalletSetupResponse> {
    match result {
        Ok(response) => Json(response),
        Err(error) => Json(WalletSetupResponse {
            ok: false,
            error: Some(format!("{error:#}")),
            address: None,
            seed_phrase: None,
            dev_verify_bypass: dev_seed_verify_bypass_enabled(),
            requires_peer: false,
        }),
    }
}

fn dev_seed_verify_bypass_enabled() -> bool {
    dev_seed_verify_bypass_allowed(std::env::var_os("IUNA_DEV_SKIP_SEED_VERIFY").is_some())
}

fn dev_seed_verify_bypass_allowed(env_present: bool) -> bool {
    env_present
}

async fn transfer(state: &HttpState, form: TransferForm) -> Result<()> {
    let (to, amount, fee_per_byte, selected_utxos) = validate_transfer_form(form)?;

    let result = {
        let mut node = state.node.lock().await;
        let result = node.transfer_with_fee_rate(to, amount, fee_per_byte, &selected_utxos);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => state.gossip.broadcast(result.1).await,
        Err(error) => Err(error),
    }
}

fn validate_transfer_form(form: TransferForm) -> Result<(String, Amount, Amount, Vec<OutPoint>)> {
    let to = form.to.trim();
    if to.is_empty() {
        bail!("recipient is required");
    }
    if form.amount == 0 {
        bail!("amount must be greater than zero");
    }
    let fee = required_fee_per_byte_transfer(&form)?;
    let selected_utxos = form
        .utxos
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|value| !value.trim().is_empty())
        .map(parse_outpoint)
        .collect::<Result<Vec<_>>>()?;
    Ok((to.to_string(), form.amount, fee, selected_utxos))
}

async fn estimate_transfer_fee(state: &HttpState, form: TransferForm) -> Result<FeeEstimate> {
    let (to, amount, fee_per_byte, selected_utxos) = validate_transfer_form(form)?;
    state
        .node
        .lock()
        .await
        .estimate_transfer_fee(to, amount, fee_per_byte, &selected_utxos)
}

async fn estimate_burn_fee(state: &HttpState, form: BurnSettingsForm) -> Result<FeeEstimate> {
    let fee_per_byte = required_fee_per_byte_burn(&form)?;
    if form.amount == 0 {
        bail!("amount must be greater than zero");
    }
    state
        .node
        .lock()
        .await
        .estimate_burn_fee(form.amount, fee_per_byte)
}

async fn estimate_mine_fee(state: &HttpState) -> Result<FeeEstimate> {
    state
        .node
        .lock()
        .await
        .estimate_mine_fee(MINE_FINALIZER_FEE)
}

fn required_fee_per_byte_transfer(form: &TransferForm) -> Result<Amount> {
    form.fee_per_byte.context("fee per byte is required")
}

fn required_fee_per_byte_burn(form: &BurnSettingsForm) -> Result<Amount> {
    form.fee_per_byte.context("fee per byte is required")
}

fn fee_estimate_json(result: Result<FeeEstimate>) -> Json<FeeEstimateResponse> {
    match result {
        Ok(estimate) => Json(FeeEstimateResponse {
            ok: true,
            error: None,
            bytes: Some(estimate.bytes),
            fee: Some(estimate.fee),
        }),
        Err(error) => Json(FeeEstimateResponse {
            ok: false,
            error: Some(format!("{error:#}")),
            bytes: None,
            fee: None,
        }),
    }
}

fn parse_outpoint(value: &str) -> Result<OutPoint> {
    let (txid, index) = value
        .rsplit_once(':')
        .with_context(|| format!("invalid UTXO reference {value}"))?;
    if txid.is_empty() {
        bail!("invalid UTXO reference {value}");
    }
    Ok(OutPoint {
        txid: txid.to_string(),
        index: index
            .parse::<u32>()
            .with_context(|| format!("invalid UTXO reference {value}"))?,
    })
}

fn action_json(result: Result<()>) -> Json<ActionResponse> {
    match result {
        Ok(_) => Json(ActionResponse {
            ok: true,
            error: None,
        }),
        Err(error) => Json(ActionResponse {
            ok: false,
            error: Some(format!("{error:#}")),
        }),
    }
}

fn auth_error(message: &str) -> (StatusCode, Json<ActionResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ActionResponse {
            ok: false,
            error: Some(message.to_string()),
        }),
    )
}

fn csrf_error() -> (StatusCode, Json<ActionResponse>) {
    (
        StatusCode::FORBIDDEN,
        Json(ActionResponse {
            ok: false,
            error: Some("same-origin request required".to_string()),
        }),
    )
}

fn api_error(error: anyhow::Error) -> Json<ActionResponse> {
    Json(ActionResponse {
        ok: false,
        error: Some(format!("{error:#}")),
    })
}

fn auth_client_key(headers: &HeaderMap, socket_addr: Option<SocketAddr>) -> String {
    if let Some(addr) = socket_addr {
        if !trusted_forwarding_peer(addr.ip()) {
            return addr.ip().to_string();
        }
    }
    forwarded_for_client(headers)
        .or_else(|| header_string(headers, "x-real-ip"))
        .or_else(|| forwarded_header_client(headers))
        .or_else(|| socket_addr.map(|addr| addr.ip().to_string()))
        .unwrap_or_else(|| UNKNOWN_CLIENT_KEY.to_string())
}

fn trusted_forwarding_peer(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ipv6_is_unique_local(ip) || ipv6_is_unicast_link_local(ip)
        }
    }
}

fn ipv6_is_unique_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn ipv6_is_unicast_link_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn forwarded_for_client(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "x-forwarded-for").and_then(|value| {
        value
            .split(',')
            .next()
            .map(str::trim)
            .filter(|client| !client.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn forwarded_header_client(headers: &HeaderMap) -> Option<String> {
    let value = header_string(headers, "forwarded")?;
    for item in value.split(';') {
        let Some((name, value)) = item.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("for") {
            return Some(
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('[')
                    .trim_matches(']')
                    .to_string(),
            )
            .filter(|client| !client.is_empty());
        }
    }
    None
}

async fn setup_auth_password(
    state: &HttpState,
    password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    if let Err(error) = validate_password(password) {
        record_auth_failure(state, client_key).await;
        return Err(error);
    }
    let mut config = state.ui_config.lock().await;
    if config.auth_password_hash.is_some() {
        record_auth_failure(state, client_key).await;
        bail!("authentication is already configured");
    }
    config.auth_password_hash = Some(hash_password(password)?);
    config_store::save(&state.config_path, &config)?;
    drop(config);
    wallet_store::encrypt_existing_with_password(&state.wallet_path, password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, password)?;
    state.node.lock().await.replace_wallet(wallet);
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

async fn login_auth_password(
    state: &HttpState,
    password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    let hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(password, &hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid password");
    }
    wallet_store::encrypt_existing_with_password(&state.wallet_path, password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, password)?;
    state.node.lock().await.replace_wallet(wallet);
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

async fn change_auth_password(
    state: &HttpState,
    old_password: &str,
    new_password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    validate_password(new_password)?;
    let current_hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(old_password, &current_hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid current password");
    }
    let wallet =
        wallet_store::reencrypt_with_password(&state.wallet_path, old_password, new_password)?;
    {
        let mut config = state.ui_config.lock().await;
        config.auth_password_hash = Some(hash_password(new_password)?);
        config_store::save(&state.config_path, &config)?;
    }
    state.node.lock().await.replace_wallet(wallet);
    state.auth_sessions.lock().await.clear();
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, new_password).await
}

async fn check_auth_backoff(state: &HttpState, client_key: &str) -> Result<()> {
    let now = now_ms();
    let mut backoffs = state.auth_backoff.lock().await;
    let backoff = backoffs.entry(client_key.to_string()).or_default();
    if backoff
        .locked_until_ms
        .is_some_and(|locked_until| locked_until > now)
    {
        bail!("too many failed login attempts; try again later");
    }
    if backoff.locked_until_ms.is_some() {
        backoff.locked_until_ms = None;
        backoff.failed_attempts = 0;
    }
    Ok(())
}

async fn record_auth_failure(state: &HttpState, client_key: &str) {
    let mut backoffs = state.auth_backoff.lock().await;
    let backoff = backoffs.entry(client_key.to_string()).or_default();
    backoff.failed_attempts = backoff.failed_attempts.saturating_add(1);
    if backoff.failed_attempts >= AUTH_MAX_FAILED_ATTEMPTS {
        backoff.locked_until_ms = Some(now_ms().saturating_add(AUTH_LOCKOUT_MS));
    }
}

async fn clear_auth_backoff(state: &HttpState, client_key: &str) {
    state.auth_backoff.lock().await.remove(client_key);
}

fn validate_password(password: &str) -> Result<()> {
    if password.len() < 12 {
        bail!("password must be at least 12 characters");
    }
    if password.len() > 1024 {
        bail!("password is too long");
    }
    Ok(())
}

async fn create_session_cookie(state: &HttpState, password: &str) -> Result<String> {
    let token = random_hex(32)?;
    let token_hash = session_token_hash(&token);
    let expires_at = now_ms().saturating_add(AUTH_SESSION_TTL_MS);
    state.auth_sessions.lock().await.insert(
        token_hash,
        AuthSession {
            expires_at,
            wallet_password: password.to_string(),
        },
    );
    Ok(format!(
        "{AUTH_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        AUTH_SESSION_TTL_MS / 1000
    ))
}

fn session_token_hash(token: &str) -> String {
    hex_encode(Sha256::digest(format!("iuna-session:{token}").as_bytes()))
}

fn auth_cookie(headers: &HeaderMap) -> Option<&str> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == AUTH_COOKIE_NAME).then_some(value)
    })
}

fn hash_password(password: &str) -> Result<String> {
    let salt = random_bytes::<16>()?;
    let hash = pbkdf2_sha256(password.as_bytes(), &salt, PASSWORD_KDF_ITERATIONS);
    Ok(format!(
        "{PASSWORD_KDF_ALGORITHM}${PASSWORD_KDF_ITERATIONS}${}${}",
        hex_encode(salt),
        hex_encode(hash)
    ))
}

fn verify_password(password: &str, encoded: &str) -> Result<bool> {
    let parts = encoded.split('$').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != PASSWORD_KDF_ALGORITHM {
        bail!("unsupported password hash");
    }
    let iterations = parts[1]
        .parse::<u32>()
        .context("invalid password hash iterations")?;
    let salt = decode_hex(parts[2]).context("invalid password hash salt")?;
    let expected = decode_hex(parts[3]).context("invalid password hash")?;
    let actual = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
    Ok(constant_time_eq(&actual, &expected))
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut block_salt = Vec::with_capacity(salt.len() + 4);
    block_salt.extend_from_slice(salt);
    block_salt.extend_from_slice(&1_u32.to_be_bytes());
    let hmac = HmacSha256Key::new(password);
    let mut u = hmac.digest(&block_salt);
    let mut output = u;
    for _ in 1..iterations {
        u = hmac.digest(&u);
        for (left, right) in output.iter_mut().zip(u) {
            *left ^= right;
        }
    }
    output
}

struct HmacSha256Key {
    outer_key_pad: [u8; 64],
    inner_key_pad: [u8; 64],
}

impl HmacSha256Key {
    fn new(key: &[u8]) -> Self {
        let mut key_block = [0_u8; 64];
        if key.len() > 64 {
            key_block[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            key_block[..key.len()].copy_from_slice(key);
        }

        let mut outer_key_pad = [0x5c_u8; 64];
        let mut inner_key_pad = [0x36_u8; 64];
        for index in 0..64 {
            outer_key_pad[index] ^= key_block[index];
            inner_key_pad[index] ^= key_block[index];
        }
        Self {
            outer_key_pad,
            inner_key_pad,
        }
    }

    fn digest(&self, message: &[u8]) -> [u8; 32] {
        let mut inner = Sha256::new();
        inner.update(self.inner_key_pad);
        inner.update(message);
        let inner_hash = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(self.outer_key_pad);
        outer.update(inner_hash);
        outer.finalize().into()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("secure random generation failed: {error}"))?;
    Ok(bytes)
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut value = vec![0_u8; bytes];
    getrandom(&mut value)
        .map_err(|error| anyhow::anyhow!("secure random generation failed: {error}"))?;
    Ok(hex_encode(value))
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn decode_hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="data:,">
  <title>iuna</title>
  <style>
    [x-cloak] { display: none !important; }
    .visually-hidden { position: absolute !important; width: 1px !important; height: 1px !important; overflow: hidden !important; clip: rect(0 0 0 0) !important; clip-path: inset(50%) !important; white-space: nowrap !important; }
    :root {
      color-scheme: dark;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #0f1012;
      color: #e8edf0;
    }
    * { box-sizing: border-box; }
    html { width: 100%; max-width: 100%; overflow-x: hidden; }
    body { margin: 0; min-height: 100vh; max-width: 100%; overflow-x: hidden; background: #0f1012; color: #e8edf0; }
    .app-shell { width: 100%; max-width: 100%; min-height: 100vh; display: block; overflow-x: hidden; }
    .sidebar { position: fixed; z-index: 5; inset: 0 auto 0 0; width: 84px; height: 100vh; display: flex; flex-direction: column; align-items: center; gap: 20px; padding: 16px 10px; background: #15171a; border-right: 1px solid #262b2f; }
    .brand-mark { position: relative; width: 38px; height: 38px; display: grid; place-items: center; overflow: hidden; border: 1px solid #e8ff8d; border-radius: 8px; background: linear-gradient(145deg, #ecff8a 0%, #d5f55f 54%, #8de9cd 100%); box-shadow: inset 0 1px 0 rgba(255, 255, 255, .42), 0 10px 24px rgba(213, 245, 95, .16); user-select: none; cursor: default; }
    .brand-mark::after { content: ""; position: absolute; inset: -40% -70%; background: linear-gradient(100deg, transparent 42%, rgba(255, 255, 255, .34) 50%, transparent 58%); transform: translateX(-58%) rotate(8deg); opacity: 0; pointer-events: none; }
    .brand-mark svg { position: relative; z-index: 1; width: 24px; height: 24px; display: block; }
    .brand-mark .mark-loop { fill: none; stroke: #101315; stroke-width: 4.2; stroke-linecap: round; stroke-linejoin: round; }
    .brand-mark .mark-dot { fill: #101315; }
    .brand-mark:hover::after { animation: mark-sheen .72s ease both; }
    @keyframes mark-sheen { from { opacity: 0; transform: translateX(-58%) rotate(8deg); } 32% { opacity: 1; } to { opacity: 0; transform: translateX(58%) rotate(8deg); } }
    .side-nav { display: grid; gap: 10px; width: 100%; }
    .nav-button { width: 64px; min-height: 58px; display: grid; place-items: center; gap: 4px; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; background: transparent; color: #9fa8ad; }
    .nav-button svg { width: 21px; height: 21px; stroke: currentColor; stroke-width: 2; fill: none; }
    .nav-button svg.chain-icon { stroke-width: 1.35; }
    .nav-button span { font-size: 11px; font-weight: 800; }
    .nav-button:hover, .nav-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .settings-button { margin-top: auto; width: 64px; min-height: 54px; display: grid; place-items: center; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #9fa8ad; background: transparent; text-align: center; }
    .settings-button svg { width: 23px; height: 23px; stroke: currentColor; stroke-width: 1.9; fill: none; }
    .settings-button:hover, .settings-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .version-panel { width: 64px; display: grid; gap: 4px; justify-items: center; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #7f888e; background: transparent; font-size: 10px; font-weight: 850; text-align: center; }
    .version-panel.update { border-color: #566d25; color: #d5f55f; background: #1c2516; cursor: pointer; }
    .version-panel.checking { color: #a8b2b8; }
    .version-panel.failed { color: #ffb1a8; }
    .version-dot { width: 6px; height: 6px; border-radius: 999px; background: #3a4248; }
    .version-panel.update .version-dot { background: #d5f55f; box-shadow: 0 0 0 3px rgba(213, 245, 95, .12); }
    .version-panel.failed .version-dot { background: #ff8f82; }
    .version-label { line-height: 1; }
    .version-update { color: #d5f55f; font-size: 9px; line-height: 1; text-transform: uppercase; }
    .content { width: 100%; min-width: 0; overflow-x: hidden; padding: 22px 24px 48px 108px; }
    main { width: 100%; }
    main > section { width: 100%; }
    header { display: flex; justify-content: space-between; gap: 18px; align-items: flex-start; padding: 0 0 18px; }
    .header-actions { display: flex; gap: 10px; align-items: center; }
    .basic-status-row { display: inline-flex; gap: 8px; align-items: center; margin-top: 5px; }
    .basic-status { display: inline-flex; gap: 7px; align-items: center; border: 1px solid transparent; border-radius: 999px; padding: 3px 7px; color: #9eb3bc; font-size: 12px; font-weight: 800; }
    .basic-status::before { content: ""; width: 6px; height: 6px; flex: 0 0 auto; border-radius: 999px; background: #7f888e; }
    .basic-status.healthy { color: #d5f55f; }
    .basic-status.healthy::before { background: #d5f55f; box-shadow: 0 0 12px rgba(213, 245, 95, .45); }
    .basic-status.syncing { color: #ffd070; }
    .basic-status.syncing::before { background: #ffd070; }
    .basic-status.isolated, .basic-status.stale, .basic-status.banned, .basic-status.error { border-color: #6a332c; background: #261817; color: #ffb8ad; font-weight: 900; }
    .basic-status.isolated::before, .basic-status.stale::before, .basic-status.banned::before, .basic-status.error::before { background: #ff7668; box-shadow: 0 0 12px rgba(255, 118, 104, .38); }
    .basic-status-detail { padding: 3px 7px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 11px; }
    .basic-status-detail:hover { border-color: #d5f55f; color: #d5f55f; }
    .lock-button { padding: 5px 8px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 12px; }
    .lock-button:hover { border-color: #d5f55f; color: #d5f55f; }
    h1 { margin: 0 0 4px; font-size: 28px; }
    h2 { margin: 0 0 12px; font-size: 18px; }
    h3 { margin: 0 0 10px; font-size: 15px; }
    button { border: 1px solid #3a4248; border-radius: 6px; padding: 8px 11px; font: inherit; font-weight: 700; background: #191c20; color: #e8edf0; cursor: pointer; }
    button:hover { border-color: #d5f55f; color: #d5f55f; }
    button.primary { background: #d5f55f; border-color: #d5f55f; color: #15171a; }
    button.primary:hover { background: #e4ff83; color: #15171a; }
    button.subtle { background: transparent; }
    button:disabled { cursor: default; opacity: .5; }
    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; }
    .metric, .panel { background: #181b1f; border: 1px solid #2a3035; border-radius: 8px; padding: 13px; }
    .metric .label { color: #8d989f; font-size: 11px; text-transform: uppercase; }
    .metric .value { margin-top: 7px; font-weight: 850; overflow-wrap: anywhere; }
    .panel { margin-bottom: 12px; }
    .split { display: grid; grid-template-columns: minmax(0, 1fr) minmax(320px, .72fr); gap: 12px; }
    form { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    label { display: grid; gap: 5px; color: #a8b2b8; font-size: 13px; }
    input, textarea { min-width: 180px; border: 1px solid #3a444b; border-radius: 6px; padding: 9px 10px; font: inherit; background: #101215; color: #edf2f5; }
    textarea { min-height: 118px; resize: vertical; line-height: 1.45; }
    input:focus, textarea:focus { outline: 2px solid #d5f55f; outline-offset: 1px; }
    table { width: 100%; border-collapse: collapse; font-size: 13px; }
    th, td { text-align: left; border-bottom: 1px solid #2a3035; padding: 8px; vertical-align: top; }
    th { color: #8d989f; font-size: 11px; text-transform: uppercase; }
    code { overflow-wrap: anywhere; color: #c7f5ea; }
    .table-wrap { overflow-x: auto; }
    .muted { color: #8d989f; }
    .flash { position: fixed; top: 18px; right: 18px; z-index: 80; width: min(420px, calc(100vw - 36px)); border-radius: 6px; padding: 10px 12px; border: 1px solid; font-weight: 700; box-shadow: 0 18px 48px rgba(0, 0, 0, .38); }
    .flash.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .flash.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .ok { color: #d5f55f; }
    .page-title { margin-bottom: 16px; }
    .setup-overlay { position: fixed; inset: 0; z-index: 30; display: grid; place-items: center; padding: 22px; background: rgba(8, 9, 10, .72); backdrop-filter: blur(8px); }
    .transaction-overlay { z-index: 40; }
    .setup-modal { width: min(980px, 100%); max-height: calc(100vh - 44px); overflow: auto; border: 1px solid #3b4448; border-radius: 8px; padding: 18px; background: #181b1f; box-shadow: 0 24px 80px rgba(0, 0, 0, .42); }
    .setup-modal-head { display: grid; gap: 5px; margin-bottom: 16px; }
    .setup-modal-head h2 { margin: 0; font-size: 24px; }
    .setup-welcome { color: #d5f55f; font-size: 12px; font-weight: 900; text-transform: uppercase; }
    .setup-copy { max-width: 620px; color: #a8b2b8; line-height: 1.45; }
    .setup-feedback { border: 1px solid; border-radius: 8px; padding: 10px 12px; margin-bottom: 14px; font-weight: 800; }
    .setup-feedback.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .setup-feedback.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .setup-grid { width: 100%; display: grid; grid-template-columns: minmax(0, .9fr) minmax(320px, .7fr); gap: 12px; align-items: start; }
    .setup-section { border: 1px solid #2f363c; border-radius: 8px; padding: 13px; background: #111316; }
    .setup-node-mode, .setup-network, .setup-wallet-section { grid-column: 1 / -1; }
    .segmented.setup-mode-picker { width: 100%; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); }
    .setup-mode-picker button { min-width: 0; min-height: 38px; white-space: normal; }
    .setup-network-row { display: grid; grid-template-columns: minmax(0, 1fr); gap: 10px; align-items: end; }
    .setup-network-copy { margin-top: 8px; color: #a8b2b8; line-height: 1.45; }
    .setup-network-link { color: #d5f55f; font-size: 12px; font-weight: 900; text-decoration: none; }
    .setup-network-link:hover { text-decoration: underline; }
    .setup-field { display: grid; gap: 6px; }
    .setup-field-label { color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; letter-spacing: 0; }
    .setup-address-box { display: flex; justify-content: space-between; gap: 10px; align-items: center; }
    .setup-address-box code { min-width: 0; }
    .setup-address-box button { flex: 0 0 auto; }
    .setup-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 14px; }
    .segmented { display: inline-flex; gap: 4px; padding: 4px; border: 1px solid #2f363c; border-radius: 8px; background: #181b1f; }
    .segmented button { border-color: transparent; background: transparent; color: #9fa8ad; }
    .segmented button.active { background: #d5f55f; color: #15171a; }
    .seed-panel { display: grid; gap: 12px; }
    .seed-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; }
    .seed-word { display: grid; grid-template-columns: 30px minmax(0, 1fr); gap: 7px; align-items: center; border: 1px solid #2f363c; border-radius: 6px; padding: 7px 8px; background: #181b1f; }
    .seed-word .index { color: #8d989f; font-size: 11px; font-weight: 800; }
    .seed-word .word { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-weight: 800; color: #c7f5ea; }
    .verify-grid { display: grid; gap: 8px; }
    .setup-status { border: 1px solid #566d25; border-radius: 8px; padding: 10px; background: #1c2516; color: #d5f55f; font-weight: 800; }
    .auth-form { width: min(420px, 100%); display: grid; gap: 10px; }
    .auth-form form { display: grid; gap: 10px; align-items: stretch; }
    .auth-form input { width: 100%; }
    .settings-grid { width: min(760px, 100%); display: grid; gap: 12px; }
    .settings-mode-row { display: flex; justify-content: space-between; gap: 14px; align-items: center; }
    .settings-mode-copy { min-width: 0; display: grid; gap: 4px; }
    .settings-mode-title { color: #e8edf0; font-size: 15px; font-weight: 850; }
    .settings-form { display: grid; gap: 10px; align-items: stretch; }
    .public-p2p-form { margin-top: 14px; }
    .settings-form label, .settings-form input { width: 100%; }
    .metrics-shell { display: grid; gap: 12px; }
    .metrics-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; }
    .metrics-head h2 { margin: 0; }
    .metrics-range { flex: 0 0 auto; }
    .metrics-range button { padding: 5px 9px; font-size: 12px; white-space: nowrap; }
    .metrics-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; }
    .metrics-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 430px), 1fr)); gap: 12px; }
    .metric-chart-card { display: grid; gap: 10px; min-width: 0; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; background: #181b1f; }
    .metric-chart-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; }
    .metric-chart-title { margin: 0; color: #e8edf0; font-size: 14px; font-weight: 850; }
    .metric-chart-value { color: #d5f55f; font-size: 13px; font-weight: 850; font-variant-numeric: tabular-nums; }
    .metric-chart-frame { display: grid; grid-template-columns: 50px minmax(0, 1fr); grid-template-rows: 156px 18px; column-gap: 6px; row-gap: 4px; min-width: 0; }
    .metric-chart-plot { position: relative; min-width: 0; }
    .metric-chart-svg { width: 100%; height: 156px; display: block; border: 1px solid #2f363c; border-radius: 8px; background: #111316; }
    .metric-chart-gridline { stroke: #3a4248; stroke-width: .8; stroke-dasharray: 3 7; opacity: .58; }
    .metric-chart-axis { stroke: #3a4248; stroke-width: 1.2; }
    .metric-chart-axis-label { color: #8d989f; font-size: 10px; font-weight: 750; font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .metric-chart-y-axis { position: relative; min-width: 0; }
    .metric-chart-y-axis .metric-chart-axis-label { position: absolute; right: 0; transform: translateY(-50%); max-width: 100%; }
    .metric-chart-x-axis { position: relative; grid-column: 2; min-width: 0; overflow: visible; }
    .metric-chart-x-axis .metric-chart-axis-label { position: absolute; top: 0; transform: translateX(-50%); }
    .metric-chart-line { fill: none; stroke: #d5f55f; stroke-width: 2.2; stroke-linejoin: round; stroke-linecap: round; }
    .metric-chart-points { position: absolute; inset: 0; }
    .metric-chart-point-hit { position: absolute; width: 18px; height: 18px; border: 0; border-radius: 50%; padding: 0; background: transparent; cursor: crosshair; transform: translate(-50%, -50%); }
    .metric-chart-point-hit::after { content: ""; position: absolute; left: 50%; top: 50%; width: 5px; height: 5px; border-radius: 50%; background: #d5f55f; opacity: .2; transform: translate(-50%, -50%); }
    .metric-chart-point-hit:hover::after, .metric-chart-point-hit:focus-visible::after, .metric-chart-point-hit.is-active::after { width: 8px; height: 8px; opacity: 1; }
    .metric-chart-tooltip { position: absolute; z-index: 1; max-width: min(180px, 80%); border: 1px solid #566d25; border-radius: 6px; padding: 5px 7px; background: #202615; color: #e8edf0; font-size: 11px; font-weight: 850; font-variant-numeric: tabular-nums; line-height: 1.25; pointer-events: none; box-shadow: 0 8px 20px rgba(0, 0, 0, .28); white-space: nowrap; }
    .metrics-empty { border: 1px dashed #3a4248; border-radius: 8px; padding: 14px; color: #8d989f; background: #111316; }
    .wallet-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .wallet-actions { display: grid; gap: 12px; }
    .advanced-toggle { flex-basis: 100%; width: max-content; align-self: flex-start; border-color: #3a4248; padding: 4px 7px; background: #202328; color: #9fa8ad; font-size: 12px; }
    .advanced-toggle:hover { border-color: #5a646b; color: #d6dee2; }
    .send-utxo-list { display: grid; gap: 8px; max-height: 260px; overflow: auto; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #111316; }
    .send-utxo-list-head { display: flex; justify-content: space-between; gap: 8px; align-items: center; color: #8d989f; font-size: 12px; font-weight: 800; }
    .send-utxo-actions { display: flex; gap: 6px; align-items: center; }
    .utxo-select-button { padding: 3px 7px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 12px; }
    .utxo-select-button:hover { border-color: #5a646b; color: #d6dee2; }
    .send-utxo-option { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #181b1f; }
    .send-utxo-option.disabled { border-color: #262c31; background: #14171a; color: #687178; }
    .send-utxo-option.disabled code, .send-utxo-option.disabled .utxo-node-amount { color: #687178; }
    .send-utxo-option input { min-width: auto; margin-top: 3px; }
    .utxo-status { color: #8d989f; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .send-utxo-summary { flex-basis: 100%; width: 100%; display: grid; gap: 5px; color: #9eb3bc; font-size: 13px; }
    .wallet-balance-line { display: inline-grid; grid-template-columns: auto auto; gap: 10px; align-items: baseline; padding: 8px 10px; border: 1px solid #2f363c; border-radius: 8px; background: #111316; color: inherit; cursor: pointer; }
    .wallet-balance-line:hover, .wallet-balance-line:focus-visible { border-color: #d5f55f; outline: none; }
    .wallet-balance-line .tx-value { font-size: 16px; font-weight: 850; }
    .mining-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; align-items: start; }
    .panel-description { max-width: 760px; margin: -4px 0 12px; color: #9eb3bc; font-size: 13px; line-height: 1.45; }
    .mining-form { width: 100%; display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .burn-fields { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .mine-action-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px; align-items: center; }
    .mine-settings-form { display: grid; gap: 10px; }
    .mine-fee-fields { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .fee-preview { flex-basis: 100%; color: #9eb3bc; font-size: 12px; font-weight: 700; }
    .mine-stats { display: grid; grid-template-columns: repeat(4, minmax(112px, 1fr)); gap: 8px; min-width: 0; }
    .fee-history { grid-template-columns: repeat(3, minmax(112px, 1fr)); margin-top: 12px; }
    .mine-stat { min-width: 0; border: 1px solid #2f363c; border-radius: 8px; padding: 9px 10px; background: #111316; }
    .mine-stat-label { display: flex; gap: 5px; align-items: center; color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .mine-stat-value { margin-top: 5px; color: #dce4e7; font-size: 14px; font-weight: 850; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
    .mine-stat-value.money { color: #d5f55f; }
    .mine-reward-control { display: grid; gap: 7px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .mine-reward-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; color: #a8b2b8; font-size: 13px; font-weight: 800; }
    .mine-reward-head strong { color: #d5f55f; font-size: 14px; font-variant-numeric: tabular-nums; }
    .mine-reward-control input[type="range"] { width: 100%; min-width: 0; padding: 0; accent-color: #d5f55f; }
    .mine-slider-hints { display: flex; justify-content: space-between; gap: 10px; color: #879198; font-size: 11px; font-weight: 750; }
    .mine-include-status { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 7px; color: #9eb3bc; font-size: 12px; font-weight: 800; }
    .mine-include-status.waiting { color: #ffd280; }
    .mine-include-status.ready { color: #d5f55f; }
    .mine-include-status.muted { color: #879198; }
    .mine-save-row { display: flex; justify-content: flex-start; }
    .panel-separator { border-top: 1px solid #2f363c; margin: 14px 0 12px; }
    .stratum-config { display: grid; gap: 10px; }
    .stratum-note { max-width: 760px; color: #9eb3bc; font-size: 12px; line-height: 1.45; }
    .stratum-note code { color: #dce4e7; }
    .stratum-fields { display: grid; gap: 8px; }
    .stratum-field { display: grid; grid-template-columns: 86px minmax(0, 1fr); gap: 10px; align-items: baseline; min-width: 0; }
    .stratum-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .stratum-value { min-width: 0; color: #dce4e7; font-size: 13px; font-weight: 650; overflow-wrap: anywhere; }
    .stratum-value.hash { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; font-weight: 500; }
    .info-button { display: inline-grid; place-items: center; width: 18px; height: 18px; padding: 0; border-radius: 999px; border-color: #3a4248; background: #181b1f; color: #9eb3bc; font-size: 11px; line-height: 1; }
    .info-button:hover, .info-button:focus-visible { border-color: #d5f55f; color: #d5f55f; outline: none; }
    .info-copy { display: grid; gap: 10px; color: #c3cbd0; line-height: 1.45; }
    .info-copy p { margin: 0; }
    .info-facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; }
    .info-fact { border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .info-fact .label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .info-fact .value { margin-top: 5px; color: #d5f55f; font-weight: 850; }
    .mining-head { align-items: center; }
    .toggle-switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 9px; color: #9fa8ad; font-size: 12px; font-weight: 850; cursor: pointer; user-select: none; }
    .toggle-switch input { position: absolute; width: 1px; height: 1px; min-width: 0; margin: 0; opacity: 0; pointer-events: none; }
    .toggle-track { position: relative; width: 46px; height: 26px; border: 1px solid #3a4248; border-radius: 999px; background: #101215; transition: background .16s ease, border-color .16s ease; }
    .toggle-thumb { position: absolute; top: 3px; left: 3px; width: 18px; height: 18px; border-radius: 999px; background: #879198; transition: transform .16s ease, background .16s ease; }
    .toggle-switch.active { color: #d5f55f; }
    .toggle-switch.active .toggle-track { border-color: #d5f55f; background: #263219; }
    .toggle-switch.active .toggle-thumb { transform: translateX(20px); background: #d5f55f; }
    .toggle-switch:focus-within .toggle-track { outline: 2px solid #d5f55f; outline-offset: 2px; }
    .toggle-text { min-width: 22px; text-align: right; }
    .receive-address { display: grid; gap: 8px; }
    .address-box { border: 1px solid #2f363c; border-radius: 8px; padding: 11px; background: #111316; }
    .panel-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; margin-bottom: 12px; }
    .panel-head h2, .panel-head h3 { margin-bottom: 0; }
    .switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 8px; color: #d6dee2; font-weight: 700; }
    .switch input { width: auto; min-width: 0; accent-color: #d5f55f; }
    .wallet-tx-panel { min-width: 0; overflow: hidden; }
    .wallet-tx-panel .panel-head { flex-wrap: wrap; }
    .wallet-tx-filters { display: flex; flex-wrap: wrap; gap: 6px; justify-content: flex-end; align-items: center; }
    .tx-filter { display: inline-flex; align-items: center; gap: 6px; border: 1px solid #3a4248; border-radius: 999px; padding: 4px 8px; color: #9fa8ad; background: #111316; font-size: 12px; font-weight: 850; cursor: pointer; user-select: none; }
    .tx-filter input { position: absolute; width: 1px; height: 1px; min-width: 0; margin: 0; opacity: 0; pointer-events: none; }
    .tx-filter.active { border-color: #d5f55f; background: #202616; color: #d5f55f; }
    .tx-filter:focus-within { outline: 2px solid #d5f55f; outline-offset: 2px; }
    .wallet-tx-list { max-height: min(620px, calc(100vh - 220px)); min-width: 0; display: grid; gap: 8px; overflow-y: auto; overscroll-behavior-y: contain; padding-right: 4px; }
    .wallet-tx-row { position: relative; display: grid; grid-template-columns: minmax(0, 1fr); gap: 8px; align-items: start; border: 1px solid #2f363c; border-radius: 8px; padding: 12px; background: #111316; cursor: pointer; text-align: left; }
    .wallet-tx-row:hover, .wallet-tx-row:focus-visible, .tx-card:hover, .tx-card:focus-visible, .mempool-item:hover, .mempool-item:focus-visible { border-color: #d5f55f; box-shadow: 0 0 0 1px rgba(213, 245, 95, .22); outline: none; }
    .wallet-tx-row.pending { border-color: #3a4147; background: #191c20; box-shadow: inset 3px 0 0 #6f7880; }
    .wallet-tx-row .pill { position: absolute; top: 10px; right: 10px; }
    .wallet-tx-main { display: grid; gap: 5px; min-width: 0; padding-right: 92px; }
    .tx-field { display: grid; grid-template-columns: 74px minmax(0, 1fr); gap: 8px; align-items: baseline; min-width: 0; }
    .tx-label { color: #879198; font-size: 10px; font-weight: 800; text-transform: uppercase; letter-spacing: 0; }
    .tx-value { min-width: 0; color: #dce4e7; font-size: 13px; font-weight: 600; overflow-wrap: anywhere; }
    .tx-value.money { color: #d5f55f; font-variant-numeric: tabular-nums; }
    .tx-value.hash { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; font-weight: 500; }
    .tx-value.number { color: #c7d0d5; font-variant-numeric: tabular-nums; }
    .tx-value.text { color: #e8edf0; }
    .metric-context { display: grid; gap: 5px; margin-top: 12px; }
    .peer-toolbar { display: flex; justify-content: space-between; gap: 12px; align-items: start; flex-wrap: wrap; margin-bottom: 12px; }
    .peer-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(132px, 1fr)); gap: 8px; margin-bottom: 12px; }
    .peer-summary-item { min-width: 0; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .peer-summary-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .peer-summary-value { margin-top: 5px; color: #dce4e7; font-size: 15px; font-weight: 850; }
    .peer-form { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .peer-form label { min-width: min(320px, 100%); }
    .peer-form input { width: 100%; }
    .peer-status { display: inline-flex; align-items: center; border: 1px solid #3a4248; border-radius: 999px; padding: 3px 8px; color: #a8b2b8; font-size: 11px; font-weight: 850; }
    .peer-status.synced, .peer-status.active { border-color: #566d25; color: #d5f55f; background: #1c2516; }
    .peer-status.stale { border-color: #5f5125; color: #ffe08a; background: #211d12; }
    .peer-status.banned { border-color: #713434; color: #ffb1a8; background: #2a1717; }
    .peer-status.error { border-color: #713434; color: #ffb1a8; background: #2a1717; }
    .peer-actions { display: flex; gap: 6px; align-items: center; }
    .peer-remove { padding: 4px 7px; border-color: #4f3737; background: #221717; color: #ffb1a8; font-size: 12px; }
    .peer-remove:hover { border-color: #ffb1a8; color: #ffd4cf; }
    .network-health { display: grid; grid-template-columns: minmax(180px, .8fr) minmax(0, 1.2fr); gap: 12px; align-items: stretch; margin-bottom: 12px; }
    .network-health-state { display: grid; align-content: center; gap: 5px; border: 1px solid #3a4248; border-radius: 8px; padding: 12px; background: #111316; }
    .network-health-state.healthy { border-color: #566d25; background: #182112; }
    .network-health-state.syncing, .network-health-state.stale { border-color: #5f5125; background: #211d12; }
    .network-health-state.isolated, .network-health-state.error, .network-health-state.banned { border-color: #713434; background: #241716; }
    .network-health-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .network-health-value { color: #e8edf0; font-size: 20px; font-weight: 900; text-transform: capitalize; }
    .network-health-detail { color: #a8b2b8; font-size: 12px; }
    .network-health-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(116px, 1fr)); gap: 8px; }
    .panel .grid + form { margin-top: 12px; }
    .explorer-shell { width: 100%; display: grid; gap: 12px; }
    .block-rail-wrap { background: #181b1f; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; overflow: hidden; }
    .block-rail-head { display: flex; justify-content: space-between; gap: 10px; align-items: center; margin-bottom: 10px; }
    .block-rail { display: flex; gap: 8px; overflow-x: auto; padding: 1px 0 10px; scroll-snap-type: x proximity; }
    .block-card { flex: 0 0 122px; min-height: 100px; display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 9px; background: #111316; color: #e8edf0; text-align: left; scroll-snap-align: start; }
    .block-card:hover { border-color: #d5f55f; color: #d5f55f; }
    .block-card.selected { background: #202616; border-color: #d5f55f; box-shadow: inset 0 0 0 1px #d5f55f; }
    .block-card.new-block { animation: block-arrive .45s ease both; }
    @keyframes block-arrive { from { opacity: .2; transform: translateX(-12px); } to { opacity: 1; transform: translateX(0); } }
    .block-height { font-size: 18px; font-weight: 900; }
    .block-meta { display: flex; gap: 8px; color: #8d989f; font-size: 12px; }
    .block-miner { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11px; overflow-wrap: anywhere; color: #9eb3bc; }
    .skeleton-card { pointer-events: none; position: relative; overflow: hidden; }
    .skeleton-card::after { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent, rgba(213, 245, 95, .12), transparent); animation: skeleton-sweep 1.15s ease-in-out infinite; }
    @keyframes skeleton-sweep { from { transform: translateX(-100%); } to { transform: translateX(100%); } }
    .skeleton-line { height: 12px; border-radius: 6px; background: #2b3136; }
    .skeleton-line.short { width: 42%; }
    .skeleton-line.medium { width: 68%; }
    .skeleton-line.long { width: 88%; }
    .page-sentinel { min-height: 1px; }
    .block-page-sentinel { flex: 0 0 1px; min-height: 100px; }
    .dataset-loader { display: grid; gap: 8px; min-width: 0; }
    .skeleton-table-cell { height: 12px; width: 100%; border-radius: 6px; background: #2b3136; }
    tr.skeleton-card td { padding-top: 11px; padding-bottom: 11px; }
    .detail-grid { display: grid; grid-template-columns: minmax(0, .9fr) minmax(0, 1.1fr); gap: 12px; }
    .detail-kv { display: grid; grid-template-columns: 90px minmax(0, 1fr); gap: 8px; font-size: 13px; margin: 7px 0; }
    .detail-kv .key { color: #8d989f; }
    .detail-link { width: fit-content; max-width: 100%; padding: 0; border: 0; background: transparent; color: #d7f2ff; font: inherit; text-align: left; cursor: pointer; }
    .detail-link code { color: inherit; text-decoration: underline; text-underline-offset: 3px; }
    .rank-list { display: grid; gap: 8px; }
    .rank-row { display: grid; grid-template-columns: 52px minmax(0, 1fr); gap: 10px; align-items: start; border: 1px solid #30383d; border-radius: 8px; padding: 10px; background: #15191d; }
    .rank-number { color: #d7f2ff; font-weight: 700; }
    .rank-details { display: grid; gap: 6px; min-width: 0; }
    .tx-list { display: grid; gap: 8px; }
    .tx-card, .mempool-item { position: relative; display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 12px; background: #111316; cursor: pointer; text-align: left; }
    .tx-card .pill, .mempool-item .pill { position: absolute; top: 10px; right: 10px; }
    .pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 2px 8px; font-size: 12px; font-weight: 800; background: #2b3136; color: #d6dee2; }
    .pill.burn { background: #332918; color: #ffd070; }
    .pill.transfer { background: #17312a; color: #8de9cd; }
    .pill.mine { background: #172a34; color: #8bdcff; }
    .mempool-panel { min-width: 0; overflow: hidden; }
    .mempool-strip { width: 100%; min-width: 0; display: flex; gap: 8px; overflow-x: auto; overscroll-behavior-x: contain; padding: 1px 0 10px; scroll-snap-type: x proximity; }
    .mempool-item { flex: 0 0 220px; scroll-snap-align: start; }
    .tx-modal { width: min(940px, 100%); max-height: calc(100vh - 44px); overflow: auto; border: 1px solid #3b4448; border-radius: 8px; padding: 16px; background: #181b1f; box-shadow: 0 24px 80px rgba(0, 0, 0, .46); }
    .tx-modal-head { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; margin-bottom: 14px; }
    .tx-modal-title { display: grid; justify-items: start; gap: 6px; min-width: 0; }
    .tx-modal-title h2 { margin: 0; }
    .tx-modal-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 8px; margin-bottom: 12px; }
    .utxo-flow { display: grid; grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr); gap: 12px; align-items: stretch; }
    .utxo-column { display: grid; align-content: start; gap: 8px; min-width: 0; }
    .utxo-column h3 { margin: 0; color: #8d989f; font-size: 11px; text-transform: uppercase; }
    .utxo-node { display: grid; gap: 5px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; min-width: 0; }
    .utxo-node.burned { border-color: #5e4821; background: #1f1a12; }
    .utxo-node.fee { border-color: #4b5260; background: #171a20; }
    .utxo-node-label { display: flex; justify-content: space-between; gap: 8px; color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; }
    .utxo-node-amount { color: #d5f55f; font-weight: 850; font-variant-numeric: tabular-nums; }
    .utxo-node-address, .utxo-node-ref { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; overflow-wrap: anywhere; }
    .utxo-arrow { display: grid; place-items: center; color: #d5f55f; font-size: 24px; font-weight: 900; }
    .tx-modal-empty { border: 1px dashed #3a4248; border-radius: 8px; padding: 10px; color: #8d989f; }
    .utxo-list { display: grid; gap: 8px; }
    .wallet-utxo-row { display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    @media (max-width: 760px) { .utxo-flow, .mine-action-row, .mine-stats { grid-template-columns: 1fr; } .utxo-arrow { min-height: 28px; transform: rotate(90deg); } .tx-modal-head { align-items: stretch; } }
    @media (max-width: 920px) { .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .network-health { grid-template-columns: 1fr; } }
    @media (max-width: 760px) {
      .app-shell { display: grid; grid-template-columns: 1fr; }
      .sidebar { position: sticky; inset: auto; width: auto; height: auto; flex-direction: row; justify-content: space-between; padding: 8px; border-right: 0; border-bottom: 1px solid #262b2f; }
      .brand-mark { width: 34px; height: 34px; }
      .brand-mark svg { width: 22px; height: 22px; }
      .side-nav { display: flex; width: auto; gap: 8px; }
      .nav-button { width: 52px; min-height: 48px; }
      .nav-button span { font-size: 10px; }
      .settings-button, .version-panel { margin-top: 0; width: 48px; min-height: 48px; padding: 6px 3px; }
      .settings-button svg { width: 21px; height: 21px; }
      .content { padding: 16px 12px 36px; }
      header, .split, .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .wallet-tx-row { grid-template-columns: 1fr; }
      header { display: grid; }
      .settings-mode-row { align-items: flex-start; }
      .metrics-head { align-items: flex-start; flex-direction: column; }
      .metrics-range { width: 100%; }
      .metrics-range button { flex: 1 1 0; }
      .segmented.setup-mode-picker { grid-template-columns: 1fr; }
      .metrics-grid { grid-template-columns: 1fr; }
      input { min-width: 0; width: 100%; }
      .switch input { width: auto; }
      .seed-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .block-card { flex-basis: 108px; }
    }
  </style>
  <script defer src="/assets/iuna-ui.js?v=80"></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="iunaApp()" x-init="init()" @keydown.window.escape="closeModals()" x-cloak>
  <div class="app-shell">
    <aside class="sidebar" aria-label="iuna navigation">
      <div class="brand-mark" title="iuna" aria-label="iuna"><svg viewBox="0 0 32 32" aria-hidden="true" focusable="false"><circle class="mark-dot" cx="9.4" cy="7.6" r="2.8"></circle><path class="mark-loop" d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13"></path></svg></div>
      <nav class="side-nav">
        <button class="nav-button" :class="{ active: tab === 'wallet' }" @click="setTab('wallet')" type="button" title="Wallet" aria-label="Wallet">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3z"></path><path d="M3 7V5a2 2 0 0 1 2-2h12"></path><path d="M16 13h3"></path></svg>
          <span>Wallet</span>
        </button>
        <button class="nav-button" x-show="advancedMode()" :class="{ active: tab === 'mining' }" @click="setTab('mining')" type="button" title="Mining" aria-label="Mining">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 19V5"></path><path d="M4 19h16"></path><path d="M7 15l4-4 3 3 5-7"></path></svg>
          <span>Mining</span>
        </button>
        <button class="nav-button" x-show="advancedMode()" :class="{ active: tab === 'p2p' }" @click="setTab('p2p')" type="button" title="P2P" aria-label="P2P">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="12" r="3"></circle><circle cx="18" cy="6" r="3"></circle><circle cx="18" cy="18" r="3"></circle><path d="M8.5 10.5 15.5 7.5"></path><path d="M8.5 13.5 15.5 16.5"></path></svg>
          <span>P2P</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'chain' }" @click="setTab('chain')" type="button" title="Explorer" aria-label="Explorer">
          <svg class="chain-icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="1.5" y="9" width="5.5" height="5.5"></rect><rect x="9.25" y="9" width="5.5" height="5.5"></rect><rect x="17" y="9" width="5.5" height="5.5"></rect></svg>
          <span>Chain</span>
        </button>
        <button class="nav-button" x-show="config.keep_track_of_metrics" :class="{ active: tab === 'metrics' }" @click="setTab('metrics')" type="button" title="Metrics" aria-label="Metrics">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 19V5"></path><path d="M4 19h16"></path><path d="M7 15l3-4 3 2 4-7"></path><path d="M7 17h10"></path></svg>
          <span>Metrics</span>
        </button>
      </nav>
      <button class="settings-button" :class="{ active: tab === 'settings' }" type="button" @click="setTab('settings')" title="Settings" aria-label="Settings">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.7 3.2 9.2 5.5a7.2 7.2 0 0 0-1.4.8L5.6 5.6 3.2 9.8l1.7 1.6a7.8 7.8 0 0 0 0 1.6l-1.7 1.6 2.4 4.2 2.2-.7a7.2 7.2 0 0 0 1.4.8l.5 2.3h4.8l.5-2.3a7.2 7.2 0 0 0 1.4-.8l2.2.7 2.4-4.2-1.7-1.6a7.8 7.8 0 0 0 0-1.6L21 9.8l-2.4-4.2-2.2.7a7.2 7.2 0 0 0-1.4-.8l-.5-2.3H9.7Z"></path><circle cx="12" cy="12.2" r="3.1"></circle></svg>
      </button>
      <button class="version-panel" type="button" :class="{ update: updateAvailable(), checking: releaseCheckState === 'checking', failed: releaseCheckState === 'failed' }" :title="versionPanelTitle()" @click="openLatestRelease">
        <span class="version-dot" aria-hidden="true"></span>
        <span class="version-label" x-text="appVersionLabel()"></span>
        <span class="version-update" x-show="updateAvailable()">Update</span>
      </button>
    </aside>

    <main class="content">
    <header>
      <div>
        <h1 x-text="pageTitle()">iuna</h1>
        <div class="basic-status-row" x-show="basicMode()">
          <span class="basic-status" :class="networkHealthClass()" x-text="basicNetworkStatusLabel()"></span>
          <button class="basic-status-detail" type="button" x-show="basicNetworkNeedsAttention()" @click="setUiMode('advanced'); setTab('p2p')">Details</button>
        </div>
      </div>
      <div class="header-actions">
        <div class="muted" x-text="lastUpdatedLabel()"></div>
        <button class="lock-button" type="button" x-show="auth.authenticated" @click="logout">Lock</button>
      </div>
    </header>

    <div class="flash" :class="flash?.kind" x-show="flash" x-transition x-text="flash?.message"></div>

    <section x-show="tab === 'wallet'">
      <div class="page-title">
        <button class="wallet-balance-line" type="button" @click="openWalletUtxosModal" title="Show wallet UTXOs">
          <span class="tx-label">Balance</span>
          <span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span>
        </button>
      </div>
      <div class="wallet-grid">
        <div class="wallet-actions">
          <div class="panel">
            <h3>Send</h3>
            <form @submit.prevent="sendTransfer">
              <label>Recipient<input x-model="transferTo" @input="scheduleFeeEstimates" autocomplete="off" required></label>
              <label>Amount<input x-model="transferAmount" @input="scheduleFeeEstimates" type="number" min="0.000001" step="0.000001" required></label>
              <label>Fee / byte<input x-model="transferFee" @input="scheduleFeeEstimates" type="number" min="0" step="0.000001" required></label>
              <div class="fee-preview" x-text="feeEstimateLabel('transfer')"></div>
              <button class="advanced-toggle" type="button" @click="toggleSendAdvanced" x-text="showSendAdvanced ? 'Hide UTXOs' : 'UTXOs'"></button>
              <div class="send-utxo-summary" x-show="showSendAdvanced">
                <div>Selected UTXOs: <span x-text="selectedTransferUtxos.length"></span></div>
                <div>Selected total: IUNA <span x-text="amountLabel(selectedTransferUtxoTotal())"></span></div>
                <div>Required: IUNA <span x-text="amountLabel(transferRequiredTotal())"></span></div>
                <div class="setup-feedback error" x-show="!selectedTransferUtxosCoverTransfer()">Selected UTXOs do not cover amount plus fee</div>
                <div class="send-utxo-list">
                  <div class="send-utxo-list-head">
                    <span>UTXOs</span>
                    <span class="send-utxo-actions">
                      <button class="utxo-select-button" type="button" @click="selectAllTransferUtxos" :disabled="walletUtxoPage.loading && walletUtxos.length === 0">Select all</button>
                      <button class="utxo-select-button" type="button" @click="clearTransferUtxos" :disabled="selectedTransferUtxos.length === 0">None</button>
                    </span>
                  </div>
                  <template x-for="utxo in walletUtxos" :key="utxoOutpoint(utxo)">
                    <label class="send-utxo-option" :class="{ disabled: !utxo.spendable }">
                      <input type="checkbox" :value="utxoOutpoint(utxo)" x-model="selectedTransferUtxos" @change="scheduleFeeEstimates" :disabled="!utxo.spendable">
                      <span>
                        <span class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">IUNA <span x-text="amountLabel(utxo.amount)"></span></span></span>
                        <span class="utxo-status" x-show="!utxo.spendable">Pending</span>
                        <code class="tx-value hash" x-text="utxoOutpoint(utxo)"></code>
                      </span>
                    </label>
                  </template>
                  <div class="dataset-loader" x-show="walletUtxoPage.loading" aria-hidden="true">
                    <div class="send-utxo-option skeleton-card"><span><span class="skeleton-line medium"></span><span class="skeleton-line long"></span></span></div>
                    <div class="send-utxo-option skeleton-card"><span><span class="skeleton-line short"></span><span class="skeleton-line long"></span></span></div>
                  </div>
                  <div class="page-sentinel" x-show="walletUtxoPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletUtxo', $el))"></div>
                  <div class="tx-modal-empty" x-show="walletUtxos.length === 0 && !walletUtxoPage.loading">No UTXOs</div>
                </div>
              </div>
              <button class="primary" type="submit">Send</button>
            </form>
          </div>
          <div class="panel">
            <div class="panel-head">
              <h3>Receive</h3>
              <button type="button" @click="copyAddress">Copy</button>
            </div>
            <div class="receive-address">
              <div class="muted">Public key / address</div>
              <div class="address-box"><code x-text="status.wallet_address || '-'"></code></div>
            </div>
          </div>
        </div>
        <div class="panel wallet-tx-panel">
          <div class="panel-head">
            <h3>Transactions</h3>
            <div class="wallet-tx-filters" aria-label="Transaction filters">
              <label class="tx-filter" :class="{ active: walletTxFilters.transfer }">
                <input type="checkbox" x-model="walletTxFilters.transfer" @change="refreshWalletTransactions()">
                <span>Tx</span>
              </label>
              <label class="tx-filter" :class="{ active: walletTxFilters.mine }">
                <input type="checkbox" x-model="walletTxFilters.mine" @change="refreshWalletTransactions()">
                <span>Mine</span>
              </label>
              <label class="tx-filter" :class="{ active: walletTxFilters.burn }">
                <input type="checkbox" x-model="walletTxFilters.burn" @change="refreshWalletTransactions()">
                <span>Burn</span>
              </label>
            </div>
          </div>
          <div class="wallet-tx-list">
            <template x-for="tx in walletTransactions()" :key="tx.status + '-' + tx.signature">
              <div class="wallet-tx-row" :class="{ pending: tx.status === 'pending' }" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Wallet' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Wallet' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Wallet' })">
                <span class="pill" :class="tx.kind" x-text="tx.direction"></span>
                <div class="wallet-tx-main">
                  <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.amount)"></span></span></div>
                  <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                  <div class="tx-field"><span class="tx-label">Status</span><span class="tx-value text" x-text="txTitle(tx)"></span></div>
                  <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(tx.from)"></code></div>
                  <div class="tx-field" x-show="tx.to"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(tx.to)"></code></div>
                  <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                  <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                  <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                </div>
              </div>
            </template>
            <div class="dataset-loader" x-show="walletTxPage.loading" aria-hidden="true">
              <div class="wallet-tx-row skeleton-card"><div class="wallet-tx-main"><div class="skeleton-line medium"></div><div class="skeleton-line short"></div><div class="skeleton-line long"></div></div></div>
              <div class="wallet-tx-row skeleton-card"><div class="wallet-tx-main"><div class="skeleton-line short"></div><div class="skeleton-line medium"></div><div class="skeleton-line long"></div></div></div>
            </div>
            <div class="page-sentinel" x-show="walletTxPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletTx', $el))"></div>
            <div class="muted" x-show="walletTransactions().length === 0 && !walletTxPage.loading">No wallet transactions</div>
          </div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'mining'">
      <div class="page-title">
        <div class="muted">PoB/VDF block production with PoW issuance actions</div>
      </div>
      <div class="mining-grid">
        <div class="panel">
          <h3>Status</h3>
          <div class="grid">
            <div class="metric"><div class="label">Current Leader</div><div class="value" x-text="isLeaderLabel()"></div></div>
            <div class="metric"><div class="label">Last Burn Height</div><div class="value" x-text="status.mining?.last_auto_burn_height ?? '-'"></div></div>
            <div class="metric"><div class="label">VDF Rounds</div><div class="value" x-text="status.mining?.vdf_rounds ?? '-'"></div></div>
            <div class="metric"><div class="label">Target</div><div class="value" x-text="targetSecondsLabel()"></div></div>
          </div>
        </div>
        <div class="panel">
          <div class="panel-head mining-head">
            <h3>Burn</h3>
            <label class="toggle-switch" :class="{ active: miningEnabled }">
              <input type="checkbox" :checked="miningEnabled" @change="setMiningEnabled($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="miningEnabled ? 'On' : 'Off'"></span>
            </label>
          </div>
          <div class="panel-description">Burn IUNA to compete for block finalization. Winning burns finalize PoB/VDF blocks and earn the transaction fees in those blocks.</div>
          <form class="mining-form" @submit.prevent="saveBurn">
            <div class="burn-fields">
              <label>IUNA per block<input x-model="burnAmountDraft" @input="burnAmountDirty = true; scheduleFeeEstimates()" type="number" min="0.000001" step="0.000001"></label>
              <label>Fee / byte<input x-model="burnFeeDraft" @input="burnAmountDirty = true; scheduleFeeEstimates()" type="number" min="0" step="0.000001" required></label>
              <button class="primary" type="submit">Save</button>
            </div>
            <div class="fee-preview" x-text="feeEstimateLabel('burn')"></div>
          </form>
          <div class="mine-stats fee-history" aria-label="Recent block fees">
            <div class="mine-stat">
              <div class="mine-stat-label">Last block fees</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(1))"></span></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">5 block avg</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(5))"></span></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">30 block avg</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(30))"></span></div>
            </div>
          </div>
        </div>
        <div class="panel">
          <h3>Mine</h3>
          <div class="panel-description">Search for PoW actions that mint a fixed IUNA reward.</div>
          <div class="mine-settings-form">
            <div class="mine-action-row">
              <div class="mine-stats" aria-label="PoW issuance settings">
                <div class="mine-stat">
                  <div class="mine-stat-label">You receive</div>
                  <div class="mine-stat-value money">IUNA <span x-text="amountLabel(powMineReward())"></span></div>
                </div>
                <div class="mine-stat">
                  <div class="mine-stat-label">Finalizer earns</div>
                  <div class="mine-stat-value">IUNA <span x-text="amountLabel(feeEstimates.mine?.fee ?? 0)"></span></div>
                </div>
                <div class="mine-stat">
                  <div class="mine-stat-label">Difficulty <button class="info-button" type="button" @click="openPowDifficultyInfo" title="How difficulty is adjusted" aria-label="How PoW difficulty is adjusted">i</button></div>
                  <div class="mine-stat-value"><span x-text="status.chain?.current_mine_difficulty_bits ?? status.launch_profile?.mine_difficulty_bits ?? '-'"></span> bits</div>
                </div>
              </div>
              <label class="toggle-switch" :class="{ active: powMiningEnabled }" title="Continuously search for PoW mine actions with a small local work budget">
                <input type="checkbox" :checked="powMiningEnabled" @change="setPowMiningEnabled($event.target.checked)">
                <span class="toggle-track"><span class="toggle-thumb"></span></span>
                <span class="toggle-text" x-text="powMiningEnabled ? 'On' : 'Off'"></span>
              </label>
            </div>
            <div class="fee-preview" x-text="autoPowStatusLabel()"></div>
          </div>
          <div class="panel-separator"></div>
          <div class="stratum-config">
            <div class="stratum-note">Start the node with <code>--stratum 0.0.0.0:3333</code> to expose a Stratum V1 endpoint for ASIC miners. Use the pool URL below in the miner configuration.</div>
            <div class="stratum-fields" aria-label="Stratum settings">
              <div class="stratum-field">
                <div class="stratum-label">Status</div>
                <div class="stratum-value" x-text="status.stratum?.enabled ? 'On' : 'Off'"></div>
              </div>
              <div class="stratum-field">
                <div class="stratum-label">Listener</div>
                <code class="stratum-value hash" x-text="stratumListenAddr()"></code>
              </div>
              <div class="stratum-field">
                <div class="stratum-label">Pool URL</div>
                <code class="stratum-value hash" x-text="stratumPoolUrl()"></code>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'p2p'">
      <div class="panel">
        <div class="peer-toolbar">
          <div>
            <h2>Peers</h2>
            <div class="panel-description" x-text="p2pAcceptInbound ? 'Manage outbound peers and inspect inbound or outbound sessions. Public node is accepting inbound P2P connections.' : 'Manage outbound peers and inspect sync health. This node is outbound-only and does not open an inbound P2P port.'"></div>
          </div>
          <form class="peer-form" @submit.prevent="addPeer">
            <label>Peer address<input x-model="peerAddress" placeholder="seed.example:9444"></label>
            <button class="primary" type="submit">Add</button>
          </form>
        </div>
        <div class="network-health">
          <div class="network-health-state" :class="networkHealthClass()">
            <div class="network-health-label">Network Health</div>
            <div class="network-health-value" x-text="networkHealth.state || '-'"></div>
            <div class="network-health-detail" x-text="networkHealth.last_error || 'No peer errors reported'"></div>
          </div>
          <div class="network-health-grid">
            <div class="peer-summary-item"><div class="peer-summary-label">Local Height</div><div class="peer-summary-value" x-text="networkHealth.local_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Best Known</div><div class="peer-summary-value" x-text="networkHealth.best_known_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Lag</div><div class="peer-summary-value" x-text="networkLagLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Stale</div><div class="peer-summary-value" x-text="networkHealth.stale_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Banned</div><div class="peer-summary-value" x-text="networkHealth.banned_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Mempool</div><div class="peer-summary-value" x-text="networkHealth.pending_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Peer Mempools</div><div class="peer-summary-value" x-text="networkHealth.mempool_known_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Divergent</div><div class="peer-summary-value" x-text="networkHealth.mempool_divergent_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Missing Tx</div><div class="peer-summary-value" x-text="networkHealth.mempool_missing_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Time Offset</div><div class="peer-summary-value" x-text="networkTimeOffsetLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Clock Warnings</div><div class="peer-summary-value" x-text="networkHealth.bad_clock_peers ?? '-'"></div></div>
          </div>
        </div>
        <div class="peer-summary">
          <div class="peer-summary-item"><div class="peer-summary-label">Outbound</div><div class="peer-summary-value" x-text="outboundPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Inbound</div><div class="peer-summary-value" x-text="inboundPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Healthy</div><div class="peer-summary-value" x-text="healthyPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Errors</div><div class="peer-summary-value" x-text="failedPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Shared Height</div><div class="peer-summary-value" x-text="sharedHeightLabel()"></div></div>
        </div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>Status</th><th>Address</th><th>Direction</th><th>Last Contact</th><th>Clock</th><th>Ban</th><th>Score</th><th>Height</th><th>Delta</th><th>Tip</th><th>Sent</th><th>Received</th><th>Last Error</th><th>Actions</th></tr></thead>
            <tbody>
              <template x-for="peer in peers" :key="peer.address">
                <tr>
                  <td><span class="peer-status" :class="peerStatus(peer)" x-text="peerStatusLabel(peer)"></span></td>
                  <td><code x-text="peer.address"></code></td>
                  <td x-text="peer.direction"></td>
                  <td x-text="peerLastContactLabel(peer)"></td>
                  <td x-text="peerClockLabel(peer)"></td>
                  <td x-text="peerBanLabel(peer)"></td>
                  <td x-text="peer.misbehavior_score ?? 0"></td>
                  <td x-text="peer.last_known_height ?? '-'"></td>
                  <td x-text="peerHeightDelta(peer)"></td>
                  <td><code x-text="short(peer.last_known_tip_hash)"></code></td>
                  <td x-text="peer.messages_sent"></td>
                  <td x-text="peer.messages_received"></td>
                  <td x-text="peer.last_error || ''"></td>
                  <td><div class="peer-actions"><button class="peer-remove" type="button" x-show="canRemovePeer(peer)" @click="removePeer(peer)">Remove</button><span class="muted" x-show="!canRemovePeer(peer)">Observed</span></div></td>
                </tr>
              </template>
              <tr class="skeleton-card" x-show="peerPage.loading" aria-hidden="true">
                <td colspan="14"><div class="skeleton-table-cell"></div></td>
              </tr>
              <tr class="skeleton-card" x-show="peerPage.loading" aria-hidden="true">
                <td colspan="14"><div class="skeleton-table-cell"></div></td>
              </tr>
              <tr x-show="peerPage.hasMore"><td colspan="14"><div class="page-sentinel" x-init="$nextTick(() => observePageSentinel('peer', $el))"></div></td></tr>
              <tr x-show="peers.length === 0 && !peerPage.loading"><td colspan="14">No peers</td></tr>
            </tbody>
          </table>
        </div>
      </div>
      <div class="panel">
        <h2>Metrics</h2>
        <div class="grid">
          <div class="metric"><div class="label">Inbound Sessions</div><div class="value" x-text="p2pMetrics.inbound_sessions_started ?? 0"></div></div>
          <div class="metric"><div class="label">Inbound Rejects</div><div class="value" x-text="p2pMetrics.inbound_sessions_rejected ?? 0"></div></div>
          <div class="metric"><div class="label">Outbound Attempts</div><div class="value" x-text="p2pMetrics.outbound_connect_attempts ?? 0"></div></div>
          <div class="metric"><div class="label">Connect Failures</div><div class="value" x-text="p2pMetrics.outbound_connect_failures ?? 0"></div></div>
          <div class="metric"><div class="label">Session Failures</div><div class="value" x-text="p2pMetrics.session_failures ?? 0"></div></div>
          <div class="metric"><div class="label">Parse Errors</div><div class="value" x-text="p2pMetrics.parse_errors ?? 0"></div></div>
          <div class="metric"><div class="label">Empty Frames</div><div class="value" x-text="p2pMetrics.empty_frames ?? 0"></div></div>
          <div class="metric"><div class="label">Self Rejects</div><div class="value" x-text="p2pMetrics.self_peer_rejections ?? 0"></div></div>
          <div class="metric"><div class="label">Self Skips</div><div class="value" x-text="p2pMetrics.self_peer_skips ?? 0"></div></div>
          <div class="metric"><div class="label">Received</div><div class="value" x-text="p2pMetrics.envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Bytes In</div><div class="value" x-text="p2pMetrics.bytes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Status Rx</div><div class="value" x-text="p2pMetrics.peer_status_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Hello Rx</div><div class="value" x-text="p2pMetrics.hello_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Inventory Rx</div><div class="value" x-text="p2pMetrics.inventory_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Data Rx</div><div class="value" x-text="p2pMetrics.data_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Control Rx</div><div class="value" x-text="p2pMetrics.control_envelopes_received ?? 0"></div></div>
        </div>
        <div class="metric-context">
          <div class="tx-field"><span class="tx-label">Last Failure</span><span class="tx-value text" x-text="p2pMetrics.last_session_failure || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Empty</span><span class="tx-value text" x-text="p2pMetrics.last_empty_frame_remote || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Parse</span><span class="tx-value text" x-text="p2pMetrics.last_parse_error || '-'"></span></div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'chain'">
      <div class="explorer-shell">
        <div class="block-rail-wrap">
          <div class="block-rail-head">
            <h2>Blocks</h2>
            <div class="muted"><span x-text="blocks.length"></span> loaded</div>
          </div>
          <div class="block-rail" x-ref="blockRail" @scroll.debounce.200ms="maybeLoadOlderBlocks($event)">
            <template x-for="block in blocks" :key="block.hash">
              <button class="block-card" :class="{ selected: selectedBlock?.hash === block.hash, 'new-block': newBlockHashes.has(block.hash) }" @click="selectBlock(block)" type="button">
                <div class="block-height" x-text="block.height"></div>
                <div class="block-meta">
                  <span x-text="burnCountLabel(block)"></span>
                  <span x-text="transferCountLabel(block)"></span>
                  <span x-text="mineCountLabel(block)"></span>
                </div>
                <div class="block-miner" x-text="blockFinalizerLabel(block)"></div>
              </button>
            </template>
            <template x-if="loadingOlder">
              <div class="block-card skeleton-card" aria-hidden="true">
                <div class="skeleton-line short"></div>
                <div class="skeleton-line medium"></div>
                <div class="skeleton-line long"></div>
              </div>
            </template>
            <div class="page-sentinel block-page-sentinel" x-show="hasMoreBlocks" x-init="$nextTick(() => observeBlockSentinel($el))"></div>
          </div>
        </div>

        <section class="panel">
          <h2>Block Detail</h2>
          <template x-if="selectedBlock">
            <div class="detail-grid">
              <div>
                <div class="detail-kv"><div class="key">Height</div><div x-text="selectedBlock.height"></div></div>
                <div class="detail-kv"><div class="key">Hash</div><code x-text="selectedBlock.hash"></code></div>
                <div class="detail-kv"><div class="key">Previous</div><code x-text="short(selectedBlock.prev_hash)"></code></div>
                <div class="detail-kv">
                  <div class="key">Finalizer</div>
                  <button class="detail-link" type="button" @click="openBurnLeaderRanksModal(selectedBlock)" title="Burn leader ranks">
                    <code x-text="short(selectedBlock.miner)"></code>
                  </button>
                </div>
                <div class="detail-kv"><div class="key">Mode</div><div x-text="selectedBlock.finalizer_mode === 'recovery' ? 'Recovery' : `Rank ${selectedBlock.finalizer_rank ?? 0}`"></div></div>
                <div class="detail-kv"><div class="key">Reward</div><div>IUNA <span x-text="amountLabel(selectedBlock.reward)"></span></div></div>
                <div class="detail-kv"><div class="key">Burns</div><div x-text="blockBurnCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Transfers</div><div x-text="blockTransferCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Total Burned</div><div>IUNA <span x-text="amountLabel(blockBurned(selectedBlock))"></span></div></div>
                <div class="detail-kv"><div class="key">VDF</div><div><span x-text="selectedBlock.vdf_rounds"></span> rounds</div></div>
              </div>
              <div class="tx-list">
                <h3>Transactions</h3>
                <template x-for="tx in selectedBlock.transactions" :key="tx.signature">
                  <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })">
                    <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                    <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                    <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                    <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(txFrom(tx))"></code></div>
                    <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(txTo(tx))"></code></div>
                    <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                    <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                    <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                  </div>
                </template>
                <div class="muted" x-show="selectedBlock.transactions.length === 0">No transactions</div>
                <template x-if="selectedBlock.revealed_transactions?.length">
                  <div class="tx-list">
                    <h3>Revealed</h3>
                    <template x-for="tx in selectedBlock.revealed_transactions" :key="tx.signature">
                      <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Revealed', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Revealed', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Revealed', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })">
                        <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(txFrom(tx))"></code></div>
                        <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(txTo(tx))"></code></div>
                        <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                      </div>
                    </template>
                  </div>
                </template>
              </div>
            </div>
          </template>
          <div class="muted" x-show="!selectedBlock">Select a block</div>
        </section>

        <section class="panel mempool-panel" x-show="mempool.length > 0 || mempoolPage.loading">
          <h2>Mempool</h2>
          <div class="mempool-strip">
            <template x-for="tx in mempool" :key="tx.signature">
              <div class="mempool-item" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Mempool' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Mempool' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Mempool' })">
                <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                <div class="tx-field" x-show="!isBlindedMempoolItem(tx)"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                <div class="tx-field" x-show="!isBlindedMempoolItem(tx)"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(txFrom(tx))"></code></div>
                <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(txTo(tx))"></code></div>
                <div class="tx-field" x-show="isBlindedMempoolItem(tx)"><span class="tx-label">Commitment</span><code class="tx-value hash" x-text="short(tx.commitment || tx.signature)"></code></div>
                <div class="tx-field" x-show="tx.encrypted_size || tx.encryptedSize"><span class="tx-label">Bytes</span><span class="tx-value number" x-text="tx.encrypted_size || tx.encryptedSize"></span></div>
                <div class="tx-field" x-show="tx.expires_at_height || tx.expiresAtHeight"><span class="tx-label">Expires</span><span class="tx-value number" x-text="tx.expires_at_height || tx.expiresAtHeight"></span></div>
                <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                <div class="tx-field" x-show="!isBlindedMempoolItem(tx)"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
              </div>
            </template>
            <template x-if="mempoolPage.loading">
              <div class="mempool-item skeleton-card" aria-hidden="true">
                <div class="skeleton-line short"></div>
                <div class="skeleton-line medium"></div>
                <div class="skeleton-line long"></div>
              </div>
            </template>
            <div class="page-sentinel" x-show="mempoolPage.hasMore" x-init="$nextTick(() => observePageSentinel('mempool', $el))"></div>
          </div>
        </section>
      </div>
    </section>
    <section x-show="tab === 'metrics'">
      <div class="metrics-shell">
        <div class="metrics-head">
          <h2>Metrics</h2>
          <div class="segmented metrics-range" role="group" aria-label="Metrics block range">
            <button type="button" :class="{ active: metricsRange === 100 }" @click="setMetricsRange(100)">Last 100</button>
            <button type="button" :class="{ active: metricsRange === 1000 }" @click="setMetricsRange(1000)">Last 1000</button>
            <button type="button" :class="{ active: metricsRange === 'all' }" @click="setMetricsRange('all')">All</button>
          </div>
        </div>
        <div class="metrics-summary">
          <div class="metric"><div class="label">Latest block</div><div class="value" x-text="metricsLatest().height ?? '-'"></div></div>
          <div class="metric"><div class="label">Supply</div><div class="value" x-text="metricAmountLabel(metricsLatest().circulatingSupply)"></div></div>
          <div class="metric"><div class="label">Total burned</div><div class="value" x-text="metricAmountLabel(metricsLatest().totalBurnedAmount)"></div></div>
          <div class="metric"><div class="label">Difficulty</div><div class="value" x-text="metricsLatest().mineDifficultyBits ?? '-'"></div></div>
        </div>
        <div class="metrics-empty" x-show="metricsCharts().length === 0">No metrics collected yet</div>
        <div class="metrics-grid">
          <template x-for="chart in metricsCharts()" :key="chart.id">
            <article class="metric-chart-card">
              <div class="metric-chart-head">
                <h3 class="metric-chart-title" x-text="chart.title"></h3>
                <div class="metric-chart-value" x-text="metricLatestValueLabel(chart)"></div>
              </div>
              <div class="metric-chart-frame">
                <div class="metric-chart-y-axis">
                  <template x-for="tick in metricYAxisTicks(chart)" :key="`${chart.id}-y-${tick}`">
                    <span class="metric-chart-axis-label" :style="metricYAxisLabelStyle(chart, tick)" x-text="metricAxisValueLabel(chart, tick)"></span>
                  </template>
                </div>
                <div class="metric-chart-plot" @mousemove="setMetricHoverFromPlot(chart, $event)" @mouseleave="clearMetricHover(chart)">
                  <svg class="metric-chart-svg" viewBox="0 0 300 148" preserveAspectRatio="none" role="img" :aria-label="chart.title">
                    <path class="metric-chart-gridline" :d="metricGridPath(chart)"></path>
                    <line class="metric-chart-axis" x1="4" y1="8" x2="4" y2="132"></line>
                    <line class="metric-chart-axis" x1="4" y1="132" x2="296" y2="132"></line>
                    <polyline class="metric-chart-line" :points="metricChartPoints(chart)"></polyline>
                  </svg>
                  <div class="metric-chart-points">
                    <template x-for="marker in metricChartPointMarkers(chart)" :key="`${chart.id}-point-${marker.height}`">
                      <button class="metric-chart-point-hit" type="button" :class="{ 'is-active': metricHover?.chartId === chart.id && metricHover?.height === marker.height }" :style="metricPointStyle(marker)" :title="marker.label" @focus="setMetricHover(chart, marker)" @blur="clearMetricHover(chart)" :aria-label="marker.label"></button>
                    </template>
                  </div>
                  <template x-if="metricHover?.chartId === chart.id">
                    <div class="metric-chart-tooltip" :style="metricTooltipStyle(chart)" x-text="metricTooltipLabel(chart)"></div>
                  </template>
                </div>
                <div class="metric-chart-x-axis">
                  <template x-for="tick in metricXAxisTicks(chart)" :key="`${chart.id}-x-${tick}`">
                    <span class="metric-chart-axis-label" :style="metricXAxisLabelStyle(chart, tick)" x-text="`#${tick}`"></span>
                  </template>
                </div>
              </div>
            </article>
          </template>
        </div>
      </div>
    </section>
    <section x-show="tab === 'settings'">
      <div class="settings-grid">
        <div class="panel">
          <div class="panel-head">
            <h2>Settings</h2>
            <span class="pill" x-text="advancedMode() ? 'Node mode' : 'Wallet mode'"></span>
          </div>
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Mode</div>
              <div class="muted" x-text="advancedMode() ? 'Node mode shows mining and peer controls.' : 'Wallet mode keeps the interface focused on wallet and chain views.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: advancedMode() }">
              <input type="checkbox" :checked="advancedMode()" @change="setUiMode($event.target.checked ? 'advanced' : 'basic')">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="advancedMode() ? 'Node' : 'Wallet'"></span>
            </label>
          </div>
        </div>
        <div class="panel">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Keep track of metrics</div>
              <div class="muted" x-text="keepTrackOfMetrics ? 'Metrics are stored per block.' : 'Metrics storage is off.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: keepTrackOfMetrics }">
              <input type="checkbox" :checked="keepTrackOfMetrics" @change="setKeepTrackOfMetrics($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="keepTrackOfMetrics ? 'On' : 'Off'"></span>
            </label>
          </div>
        </div>
        <div class="panel" x-show="advancedMode()">
          <h3>Node Networking</h3>
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Public node</div>
              <div class="muted" x-text="p2pAcceptInbound ? 'Accepting inbound P2P connections.' : 'Outbound-only P2P; no inbound port is open.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: p2pAcceptInbound }">
              <input type="checkbox" :checked="p2pAcceptInbound" @change="setP2pAcceptInbound($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="p2pAcceptInbound ? 'Public' : 'Private'"></span>
            </label>
          </div>
          <form class="settings-form public-p2p-form" x-show="p2pAcceptInbound" x-transition @submit.prevent="saveP2pAnnounce">
            <label>Public P2P address<input x-model="p2pAnnounceAddr" @input="p2pAnnounceDirty = true" placeholder="203.0.113.10:9444"></label>
            <div class="muted">Use this only when TCP port 9444 is reachable from the internet.</div>
            <div class="setup-actions"><button class="primary" type="submit">Save</button></div>
          </form>
        </div>
        <div class="panel">
          <h3>Change Password</h3>
          <div class="setup-feedback" :class="settingsFeedback?.kind" x-show="settingsFeedback" x-transition x-text="settingsFeedback?.message"></div>
          <form class="settings-form" @submit.prevent="changePassword">
            <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
            <label>Current password<input x-model="settingsOldPassword" type="password" autocomplete="current-password" required></label>
            <label>New password<input x-model="settingsNewPassword" type="password" autocomplete="new-password" minlength="12" required></label>
            <label>Confirm new password<input x-model="settingsPasswordConfirm" type="password" autocomplete="new-password" minlength="12" required></label>
            <div class="setup-actions"><button class="primary" type="submit">Change password</button></div>
          </form>
        </div>
      </div>
    </section>
    </main>
  </div>
  <div class="setup-overlay" x-show="showingAuth()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="auth-title">
    <section class="setup-modal auth-form">
      <div class="setup-modal-head">
        <div class="setup-welcome">iuna Access</div>
        <h2 id="auth-title" x-text="auth.configured ? 'Unlock iuna' : 'Set Password'"></h2>
        <div class="setup-copy" x-show="!auth.configured">Choose a local password before wallet setup continues.</div>
        <div class="setup-copy" x-show="auth.configured">Enter the local password to unlock this node.</div>
      </div>
      <div class="setup-feedback" :class="authFeedback?.kind" x-show="authFeedback" x-transition x-text="authFeedback?.message"></div>
      <form x-show="!auth.configured" @submit.prevent="setupPassword">
        <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
        <label>Password<input x-model="authPassword" type="password" autocomplete="new-password" minlength="12" required></label>
        <label>Confirm password<input x-model="authPasswordConfirm" type="password" autocomplete="new-password" minlength="12" required></label>
        <div class="setup-actions"><button class="primary" type="submit">Set password</button></div>
      </form>
      <form x-show="auth.configured && !auth.authenticated" @submit.prevent="login">
        <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
        <label>Password<input x-model="loginPassword" type="password" autocomplete="current-password" required></label>
        <div class="setup-actions"><button class="primary" type="submit">Unlock</button></div>
      </form>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showWalletUtxos" x-transition.opacity @click.self="closeWalletUtxosModal()" role="dialog" aria-modal="true" aria-labelledby="wallet-utxos-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="wallet-utxos-title">Wallet UTXOs</h2>
          <div class="tx-field"><span class="tx-label">Total</span><span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span></div>
        </div>
        <button type="button" @click="closeWalletUtxosModal">Close</button>
      </div>
      <div class="utxo-list">
        <template x-for="utxo in walletUtxos" :key="`${utxo.outpoint.txid}:${utxo.outpoint.index}`">
          <div class="wallet-utxo-row">
            <div class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">IUNA <span x-text="amountLabel(utxo.amount)"></span></span></div>
            <div class="tx-field"><span class="tx-label">Outpoint</span><code class="tx-value hash" x-text="txInputOutpoint({ outpoint: utxo.outpoint })"></code></div>
            <div class="tx-field"><span class="tx-label">Address</span><code class="tx-value hash" x-text="utxo.address"></code></div>
          </div>
        </template>
        <div class="dataset-loader" x-show="walletUtxoPage.loading" aria-hidden="true">
          <div class="wallet-utxo-row skeleton-card"><div class="skeleton-line medium"></div><div class="skeleton-line long"></div></div>
          <div class="wallet-utxo-row skeleton-card"><div class="skeleton-line short"></div><div class="skeleton-line long"></div></div>
        </div>
        <div class="page-sentinel" x-show="walletUtxoPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletUtxo', $el))"></div>
        <div class="tx-modal-empty" x-show="walletUtxos.length === 0 && !walletUtxoPage.loading">No wallet UTXOs</div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showPowDifficultyInfo" x-transition.opacity @click.self="closePowDifficultyInfo()" role="dialog" aria-modal="true" aria-labelledby="pow-difficulty-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="pow-difficulty-title">PoW Difficulty</h2>
        </div>
        <button type="button" @click="closePowDifficultyInfo">Close</button>
      </div>
      <div class="info-copy">
        <p>Difficulty is adjusted to target about one mine action per block.</p>
        <div class="info-facts">
          <div class="info-fact"><div class="label">Window</div><div class="value">10 blocks</div></div>
          <div class="info-fact"><div class="label">Target</div><div class="value">10 mine actions</div></div>
          <div class="info-fact"><div class="label">Max step</div><div class="value">2 bits</div></div>
        </div>
        <p>If a window includes more mine actions than the target, difficulty rises. If it includes fewer, difficulty falls. The initial difficulty is 12 bits.</p>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedBurnLeaderBlock" x-transition.opacity @click.self="closeBurnLeaderRanksModal()" role="dialog" aria-modal="true" aria-labelledby="burn-ranks-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="burn-ranks-title" x-text="burnLeaderRanksTitle(selectedBurnLeaderBlock)"></h2>
          <div class="tx-field"><span class="tx-label">Finalizer</span><code class="tx-value hash" x-text="selectedBurnLeaderBlock ? selectedBurnLeaderBlock.miner : '-'"></code></div>
        </div>
        <button type="button" @click="closeBurnLeaderRanksModal">Close</button>
      </div>
      <div class="rank-list">
        <template x-for="rank in burnLeaderRanks(selectedBurnLeaderBlock)" :key="`${selectedBurnLeaderBlock.hash}-${rank.rank}-${rank.ticket_id ?? rank.ticketId}`">
          <div class="rank-row">
            <div class="rank-number" x-text="burnLeaderRankLabel(rank)"></div>
            <div class="rank-details">
              <div class="tx-field"><span class="tx-label">Owner</span><code class="tx-value hash" x-text="rank.owner"></code></div>
              <div class="tx-field"><span class="tx-label">Burn</span><span class="tx-value money">IUNA <span x-text="amountLabel(rank.amount)"></span></span></div>
              <div class="tx-field"><span class="tx-label">Ticket</span><code class="tx-value hash" x-text="short(rank.ticket_id ?? rank.ticketId)"></code></div>
              <div class="tx-field"><span class="tx-label">Eligible</span><span class="tx-value number" x-text="burnLeaderEligibilityLabel(rank)"></span></div>
            </div>
          </div>
        </template>
        <div class="tx-modal-empty" x-show="burnLeaderRanks(selectedBurnLeaderBlock).length === 0">No burn leader ranks</div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedTransaction" x-transition.opacity @click.self="closeTransactionModal()" role="dialog" aria-modal="true" aria-labelledby="tx-modal-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill" :class="selectedTransaction?.tx?.kind" x-text="selectedTransaction?.tx?.kind"></span>
          <h2 id="tx-modal-title">Transaction</h2>
          <code class="tx-value hash" x-text="selectedTransaction?.tx?.signature || '-'"></code>
        </div>
        <button type="button" @click="closeTransactionModal">Close</button>
      </div>
      <div class="tx-modal-summary">
        <div class="tx-field"><span class="tx-label">Source</span><span class="tx-value text" x-text="selectedTransactionLabel()"></span></div>
        <div class="tx-field" x-show="!isBlindedMempoolItem(selectedTransaction?.tx)"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(selectedTransaction?.tx || {}))"></span></span></div>
        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">IUNA <span x-text="amountLabel(selectedTransaction?.tx?.fee ?? 0)"></span></span></div>
        <div class="tx-field" x-show="!isBlindedMempoolItem(selectedTransaction?.tx)"><span class="tx-label">From</span><code class="tx-value hash" x-text="txFrom(selectedTransaction?.tx || {})"></code></div>
        <div class="tx-field" x-show="txTo(selectedTransaction?.tx || {})"><span class="tx-label">To</span><code class="tx-value hash" x-text="txTo(selectedTransaction?.tx || {})"></code></div>
        <div class="tx-field" x-show="isBlindedMempoolItem(selectedTransaction?.tx)"><span class="tx-label">Commitment</span><code class="tx-value hash" x-text="selectedTransaction?.tx?.commitment || selectedTransaction?.tx?.signature || '-'"></code></div>
        <div class="tx-field" x-show="selectedTransaction?.tx?.encrypted_size || selectedTransaction?.tx?.encryptedSize"><span class="tx-label">Encrypted Bytes</span><span class="tx-value number" x-text="selectedTransaction?.tx?.encrypted_size || selectedTransaction?.tx?.encryptedSize"></span></div>
        <div class="tx-field" x-show="selectedTransaction?.tx?.expires_at_height || selectedTransaction?.tx?.expiresAtHeight"><span class="tx-label">Expires</span><span class="tx-value number" x-text="selectedTransaction?.tx?.expires_at_height || selectedTransaction?.tx?.expiresAtHeight"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Difficulty</span><span class="tx-value number" x-text="txDifficultyBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number" x-text="txProofBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="txProofHash(selectedTransaction?.tx) || '-'"></code></div>
      </div>
      <div class="utxo-flow">
        <div class="utxo-column">
          <h3>Inputs</h3>
          <template x-for="(input, index) in txInputs(selectedTransaction?.tx || {})" :key="txInputKey(input, index)">
            <div class="utxo-node">
              <div class="utxo-node-label"><span>Input <span x-text="index + 1"></span></span><span>spent</span></div>
              <div class="utxo-node-ref" x-text="txInputOutpoint(input)"></div>
              <div class="tx-field"><span class="tx-label">Value</span><span class="tx-value money" x-text="txInputAmountLabel(input)"></span></div>
              <div class="tx-field"><span class="tx-label">Owner</span><code class="tx-value hash" x-text="input.owner"></code></div>
              <div class="tx-field"><span class="tx-label">Sig</span><code class="tx-value hash" x-text="short(input.signature)"></code></div>
            </div>
          </template>
          <div class="tx-modal-empty" x-show="txInputs(selectedTransaction?.tx || {}).length === 0">No inputs</div>
        </div>
        <div class="utxo-arrow" aria-hidden="true">&rarr;</div>
        <div class="utxo-column">
          <h3>Outputs</h3>
          <template x-for="(output, index) in txVisualOutputs(selectedTransaction?.tx || {})" :key="txOutputKey(output, index)">
            <div class="utxo-node" :class="{ burned: output.kind === 'burned', fee: output.kind === 'fee' }">
              <div class="utxo-node-label"><span x-text="output.label"></span><span x-text="output.kind"></span></div>
              <div class="utxo-node-amount">IUNA <span x-text="amountLabel(output.amount)"></span></div>
              <template x-if="output.address">
                <div class="tx-field"><span class="tx-label">To</span><code class="tx-value hash" x-text="output.address"></code></div>
              </template>
              <template x-if="output.detail">
                <div class="tx-field"><span class="tx-label" x-text="output.detailLabel"></span><code class="tx-value hash" x-text="output.detail"></code></div>
              </template>
            </div>
          </template>
          <div class="tx-modal-empty" x-show="txVisualOutputs(selectedTransaction?.tx || {}).length === 0">No outputs</div>
        </div>
      </div>
    </section>
  </div>
  <div class="setup-overlay" x-show="showingSetup()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="setup-title">
    <section class="setup-modal">
      <div class="setup-modal-head">
        <div class="setup-welcome">Welcome to iuna</div>
        <h2 id="setup-title">Initial Setup</h2>
        <div class="setup-copy">Connect this node to the network, then set up the local wallet.</div>
      </div>
      <div class="setup-feedback" :class="setupFeedback?.kind" x-show="setupFeedback" x-transition x-text="setupFeedback?.message"></div>
      <div class="setup-grid">
        <div class="setup-section setup-node-mode">
          <div class="panel-head">
            <h3>Mode</h3>
            <span class="pill">Change later in Settings</span>
          </div>
          <div class="segmented setup-mode-picker" role="tablist" aria-label="Initial node mode">
            <button type="button" :class="{ active: setupNodeMode === 'wallet' }" @click="selectSetupNodeMode('wallet')">Wallet</button>
            <button type="button" :class="{ active: setupNodeMode === 'non-listening' }" @click="selectSetupNodeMode('non-listening')">Non-listening node</button>
            <button type="button" :class="{ active: setupNodeMode === 'listening' }" @click="selectSetupNodeMode('listening')">Listening node</button>
          </div>
          <div class="setup-network-copy" x-text="setupNodeModeCopy()"></div>
        </div>
        <div class="setup-section setup-network">
          <div class="panel-head">
            <h3>Network</h3>
            <a class="setup-network-link" href="https://github.com/iuna-labs/iuna/blob/main/KNOWN_NODES.txt" target="_blank" rel="noreferrer">Known nodes</a>
          </div>
          <div class="setup-network-row">
            <label><span x-text="setupRequiresPeer() ? 'Bootstrap peer (required)' : 'Bootstrap peer'"></span><input x-model="setupPeerAddress" placeholder="iuna.jhx.app:9444"></label>
          </div>
          <div class="setup-network-copy" x-text="setupRequiresPeer() ? 'A bootstrap peer is required before this node can join the network. Known nodes help discovery; they do not control your wallet or decide valid blocks.' : 'You can add a bootstrap peer now or later from the P2P screen. Known nodes help discovery; they do not control your wallet or decide valid blocks.'"></div>
        </div>
        <div class="setup-section setup-wallet-section seed-panel">
          <div class="panel-head">
            <h3>Wallet</h3>
          </div>
          <div class="segmented" role="tablist" aria-label="Wallet setup mode">
            <button type="button" :class="{ active: setupWalletMode === 'create' }" @click="selectSetupWalletMode('create')">Create</button>
            <button type="button" :class="{ active: setupWalletMode === 'import' }" @click="selectSetupWalletMode('import')">Import</button>
          </div>
          <div x-show="setupWalletMode === 'create'" class="seed-panel">
            <div class="setup-field">
              <div class="setup-field-label">Address</div>
              <div class="address-box setup-address-box">
                <code x-text="setupAddress()"></code>
                <button type="button" @click="copyAddress">Copy</button>
              </div>
            </div>
            <template x-if="setupSeedWords().length > 0 && setupSeedStep === 'write'">
              <div class="seed-panel">
                <div class="setup-field">
                  <div class="setup-field-label">Recovery phrase</div>
                  <div class="seed-grid">
                    <template x-for="(word, index) in setupSeedWords()" :key="index">
                      <div class="seed-word">
                        <span class="index" x-text="index + 1"></span>
                        <span class="word" x-text="word"></span>
                      </div>
                    </template>
                  </div>
                </div>
                <div class="setup-actions">
                  <button type="button" class="subtle" @click="generateSetupSeed">Regenerate</button>
                  <button type="button" class="subtle" x-show="setupWallet.dev_verify_bypass" @click="skipSeedVerificationForDev">Skip verification</button>
                  <button type="button" class="primary" @click="beginSeedVerification">I wrote it down</button>
                </div>
              </div>
            </template>
            <template x-if="setupSeedWords().length === 0">
              <div class="seed-panel">
                <div class="muted">This wallet does not have a recovery phrase yet.</div>
                <button type="button" class="primary" @click="generateSetupSeed">Generate recovery phrase</button>
              </div>
            </template>
            <template x-if="setupSeedStep === 'verify'">
              <div class="seed-panel">
                <div class="verify-grid">
                  <template x-for="challenge in verifyChallenges" :key="challenge.index">
                    <label>
                      <span>Word <span x-text="challenge.position"></span></span>
                      <input x-model="verifyAnswers[challenge.index]" autocomplete="off">
                    </label>
                  </template>
                </div>
                <div class="setup-actions">
                  <button type="button" class="subtle" @click="setupSeedStep = 'write'">Back</button>
                  <button type="button" class="primary" @click="verifyGeneratedSeed">Verify</button>
                </div>
              </div>
            </template>
            <template x-if="setupSeedStep === 'verified' && walletVerified">
              <div class="setup-status">Recovery phrase verified</div>
            </template>
          </div>
          <div x-show="setupWalletMode === 'import'" class="seed-panel">
            <form @submit.prevent="importSetupSeed">
              <label>Recovery phrase<textarea x-model="importSeedPhrase" autocomplete="off" spellcheck="false" placeholder="24 words, separated by spaces or new lines"></textarea></label>
              <button class="primary" type="submit">Import</button>
            </form>
            <template x-if="walletVerified">
              <div class="setup-status">Recovery phrase imported</div>
            </template>
          </div>
        </div>
      </div>
      <div class="setup-actions">
        <button class="primary" type="button" :disabled="!setupCanContinue()" @click="completeSetup">Continue</button>
      </div>
    </section>
  </div>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Method, Request, StatusCode, header},
        middleware,
        routing::{get, post},
    };
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    use crate::{
        adapters::{
            chain_store::SqliteChainStore, config_store, config_store::UiConfig,
            p2p::GossipNetwork, wallet_store,
        },
        app::{GossipEnvelope, NodeCore, PeerBook, PeerDirection, PeerInfo, StratumStatus},
        domain::{
            Amount, BlindedReveal, BlindedTransaction, Block, ChainSnapshot, LaunchProfile, Ledger,
            MICRO_IUNA, MINE_FINALIZER_FEE, OutPoint, Transaction, Wallet,
        },
    };

    use super::{
        AUTH_COOKIE_NAME, HttpState, PEER_STALE_AFTER_MS, TransferForm, WalletTransactionFilters,
        WalletTransactionsQuery, api_auth_change_password_form, api_auth_login_form,
        api_auth_setup_form, api_auth_status, auth_client_key, dev_seed_verify_bypass_allowed,
        hash_password, hex_encode, pbkdf2_sha256, persist_burn_settings_config,
        persist_pow_mining_config, require_auth_middleware, required_fee_per_byte_burn,
        same_origin_request, validate_password, validate_transfer_form, verify_password,
        wallet_transaction_rows, wallet_utxo_rows,
    };

    #[test]
    fn dev_seed_verify_bypass_requires_env_flag() {
        assert!(dev_seed_verify_bypass_allowed(true));
        assert!(!dev_seed_verify_bypass_allowed(false));
    }

    #[test]
    fn mempool_ui_items_can_represent_blinded_transactions_and_reveals() {
        let commitment = "a".repeat(64);
        let blinded = BlindedTransaction {
            commitment: commitment.clone(),
            fee: 7,
            encrypted_size: 123,
            expires_at_height: 42,
            nonce: "00".repeat(12),
            ciphertext: "11".repeat(123),
            payload_hash: "b".repeat(64),
        };
        let reveal = BlindedReveal {
            commitment: commitment.clone(),
            key: "22".repeat(32),
        };

        let blinded_row = super::ui_blinded_transaction(&blinded);
        let reveal_row = super::ui_blinded_reveal(&reveal);

        assert_eq!(blinded_row.kind, "blinded");
        assert_eq!(blinded_row.signature, commitment);
        assert_eq!(blinded_row.encrypted_size, Some(123));
        assert_eq!(blinded_row.expires_at_height, Some(42));
        assert_eq!(reveal_row.kind, "reveal");
        assert_eq!(reveal_row.commitment, blinded_row.commitment);
    }

    #[test]
    fn password_policy_rejects_short_or_excessive_passwords() {
        let short = validate_password("too-short").unwrap_err();
        assert!(short.to_string().contains("at least 12"));

        let long_password = "x".repeat(1025);
        let long = validate_password(&long_password).unwrap_err();
        assert!(long.to_string().contains("too long"));

        validate_password("correct horse battery staple").unwrap();
    }

    #[test]
    fn password_hash_round_trips_without_storing_plaintext() {
        let password = "correct horse battery staple";
        let encoded = hash_password(password).unwrap();

        assert!(!encoded.contains(password));
        assert!(verify_password(password, &encoded).unwrap());
        assert!(!verify_password("wrong horse battery staple", &encoded).unwrap());
    }

    #[test]
    fn pbkdf2_sha256_matches_known_vectors() {
        let one_iteration = pbkdf2_sha256(b"password", b"salt", 1);
        assert_eq!(
            hex_encode(one_iteration),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );

        let two_iterations = pbkdf2_sha256(b"password", b"salt", 2);
        assert_eq!(
            hex_encode(two_iterations),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn same_origin_check_accepts_forwarded_host_and_rejects_cross_site_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:18661".parse().unwrap());
        headers.insert("x-forwarded-host", "iuna.example".parse().unwrap());
        headers.insert(header::ORIGIN, "https://iuna.example".parse().unwrap());
        assert!(same_origin_request(&headers));

        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!same_origin_request(&headers));
    }

    #[test]
    fn auth_client_key_trusts_forwarded_headers_only_from_private_or_local_peers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.99".parse().unwrap());
        let socket = Some("203.0.113.10:51234".parse().unwrap());

        assert_eq!(auth_client_key(&headers, socket), "203.0.113.10");
        assert_eq!(
            auth_client_key(&headers, Some("127.0.0.1:51234".parse().unwrap())),
            "198.51.100.99"
        );
        assert_eq!(
            auth_client_key(&headers, Some("10.42.1.12:51234".parse().unwrap())),
            "198.51.100.99"
        );
        assert_eq!(
            auth_client_key(&headers, Some("172.20.4.8:51234".parse().unwrap())),
            "198.51.100.99"
        );
        assert_eq!(auth_client_key(&headers, None), "198.51.100.99");
    }

    #[tokio::test]
    async fn protected_endpoints_require_authentication_setup() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                auth_password_hash: None,
                ..UiConfig::default()
            },
        )
        .await;
        let app = auth_test_app(state);

        let protected = http_request(app.clone(), Method::GET, "/api/protected", None, "").await;
        assert_eq!(protected.status, StatusCode::UNAUTHORIZED);
        assert!(protected.body.contains("authentication setup is required"));

        let status = http_request(app, Method::GET, "/api/auth/status", None, "").await;
        assert_eq!(status.status, StatusCode::OK);
        assert!(status.body.contains("\"configured\":false"));
    }

    #[tokio::test]
    async fn favicon_is_public_before_authentication_setup() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                auth_password_hash: None,
                ..UiConfig::default()
            },
        )
        .await;
        let app = auth_test_app(state);

        let response = http_request(app, Method::GET, "/favicon.ico", None, "").await;

        assert_eq!(response.status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn protected_endpoints_require_valid_session_after_authentication_setup() {
        let dir = tempfile::tempdir().unwrap();
        let password = "correct horse battery staple";
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                auth_password_hash: Some(hash_password(password).unwrap()),
                ..UiConfig::default()
            },
        )
        .await;
        let app = auth_test_app(state);

        let missing_cookie =
            http_request(app.clone(), Method::GET, "/api/protected", None, "").await;
        assert_eq!(missing_cookie.status, StatusCode::UNAUTHORIZED);
        assert!(missing_cookie.body.contains("authentication required"));

        let bad_cookie = http_request(
            app.clone(),
            Method::GET,
            "/api/protected",
            Some("iuna_session=bogus"),
            "",
        )
        .await;
        assert_eq!(bad_cookie.status, StatusCode::UNAUTHORIZED);

        let login = http_request(
            app.clone(),
            Method::POST,
            "/api/auth/login",
            None,
            "password=correct+horse+battery+staple",
        )
        .await;
        assert_eq!(login.status, StatusCode::OK);
        assert!(login.body.contains("\"ok\":true"));
        let cookie = set_cookie_pair(&login.headers);
        assert!(cookie.starts_with(AUTH_COOKIE_NAME));

        let protected = http_request(app, Method::GET, "/api/protected", Some(&cookie), "").await;
        assert_eq!(protected.status, StatusCode::OK);
        assert_eq!(protected.body, "protected");
    }

    #[tokio::test]
    async fn auth_posts_require_same_origin_headers() {
        let dir = tempfile::tempdir().unwrap();
        let password = "correct horse battery staple";
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                auth_password_hash: Some(hash_password(password).unwrap()),
                ..UiConfig::default()
            },
        )
        .await;
        let app = auth_test_app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/login")
                    .header(header::HOST, "127.0.0.1:18661")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("password=correct+horse+battery+staple"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains("same-origin request required"));
    }

    #[tokio::test]
    async fn login_authentication_locks_out_after_repeated_failures() {
        let dir = tempfile::tempdir().unwrap();
        let password = "correct horse battery staple";
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                auth_password_hash: Some(hash_password(password).unwrap()),
                ..UiConfig::default()
            },
        )
        .await;
        let client_a = "198.51.100.10";
        let client_b = "198.51.100.11";

        for _ in 0..super::AUTH_MAX_FAILED_ATTEMPTS {
            let error = super::login_auth_password(&state, "wrong horse battery staple", client_a)
                .await
                .unwrap_err();
            assert!(format!("{error:#}").contains("invalid password"));
        }

        let locked = super::login_auth_password(&state, password, client_a)
            .await
            .unwrap_err();
        assert!(format!("{locked:#}").contains("too many failed login attempts"));

        let other_client_cookie = super::login_auth_password(&state, password, client_b)
            .await
            .unwrap();
        assert!(other_client_cookie.starts_with(AUTH_COOKIE_NAME));

        state
            .auth_backoff
            .lock()
            .await
            .get_mut(client_a)
            .unwrap()
            .locked_until_ms = Some(crate::app::now_ms().saturating_sub(1));
        let cookie = super::login_auth_password(&state, password, client_a)
            .await
            .unwrap();
        assert!(cookie.starts_with(AUTH_COOKIE_NAME));
        assert!(!state.auth_backoff.lock().await.contains_key(client_a));
    }

    #[tokio::test]
    async fn password_setup_creates_session_for_protected_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(config_path.clone(), UiConfig::default()).await;
        let app = auth_test_app(state);

        let setup = http_request(
            app.clone(),
            Method::POST,
            "/api/auth/setup",
            None,
            "password=correct+horse+battery+staple",
        )
        .await;
        assert_eq!(setup.status, StatusCode::OK);
        assert!(setup.body.contains("\"ok\":true"));
        let cookie = set_cookie_pair(&setup.headers);

        let stored = config_store::load_or_create(&config_path).unwrap();
        assert!(stored.auth_password_hash.is_some());
        let protected = http_request(app, Method::GET, "/api/protected", Some(&cookie), "").await;
        assert_eq!(protected.status, StatusCode::OK);
        assert_eq!(protected.body, "protected");
    }

    #[tokio::test]
    async fn password_change_reencrypts_wallet_and_replaces_login_password() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let wallet_path = config_path.with_file_name("wallet.json");
        let old_password = "correct horse battery staple";
        let new_password = "new correct battery staple";
        let state = auth_test_state(
            config_path.clone(),
            UiConfig {
                auth_password_hash: Some(hash_password(old_password).unwrap()),
                setup_complete: true,
                ..UiConfig::default()
            },
        )
        .await;
        wallet_store::encrypt_existing_with_password(&wallet_path, old_password).unwrap();
        let app = auth_test_app(state);

        let login = http_request(
            app.clone(),
            Method::POST,
            "/api/auth/login",
            None,
            "password=correct+horse+battery+staple",
        )
        .await;
        assert_eq!(login.status, StatusCode::OK);
        let cookie = set_cookie_pair(&login.headers);

        let change = http_request(
            app.clone(),
            Method::POST,
            "/api/auth/change-password",
            Some(&cookie),
            "old_password=correct+horse+battery+staple&new_password=new+correct+battery+staple",
        )
        .await;
        assert_eq!(change.status, StatusCode::OK);
        assert!(change.body.contains("\"ok\":true"));

        let stored = config_store::load_or_create(&config_path).unwrap();
        let stored_hash = stored.auth_password_hash.unwrap();
        assert!(!verify_password(old_password, &stored_hash).unwrap());
        assert!(verify_password(new_password, &stored_hash).unwrap());
        assert!(wallet_store::load_with_password(&wallet_path, old_password).is_err());
        assert!(wallet_store::load_with_password(&wallet_path, new_password).is_ok());

        let old_login = http_request(
            app.clone(),
            Method::POST,
            "/api/auth/login",
            None,
            "password=correct+horse+battery+staple",
        )
        .await;
        assert!(old_login.body.contains("invalid password"));

        let new_login = http_request(
            app,
            Method::POST,
            "/api/auth/login",
            None,
            "password=new+correct+battery+staple",
        )
        .await;
        assert_eq!(new_login.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn peer_management_updates_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(
            config_path.clone(),
            UiConfig {
                setup_complete: true,
                ..UiConfig::default()
            },
        )
        .await;

        super::add_peer(&state, " 127.0.0.1:9445 ".to_string())
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert_eq!(config.peers, vec!["127.0.0.1:9445"]);
        assert_eq!(state.peers.lock().await.addresses(), vec!["127.0.0.1:9445"]);

        super::remove_peer(&state, "127.0.0.1:9445".to_string())
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(config.peers.is_empty());
        assert!(state.peers.lock().await.addresses().is_empty());
    }

    #[tokio::test]
    async fn p2p_announce_setting_persists_config_and_waits_for_public_node() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(
            config_path.clone(),
            UiConfig {
                setup_complete: true,
                ..UiConfig::default()
            },
        )
        .await;

        super::set_p2p_announce_addr(&state, " 203.0.113.10:9444 ".to_string())
            .await
            .unwrap();

        let config = config_store::load_or_create(&config_path).unwrap();
        assert_eq!(
            config.p2p_announce_addr.as_deref(),
            Some("203.0.113.10:9444")
        );
        match state.gossip.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(!peers.contains(&"203.0.113.10:9444".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }

        super::set_p2p_accept_inbound(&state, true).await.unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(config.p2p_accept_inbound);
        assert!(state.gossip.accepts_inbound().await);
        match state.gossip.peer_exchange().await {
            GossipEnvelope::PeerList { peers } => {
                assert!(peers.contains(&"203.0.113.10:9444".to_string()));
            }
            other => panic!("expected peer list, got {other:?}"),
        }

        super::set_p2p_accept_inbound(&state, false).await.unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(!config.p2p_accept_inbound);
        assert!(!state.gossip.accepts_inbound().await);

        super::set_p2p_announce_addr(&state, " ".to_string())
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(config.p2p_announce_addr.is_none());
    }

    #[tokio::test]
    async fn p2p_announce_setting_rejects_invalid_address() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;

        let error = super::set_p2p_announce_addr(&state, "not-an-address".to_string())
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("invalid P2P announce address"));
    }

    #[tokio::test]
    async fn setup_config_form_can_add_bootstrap_peer() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(config_path.clone(), UiConfig::default()).await;

        super::apply_config_form(
            &state,
            super::ConfigForm {
                setup_complete: true,
                peer: " iuna.jhx.app:9444 ".to_string(),
            },
        )
        .await
        .unwrap();

        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(config.setup_complete);
        assert_eq!(config.peers, vec!["iuna.jhx.app:9444"]);
        assert_eq!(
            state.peers.lock().await.addresses(),
            vec!["iuna.jhx.app:9444"]
        );
    }

    #[tokio::test]
    async fn setup_config_form_requires_peer_for_placeholder_chain() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(config_path.clone(), UiConfig::default()).await;

        let error = super::apply_config_form(
            &state,
            super::ConfigForm {
                setup_complete: true,
                peer: " ".to_string(),
            },
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("add a bootstrap peer"));
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(!config.setup_complete);
    }

    #[tokio::test]
    async fn peer_management_rejects_empty_and_inbound_removal() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;

        assert!(super::add_peer(&state, "   ".to_string()).await.is_err());
        state
            .peers
            .lock()
            .await
            .record_received("127.0.0.1:9555", 1);

        let result = super::remove_peer(&state, "127.0.0.1:9555".to_string()).await;
        assert!(result.is_err());
        assert_eq!(state.peers.lock().await.addresses(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn network_health_summarizes_sync_and_peer_errors() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;
        let status = state.node.lock().await.status();

        let isolated = super::network_health(&status, &[]);
        assert!(!isolated.ok);
        assert_eq!(isolated.state, "isolated");
        assert_eq!(isolated.local_height, 0);
        assert_eq!(isolated.best_known_height, 0);

        let mut clock_peers = PeerBook::from_addresses(vec![
            "127.0.0.1:9450".to_string(),
            "127.0.0.1:9451".to_string(),
        ]);
        clock_peers.record_status("127.0.0.1:9450", 0, "tip".to_string());
        clock_peers.record_status("127.0.0.1:9451", 0, "tip".to_string());
        clock_peers.record_clock_observation(
            "127.0.0.1:9450",
            PeerDirection::Outbound,
            10_500,
            10_000,
        );
        clock_peers.record_clock_observation(
            "127.0.0.1:9451",
            PeerDirection::Outbound,
            11 * 60 * 1_000,
            10_000,
        );
        let clock_health = super::network_health_at(&status, &clock_peers.list(), 10_000);
        assert_eq!(clock_health.network_time_offset_ms, Some(500));
        assert_eq!(clock_health.bad_clock_peers, 1);

        let syncing = super::network_health(
            &status,
            &[PeerInfo {
                address: "127.0.0.1:9445".to_string(),
                direction: PeerDirection::Outbound,
                messages_sent: 1,
                messages_received: 1,
                last_known_height: Some(3),
                last_known_tip_hash: Some("remote-tip".to_string()),
                last_clock_offset_ms: None,
                last_clock_offset_accepted: None,
                last_clock_observed_ms: None,
                last_error: None,
                last_contact_ms: Some(10_000),
                last_success_ms: Some(10_000),
                last_error_ms: None,
                misbehavior_score: 0,
                banned_until_ms: None,
                ban_reason: None,
            }],
        );
        assert!(!syncing.ok);
        assert_eq!(syncing.state, "syncing");
        assert_eq!(syncing.best_known_height, 3);
        assert_eq!(syncing.lag_blocks, 3);

        let peer_errors = super::network_health(
            &status,
            &[PeerInfo {
                address: "127.0.0.1:9446".to_string(),
                direction: PeerDirection::Outbound,
                messages_sent: 0,
                messages_received: 0,
                last_known_height: None,
                last_known_tip_hash: None,
                last_clock_offset_ms: None,
                last_clock_offset_accepted: None,
                last_clock_observed_ms: None,
                last_error: Some("connection refused".to_string()),
                last_contact_ms: Some(10_000),
                last_success_ms: None,
                last_error_ms: Some(10_000),
                misbehavior_score: 1,
                banned_until_ms: None,
                ban_reason: Some("connection refused".to_string()),
            }],
        );
        assert!(!peer_errors.ok);
        assert_eq!(peer_errors.state, "peer errors");
        assert_eq!(
            peer_errors.last_error.as_deref(),
            Some("127.0.0.1:9446: connection refused")
        );

        let stale = super::network_health_at(
            &status,
            &[PeerInfo {
                address: "127.0.0.1:9447".to_string(),
                direction: PeerDirection::Outbound,
                messages_sent: 1,
                messages_received: 1,
                last_known_height: Some(0),
                last_known_tip_hash: Some("tip".to_string()),
                last_clock_offset_ms: None,
                last_clock_offset_accepted: None,
                last_clock_observed_ms: None,
                last_error: None,
                last_contact_ms: Some(1),
                last_success_ms: Some(1),
                last_error_ms: None,
                misbehavior_score: 0,
                banned_until_ms: None,
                ban_reason: None,
            }],
            PEER_STALE_AFTER_MS + 2,
        );
        assert!(!stale.ok);
        assert_eq!(stale.state, "stale");
        assert_eq!(stale.stale_peers, 1);

        let banned = super::network_health_at(
            &status,
            &[PeerInfo {
                address: "127.0.0.1:9448".to_string(),
                direction: PeerDirection::Outbound,
                messages_sent: 0,
                messages_received: 0,
                last_known_height: None,
                last_known_tip_hash: None,
                last_clock_offset_ms: None,
                last_clock_offset_accepted: None,
                last_clock_observed_ms: None,
                last_error: Some("invalid block".to_string()),
                last_contact_ms: Some(10),
                last_success_ms: None,
                last_error_ms: Some(10),
                misbehavior_score: 3,
                banned_until_ms: Some(1_000),
                ban_reason: Some("invalid block".to_string()),
            }],
            20,
        );
        assert!(!banned.ok);
        assert_eq!(banned.state, "banned");
        assert_eq!(banned.banned_peers, 1);
    }

    #[test]
    fn wallet_transactions_include_old_confirmed_transfers_without_burns_or_explorer_pagination() {
        let alice = Wallet::from_seed("wallet-history-alice");
        let bob = Wallet::from_seed("wallet-history-bob");
        let carol = Wallet::from_seed("wallet-history-carol");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 100);
        allocations.insert(bob.address().to_string(), 100);
        allocations.insert(carol.address().to_string(), 100);
        let ledger = crate::domain::Ledger::new(allocations.clone(), 1);
        let old_received = ledger.build_transfer(&bob, alice.address(), 31, 0).unwrap();
        let pending_burn = ledger.build_burn(&alice, 2, 1).unwrap();
        let carol_transfer = ledger.build_transfer(&carol, bob.address(), 5, 0).unwrap();
        let carol_burn = ledger.build_burn(&carol, 1, 0).unwrap();
        let chain = vec![
            fake_block(30, vec![carol_transfer]),
            fake_block(31, vec![old_received.clone()]),
            fake_block(32, vec![carol_burn]),
        ];

        let snapshot = fake_snapshot(allocations, chain.clone());
        let outputs = super::known_output_index(&snapshot, std::slice::from_ref(&pending_burn));
        let rows = wallet_transaction_rows(
            alice.address(),
            vec![pending_burn.clone()],
            &chain,
            &outputs,
            WalletTransactionFilters::default(),
        );

        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].signature, pending_burn.signature());
        assert_eq!(rows[0].signature, old_received.signature());
        assert_eq!(rows[0].inputs[0].amount, Some(100));
        assert_eq!(rows[0].status, "confirmed");
        assert_eq!(rows[0].block_height, Some(31));
        assert_eq!(rows[0].block_finalizer.as_deref(), Some("miner"));
        assert_eq!(rows[0].direction, "received");
    }

    #[test]
    fn wallet_transaction_query_defaults_to_tx_only() {
        assert_eq!(
            WalletTransactionFilters::from_query(WalletTransactionsQuery::default()),
            WalletTransactionFilters::default()
        );
        assert_eq!(
            WalletTransactionFilters::from_query(WalletTransactionsQuery {
                tx: Some(false),
                mine: Some(true),
                burn: Some(true),
                offset: None,
                limit: None,
            }),
            WalletTransactionFilters {
                transfer: false,
                mine: true,
                burn: true,
            }
        );
    }

    #[test]
    fn page_items_returns_bounded_slices_with_next_offset() {
        let page = super::page_items(
            vec![1, 2, 3, 4, 5],
            super::PageQuery {
                offset: Some(1),
                limit: Some(2),
            },
        );

        assert_eq!(page.items, vec![2, 3]);
        assert_eq!(page.offset, 1);
        assert_eq!(page.limit, 2);
        assert_eq!(page.total, 5);
        assert!(page.has_more);
        assert_eq!(page.next_offset, Some(3));
    }

    #[test]
    fn page_items_clamps_limit_and_empty_tail() {
        let page = super::page_items(
            vec![1, 2],
            super::PageQuery {
                offset: Some(20),
                limit: Some(0),
            },
        );

        assert!(page.items.is_empty());
        assert_eq!(page.offset, 2);
        assert_eq!(page.limit, 1);
        assert_eq!(page.total, 2);
        assert!(!page.has_more);
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn mine_transaction_views_include_protocol_finalizer_fee() {
        let alice = Wallet::from_seed("wallet-mine-fee-alice");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mine = ledger.build_mine(alice.address()).unwrap();
        let chain = vec![fake_block(1, vec![mine.clone()])];
        let snapshot = fake_snapshot(BTreeMap::new(), chain.clone());
        let outputs = super::known_output_index(&snapshot, &[]);

        let rows = wallet_transaction_rows(
            alice.address(),
            Vec::new(),
            &chain,
            &outputs,
            WalletTransactionFilters {
                transfer: false,
                mine: true,
                burn: false,
            },
        );
        let transaction = super::ui_transaction(&mine, &outputs);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount, mine.amount());
        assert_eq!(rows[0].fee, MINE_FINALIZER_FEE);
        assert_eq!(transaction.amount, mine.amount());
        assert_eq!(transaction.fee, MINE_FINALIZER_FEE);
    }

    #[test]
    fn burn_wallet_transactions_require_burn_filter() {
        let alice = Wallet::from_seed("wallet-burn-filter-alice");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10);
        let ledger = Ledger::new(allocations.clone(), 1);
        let burn = ledger.build_burn(&alice, 3, 1).unwrap();
        let snapshot = fake_snapshot(allocations, Vec::new());
        let outputs = super::known_output_index(&snapshot, std::slice::from_ref(&burn));

        let default_rows = wallet_transaction_rows(
            alice.address(),
            vec![burn.clone()],
            &[],
            &outputs,
            WalletTransactionFilters::default(),
        );
        let burn_rows = wallet_transaction_rows(
            alice.address(),
            vec![burn.clone()],
            &[],
            &outputs,
            WalletTransactionFilters {
                transfer: false,
                mine: false,
                burn: true,
            },
        );

        assert!(default_rows.is_empty());
        assert_eq!(burn_rows.len(), 1);
        assert_eq!(burn_rows[0].kind, "burn");
        assert_eq!(burn_rows[0].direction, "burned");
        assert_eq!(burn_rows[0].amount, 3);
        assert_eq!(burn_rows[0].fee, 1);
    }

    #[test]
    fn wallet_utxo_rows_include_pending_spent_outputs_as_disabled() {
        let alice = Wallet::from_seed("wallet-utxo-pending-alice");
        let bob = Wallet::from_seed("wallet-utxo-pending-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10);
        let mut ledger = Ledger::new(allocations, 1);
        let pending = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
        let Transaction::Transfer { inputs, .. } = &pending else {
            panic!("expected transfer");
        };
        let spent_outpoint = inputs[0].outpoint.clone();

        ledger.submit_transaction(pending).unwrap();
        let rows = wallet_utxo_rows(&ledger, alice.address());

        assert!(rows.iter().any(|row| row.outpoint == spent_outpoint));
        assert!(
            rows.iter()
                .any(|row| row.outpoint == spent_outpoint && !row.spendable)
        );
    }

    #[test]
    fn selectable_wallet_utxo_rows_include_only_spendable_outputs() {
        let alice = Wallet::from_seed("wallet-utxo-selectable-alice");
        let bob = Wallet::from_seed("wallet-utxo-selectable-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10);
        let mut ledger = Ledger::new(allocations, 1);
        let pending = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
        let Transaction::Transfer { inputs, .. } = &pending else {
            panic!("expected transfer");
        };
        let spent_outpoint = inputs[0].outpoint.clone();

        ledger.submit_transaction(pending).unwrap();
        let rows = super::selectable_wallet_utxo_rows(&ledger, alice.address());

        assert!(!rows.iter().any(|row| row.outpoint == spent_outpoint));
        assert!(rows.iter().all(|row| row.spendable));
    }

    fn fake_block(height: u64, transactions: Vec<Transaction>) -> Block {
        Block {
            height,
            prev_hash: format!("prev-{height}"),
            timestamp_ms: height,
            miner: "miner".to_string(),
            finalizer_mode: crate::domain::FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 100,
            vdf_rounds: 0,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            blinded_transactions: Vec::new(),
            blinded_reveals: Vec::new(),
            transactions,
            hash: format!("hash-{height}"),
        }
    }

    fn fake_snapshot(
        genesis_allocations: BTreeMap<String, Amount>,
        blocks: Vec<Block>,
    ) -> ChainSnapshot {
        ChainSnapshot {
            genesis_allocations,
            vdf_rounds: 1,
            launch_profile: LaunchProfile::default(),
            blocks,
        }
    }

    fn metric_row(
        height: u64,
        block_time_ms: Option<u64>,
        vdf_rounds: u64,
    ) -> crate::adapters::chain_store::BlockMetricRow {
        crate::adapters::chain_store::BlockMetricRow {
            height,
            block_hash: format!("hash-{height}"),
            timestamp_ms: height,
            block_time_ms,
            mine_difficulty_bits: 12,
            circulating_supply: 100,
            transaction_count: 0,
            transfer_count: 0,
            burn_count: 0,
            mine_count: 0,
            burned_amount: 0,
            total_burned_amount: 0,
            fees_amount: 0,
            reward_amount: 0,
            vdf_rounds,
            finalizer_rank: 0,
        }
    }

    #[test]
    fn metrics_response_skips_bootstrap_points_for_block_time_and_vdf_rounds() {
        let response = super::metrics_response(
            true,
            vec![
                metric_row(0, None, 0),
                metric_row(1, Some(1_764_000_000_000), 0),
                metric_row(2, Some(600_000), 120),
                metric_row(3, Some(610_000), 130),
            ],
        );

        let block_time = response
            .charts
            .iter()
            .find(|chart| chart.id == "block-time")
            .expect("block time chart should exist");
        assert_eq!(
            block_time
                .points
                .iter()
                .map(|point| (point.height, point.value))
                .collect::<Vec<_>>(),
            vec![(2, 600.0), (3, 610.0)]
        );

        let vdf_rounds = response
            .charts
            .iter()
            .find(|chart| chart.id == "vdf-rounds")
            .expect("VDF rounds chart should exist");
        assert_eq!(
            vdf_rounds
                .points
                .iter()
                .map(|point| (point.height, point.value))
                .collect::<Vec<_>>(),
            vec![(2, 120.0), (3, 130.0)]
        );
    }

    #[test]
    fn metrics_screen_includes_block_range_filter() {
        assert!(super::INDEX_HTML.contains("iuna-ui.js?v=80"));
        assert!(super::INDEX_HTML.contains("aria-label=\"Metrics block range\""));
        assert!(super::INDEX_HTML.contains("setMetricsRange(100)"));
        assert!(super::INDEX_HTML.contains("setMetricsRange(1000)"));
        assert!(super::INDEX_HTML.contains("setMetricsRange('all')"));
    }

    #[test]
    fn initial_setup_includes_node_mode_choices() {
        assert!(super::INDEX_HTML.contains("aria-label=\"Initial node mode\""));
        assert!(super::INDEX_HTML.contains("selectSetupNodeMode('wallet')"));
        assert!(super::INDEX_HTML.contains("selectSetupNodeMode('non-listening')"));
        assert!(super::INDEX_HTML.contains("selectSetupNodeMode('listening')"));
        assert!(super::INDEX_HTML.contains("Change later in Settings"));
    }

    #[test]
    fn setup_completion_refreshes_chain_data() {
        let app_js = include_str!("../../www/assets/iuna-ui.js");
        assert!(app_js.contains("await this.refresh();\n        this.setupFeedback = null;"));
        assert!(!app_js.contains(
            "await this.refreshConfig();\n        await this.resetPagedDataset(\"peer\");"
        ));
    }

    #[test]
    fn setup_completion_applies_selected_node_mode() {
        let app_js = include_str!("../../www/assets/iuna-ui.js");
        assert!(app_js.contains("setupNodeMode: \"wallet\""));
        assert!(app_js.contains("async applySetupNodeMode()"));
        assert!(app_js.contains("await this.applySetupNodeMode();"));
        assert!(app_js.contains("\"/api/settings/p2p-inbound\""));
        assert!(app_js.contains("this.setUiMode(mode === \"wallet\" ? \"basic\" : \"advanced\")"));
    }

    #[test]
    fn polling_refreshes_paged_datasets_without_visible_loaders() {
        let app_js = include_str!("../../www/assets/iuna-ui.js");
        assert!(app_js.contains("setInterval(() => this.refresh({ silent: true }), 5000)"));
        assert!(app_js.contains("backgroundLoading"));
        assert!(app_js.contains("options.silent === true ? \"backgroundLoading\" : \"loading\""));
    }

    #[test]
    fn mine_screen_shows_fixed_pow_reward_without_burn_slider() {
        let app_js = include_str!("../../www/assets/iuna-ui.js");
        assert!(app_js.contains("powMineReward()"));
        assert!(
            super::INDEX_HTML.contains("Search for PoW actions that mint a fixed IUNA reward.")
        );
        assert!(super::INDEX_HTML.contains("amountLabel(powMineReward())"));
        assert!(!super::INDEX_HTML.contains("Needs burns"));
    }

    #[test]
    fn block_detail_finalizer_opens_burn_leader_ranks_modal() {
        let app_js = include_str!("../../www/assets/iuna-ui.js");
        assert!(super::INDEX_HTML.contains("openBurnLeaderRanksModal(selectedBlock)"));
        assert!(super::INDEX_HTML.contains("id=\"burn-ranks-title\""));
        assert!(super::INDEX_HTML.contains("burnLeaderRanks(selectedBurnLeaderBlock)"));
        assert!(app_js.contains("selectedBurnLeaderBlock"));
        assert!(app_js.contains("burnLeaderRankLabel(rank)"));
        assert!(app_js.contains("block?.burn_leader_ranks"));
        assert!(super::INDEX_HTML.contains("rank.ticket_id ?? rank.ticketId"));
    }

    async fn auth_test_state(config_path: std::path::PathBuf, config: UiConfig) -> HttpState {
        config_store::save(&config_path, &config).unwrap();
        let wallet_path = config_path.with_file_name("wallet.json");
        let (wallet, _) = wallet_store::replace_with_generated_seed_phrase(&wallet_path).unwrap();
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let node = Arc::new(Mutex::new(NodeCore::from_ledger(wallet, ledger, 0)));
        let peers = Arc::new(Mutex::new(PeerBook::default()));
        let gossip = GossipNetwork::new_for_tests(node.clone(), peers.clone());
        let chain_store = SqliteChainStore::open(config_path.with_file_name("chain.sqlite3"))
            .expect("test chain store should open");
        HttpState {
            node,
            peers,
            gossip,
            ui_config: Arc::new(Mutex::new(
                config_store::load_or_create(&config_path).unwrap(),
            )),
            config_path,
            chain_store,
            wallet_path,
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            auth_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            auth_backoff: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn auth_test_app(state: HttpState) -> Router {
        Router::new()
            .route("/api/auth/status", get(api_auth_status))
            .route("/api/auth/setup", post(api_auth_setup_form))
            .route("/api/auth/login", post(api_auth_login_form))
            .route(
                "/api/auth/change-password",
                post(api_auth_change_password_form),
            )
            .route("/favicon.ico", get(super::favicon))
            .route("/api/protected", get(protected_auth_test_endpoint))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                require_auth_middleware,
            ))
            .with_state(state)
    }

    async fn protected_auth_test_endpoint() -> &'static str {
        "protected"
    }

    struct TestHttpResponse {
        status: StatusCode,
        headers: HeaderMap,
        body: String,
    }

    async fn http_request(
        app: Router,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        body: &str,
    ) -> TestHttpResponse {
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::HOST, "127.0.0.1:18661")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if matches!(method, Method::POST | Method::DELETE) {
            builder = builder.header(header::ORIGIN, "http://127.0.0.1:18661");
        }
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let response = app
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        TestHttpResponse {
            status,
            headers,
            body,
        }
    }

    fn set_cookie_pair(headers: &HeaderMap) -> String {
        let header = headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("response should include Set-Cookie header");
        header.split(';').next().unwrap().to_string()
    }

    #[tokio::test]
    async fn burn_settings_config_persistence_updates_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let ui_config = Arc::new(Mutex::new(UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        }));
        let initial_config = ui_config.lock().await.clone();
        config_store::save(&config_path, &initial_config).expect("initial config should save");

        persist_burn_settings_config(
            &ui_config,
            &config_path,
            true,
            50 * MICRO_IUNA,
            3 * MICRO_IUNA,
        )
        .await
        .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();

        assert!(config.mining_enabled);
        assert_eq!(config.burn_per_block, 50 * MICRO_IUNA);
        assert_eq!(config.burn_fee, 3 * MICRO_IUNA);
    }

    #[tokio::test]
    async fn enabled_burn_settings_reject_zero_amount() {
        let dir = tempfile::tempdir().unwrap();
        let state = auth_test_state(
            dir.path().join("config.json"),
            UiConfig {
                setup_complete: true,
                ..UiConfig::default()
            },
        )
        .await;

        let error = super::set_burn_settings(&state, true, 0, MICRO_IUNA)
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("greater than zero"));
    }

    #[tokio::test]
    async fn metrics_setting_persists_config_and_clears_rows_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let state = auth_test_state(
            config_path.clone(),
            UiConfig {
                setup_complete: true,
                ..UiConfig::default()
            },
        )
        .await;
        let wallet = Wallet::from_seed("metrics-setting-wallet");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 10);
        let ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![crate::domain::GenesisBurn::new(wallet.address(), 1)],
            1,
        )
        .unwrap();
        *state.node.lock().await = NodeCore::from_ledger(wallet, ledger, 0);

        super::set_keep_track_of_metrics(&state, true)
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(config.keep_track_of_metrics);
        assert!(!state.chain_store.load_metrics().unwrap().is_empty());

        super::set_keep_track_of_metrics(&state, false)
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();
        assert!(!config.keep_track_of_metrics);
        assert!(state.chain_store.load_metrics().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pow_mining_config_persistence_updates_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let ui_config = Arc::new(Mutex::new(UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        }));
        let initial_config = ui_config.lock().await.clone();
        config_store::save(&config_path, &initial_config).expect("initial config should save");

        persist_pow_mining_config(&ui_config, &config_path, true)
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();

        assert!(config.pow_mining_enabled);
    }

    #[test]
    fn transfer_form_requires_recipient_amount_and_fee() {
        let error = validate_transfer_form(TransferForm {
            to: " ".to_string(),
            amount: 1,
            fee_per_byte: Some(1),
            utxos: String::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("recipient is required"));

        let error = validate_transfer_form(TransferForm {
            to: "abc".to_string(),
            amount: 0,
            fee_per_byte: Some(1),
            utxos: String::new(),
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("amount must be greater than zero")
        );

        let error = validate_transfer_form(TransferForm {
            to: "abc".to_string(),
            amount: 1,
            fee_per_byte: None,
            utxos: String::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("fee per byte is required"));
    }

    #[test]
    fn burn_and_mine_forms_require_fee_per_byte() {
        let burn = required_fee_per_byte_burn(&super::BurnSettingsForm {
            enabled: Some(true),
            amount: 1,
            fee_per_byte: None,
        })
        .unwrap_err();
        assert!(burn.to_string().contains("fee per byte is required"));
    }

    #[test]
    fn transfer_form_trims_recipient() {
        let (to, amount, fee, utxos) = validate_transfer_form(TransferForm {
            to: "  abc  ".to_string(),
            amount: 2,
            fee_per_byte: Some(3),
            utxos: String::new(),
        })
        .unwrap();

        assert_eq!(to, "abc");
        assert_eq!(amount, 2);
        assert_eq!(fee, 3);
        assert!(utxos.is_empty());
    }

    #[test]
    fn transfer_form_parses_selected_utxos() {
        let (_, _, _, utxos) = validate_transfer_form(TransferForm {
            to: "abc".to_string(),
            amount: 2,
            fee_per_byte: Some(3),
            utxos: "tx-one:0\ntx:with:colons:7,\n".to_string(),
        })
        .unwrap();

        assert_eq!(
            utxos,
            vec![
                OutPoint {
                    txid: "tx-one".to_string(),
                    index: 0
                },
                OutPoint {
                    txid: "tx:with:colons".to_string(),
                    index: 7
                }
            ]
        );
    }
}
