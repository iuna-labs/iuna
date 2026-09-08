use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::{
    Form, Json, Router,
    body::Body,
    extract::State,
    http::Request,
    middleware,
    middleware::Next,
    response::Response,
    routing::{get, post},
};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    adapters::{config_store, config_store::UiConfig, p2p::GossipNetwork},
    app::{SharedNode, SharedPeerBook},
};

mod actions;
mod api;
mod auth;
mod auth_routes;
mod consolidation;
mod index_html;
mod metrics;
mod request_auth;
mod state;
mod static_assets;
mod ui;
mod wallet;
use actions::{
    api_address_book_delete_form, api_address_book_form, api_burn_fee_estimate_form,
    api_burn_per_block_form, api_chain_reset_form, api_metrics_settings_form,
    api_mine_fee_estimate_form, api_p2p_announce_form, api_p2p_inbound_form, api_peer_delete_form,
    api_peer_form, api_pow_mining_form, api_recovery_vdf_settings_form, api_stratum_settings_form,
    api_transfer_fee_estimate_form, api_transfer_form, api_wallet_endpoint_settings_form,
    api_wallet_generate_form, api_wallet_import_form, apply_config_form, burn_per_block_form,
    peer_form, transfer_form,
};
use api::{
    api_blocks, api_config, api_mempool, api_metrics, api_network_health, api_p2p_metrics,
    api_peers, api_status, api_wallet_selectable_utxos, api_wallet_transactions, api_wallet_utxos,
};
use auth_routes::{
    api_auth_change_password_form, api_auth_login_form, api_auth_logout_form, api_auth_setup_form,
    api_auth_status, require_auth_middleware,
};
use index_html::INDEX_HTML;
use metrics::{metrics_response, network_health, top_mine_proofs};
use request_auth::wallet_password_for_request;
pub use state::ServeOptions;
use state::{AuthClientKey, AuthSession, HttpState};
use static_assets::{alpine_js, app_js, favicon, index};
use ui::{
    add_pending_outputs, populate_wallet_reward_flow, ui_blocks_from_indexes, ui_transaction,
    wallet_transaction_row, wallet_transaction_rows,
};
use wallet::{
    api_wallet_setup, estimate_burn_fee, estimate_mine_fee, estimate_transfer_fee,
    fee_estimate_json, import_setup_wallet_seed, replace_setup_wallet_with_generated_seed,
    required_fee_per_byte_burn, setup_requires_peer, transfer, wallet_setup_json,
};

pub fn validate_management_password(password: &str) -> Result<()> {
    auth::validate_password(password)
}

pub fn hash_management_password(password: &str) -> Result<String> {
    auth::hash_password(password)
}

pub fn verify_management_password(password: &str, encoded: &str) -> Result<bool> {
    auth::verify_password(password, encoded)
}

const EXPLORER_LIMIT: usize = 50;
const EXPLORER_PAGE_LIMIT: usize = 20;
const DATASET_LIMIT: usize = 1_000;
const DATASET_PAGE_LIMIT: usize = 25;
const AUTH_COOKIE_NAME: &str = "iuna_session";
const SETUP_COOKIE_NAME: &str = "iuna_setup";
const SETUP_COOKIE_TTL_SECS: u64 = 10 * 60;
const AUTH_SESSION_TTL_MS: u64 = 12 * 60 * 60 * 1_000;
const AUTH_MAX_FAILED_ATTEMPTS: u32 = 5;
const AUTH_LOCKOUT_MS: u64 = 60 * 1_000;
const UNKNOWN_CLIENT_KEY: &str = "unknown";
const PEER_STALE_AFTER_MS: u64 = 20 * 60 * 1_000;
const SLOW_UI_REQUEST_LOG_MS: u128 = 250;

mod types;
use types::{
    ActionResponse, AuthForm, AuthStatusResponse, BlocksQuery, ChangePasswordForm, ConfigForm,
    ConfigResponse, MempoolCounts, MetricsQuery, MetricsResponse, NetworkHealthLocalState,
    NetworkHealthResponse, Page, PageQuery, UiBlock, UiTransaction, WalletTransactionContext,
    WalletTransactionFilters, WalletTransactionRow, WalletTransactionsQuery, WalletUtxoRow,
};

pub async fn serve(
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    ui_config: Arc<Mutex<UiConfig>>,
    options: ServeOptions,
) -> Result<()> {
    let addr = options.addr;
    let setup_capability = auth::random_hex(32)?;
    let state = HttpState {
        node,
        peers,
        gossip,
        ui_config,
        config_path: options.config_path,
        chain_store: options.chain_store,
        ui_data_store: options.ui_data_store,
        wallet_path: options.wallet_path,
        stratum: options.stratum,
        auth_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        auth_backoff: Arc::new(Mutex::new(BTreeMap::new())),
        setup_capability: Arc::new(Mutex::new(Some(setup_capability))),
        management_port: addr.port(),
        wallet_endpoint_addr: options.wallet_endpoint_addr,
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
        .route(
            "/api/address-book",
            post(api_address_book_form).delete(api_address_book_delete_form),
        )
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
        .route(
            "/api/settings/recovery-vdf",
            post(api_recovery_vdf_settings_form),
        )
        .route("/api/settings/chain-reset", post(api_chain_reset_form))
        .route("/api/settings/p2p-inbound", post(api_p2p_inbound_form))
        .route("/api/settings/p2p-announce", post(api_p2p_announce_form))
        .route("/api/settings/stratum", post(api_stratum_settings_form))
        .route(
            "/api/settings/wallet-endpoint",
            post(api_wallet_endpoint_settings_form),
        )
        .route("/api/transfer", post(api_transfer_form))
        .route("/api/wallet/optimize/preview", post(consolidation::preview))
        .route("/api/wallet/optimize/submit", post(consolidation::submit))
        .route("/settings/burn-per-block", post(burn_per_block_form))
        .route("/transfer", post(transfer_form))
        .route("/peers", post(peer_form))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth_middleware,
        ))
        .layer(middleware::from_fn(log_slow_api_request))
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

async fn log_slow_api_request(request: Request<Body>, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    if path.starts_with("/api/") && elapsed.as_millis() >= SLOW_UI_REQUEST_LOG_MS {
        println!(
            "slow UI API request: {method} {path} -> {} in {:.2}s",
            response.status().as_u16(),
            elapsed.as_secs_f64()
        );
    }
    response
}

async fn api_config_form(
    State(state): State<HttpState>,
    Form(form): Form<ConfigForm>,
) -> Json<ActionResponse> {
    action_json(apply_config_form(&state, form).await)
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

fn api_error(error: anyhow::Error) -> Json<ActionResponse> {
    Json(ActionResponse {
        ok: false,
        error: Some(format!("{error:#}")),
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
