use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::header,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    adapters::{config_store, config_store::UiConfig, p2p::GossipNetwork, wallet_store},
    app::{NodeStatus, PeerInfo, SharedNode, SharedPeerBook},
    domain::{Amount, Block, Transaction},
};

const EXPLORER_LIMIT: usize = 50;
const EXPLORER_PAGE_LIMIT: usize = 20;

#[derive(Clone)]
struct HttpState {
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    ui_config: Arc<Mutex<UiConfig>>,
    config_path: PathBuf,
    wallet_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct AmountForm {
    amount: Amount,
}

#[derive(Debug, Deserialize)]
struct TransferForm {
    to: String,
    amount: Amount,
}

#[derive(Debug, Deserialize)]
struct PeerForm {
    peer: String,
}

#[derive(Debug, Deserialize)]
struct ConfigForm {
    setup_complete: bool,
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

#[derive(Debug, Serialize)]
struct ActionResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct WalletSetupResponse {
    ok: bool,
    error: Option<String>,
    address: Option<String>,
    seed_phrase: Option<String>,
    dev_verify_bypass: bool,
}

pub async fn serve(
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    ui_config: Arc<Mutex<UiConfig>>,
    config_path: PathBuf,
    wallet_path: PathBuf,
    addr: SocketAddr,
) -> Result<()> {
    let state = HttpState {
        node,
        peers,
        gossip,
        ui_config,
        config_path,
        wallet_path,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/alpine.min.js", get(alpine_js))
        .route("/assets/mivora-ui.js", get(app_js))
        .route("/api/status", get(api_status))
        .route("/api/blocks", get(api_blocks))
        .route("/api/config", get(api_config).post(api_config_form))
        .route("/api/wallet/setup", get(api_wallet_setup))
        .route("/api/wallet/generate", post(api_wallet_generate_form))
        .route("/api/wallet/import", post(api_wallet_import_form))
        .route("/api/mempool", get(api_mempool))
        .route("/api/peers", get(api_peers).post(api_peer_form))
        .route(
            "/api/settings/burn-per-block",
            post(api_burn_per_block_form),
        )
        .route("/api/transfer", post(api_transfer_form))
        .route("/settings/burn-per-block", post(burn_per_block_form))
        .route("/transfer", post(transfer_form))
        .route("/peers", post(peer_form))
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP management UI on {addr}"))?;
    axum::serve(listener, app.into_make_service())
        .await
        .context("serving HTTP management UI")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn alpine_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../assets/alpine.min.js"),
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../assets/mivora-ui.js"),
    )
}

async fn api_status(State(state): State<HttpState>) -> Json<NodeStatus> {
    Json(state.node.lock().await.status())
}

async fn api_blocks(
    State(state): State<HttpState>,
    Query(query): Query<BlocksQuery>,
) -> Json<Vec<Block>> {
    let limit = query
        .limit
        .unwrap_or(EXPLORER_PAGE_LIMIT)
        .min(EXPLORER_LIMIT);
    let node = state.node.lock().await;
    let blocks = match query.before_height {
        Some(before_height) => node.blocks_before(before_height, limit),
        None => node.recent_blocks(limit),
    };
    Json(blocks)
}

async fn api_config(State(state): State<HttpState>) -> Json<UiConfig> {
    Json(state.ui_config.lock().await.clone())
}

async fn api_wallet_setup(State(state): State<HttpState>) -> Json<WalletSetupResponse> {
    wallet_setup_json(wallet_setup_response(&state).await)
}

async fn api_mempool(State(state): State<HttpState>) -> Json<Vec<Transaction>> {
    Json(state.node.lock().await.pending_transactions())
}

async fn api_peers(State(state): State<HttpState>) -> Json<Vec<PeerInfo>> {
    Json(state.peers.lock().await.list())
}

async fn api_config_form(
    State(state): State<HttpState>,
    Form(form): Form<ConfigForm>,
) -> Json<ActionResponse> {
    let mut config = state.ui_config.lock().await;
    config.setup_complete = form.setup_complete;
    action_json(config_store::save(&state.config_path, &config))
}

async fn api_wallet_generate_form(State(state): State<HttpState>) -> Json<WalletSetupResponse> {
    wallet_setup_json(replace_setup_wallet_with_generated_seed(&state).await)
}

async fn api_wallet_import_form(
    State(state): State<HttpState>,
    Form(form): Form<SeedPhraseForm>,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(import_setup_wallet_seed(&state, &form.seed_phrase).await)
}

async fn api_burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<AmountForm>,
) -> Json<ActionResponse> {
    let result = set_burn_per_block(&state, form.amount).await;
    action_json(result)
}

async fn burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<AmountForm>,
) -> Response {
    match set_burn_per_block(&state, form.amount).await {
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

async fn peer_form(State(state): State<HttpState>, Form(form): Form<PeerForm>) -> Response {
    match add_peer(&state, form.peer).await {
        Ok(()) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

async fn set_burn_per_block(state: &HttpState, amount: Amount) -> Result<()> {
    let result = {
        let mut node = state.node.lock().await;
        let result = node.set_burn_per_block(amount);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => {
            persist_burn_per_block_config(&state.ui_config, &state.config_path, amount).await?;
            state.gossip.broadcast(result.1).await
        }
        Err(error) => Err(error),
    }
}

async fn persist_burn_per_block_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    amount: Amount,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.burn_per_block = amount;
    config_store::save(config_path, &config)
}

async fn add_peer(state: &HttpState, peer: String) -> Result<()> {
    let addresses = {
        let mut peers = state.peers.lock().await;
        peers.add_peer(peer);
        peers.addresses()
    };
    let mut config = state.ui_config.lock().await;
    config.peers = addresses;
    config_store::save(&state.config_path, &config)
}

async fn wallet_setup_response(state: &HttpState) -> Result<WalletSetupResponse> {
    let setup_complete = state.ui_config.lock().await.setup_complete;
    let seed_phrase = if setup_complete {
        None
    } else {
        wallet_store::setup_seed_phrase(&state.wallet_path)?
    };
    let address = state.node.lock().await.wallet_address().to_string();
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
    })
}

async fn replace_setup_wallet_with_generated_seed(
    state: &HttpState,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let (wallet, seed_phrase) =
        wallet_store::replace_with_generated_seed_phrase(&state.wallet_path)?;
    let address = wallet.address().to_string();
    state.node.lock().await.replace_wallet(wallet);
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: Some(seed_phrase),
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
    })
}

async fn import_setup_wallet_seed(
    state: &HttpState,
    seed_phrase: &str,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let wallet = wallet_store::replace_with_imported_seed_phrase(&state.wallet_path, seed_phrase)?;
    let address = wallet.address().to_string();
    state.node.lock().await.replace_wallet(wallet);
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: None,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
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
        }),
    }
}

fn dev_seed_verify_bypass_enabled() -> bool {
    dev_seed_verify_bypass_allowed(std::env::var_os("MIVORA_DEV_SKIP_SEED_VERIFY").is_some())
}

fn dev_seed_verify_bypass_allowed(env_present: bool) -> bool {
    env_present
}

async fn transfer(state: &HttpState, form: TransferForm) -> Result<()> {
    let result = {
        let mut node = state.node.lock().await;
        let result = node.transfer(form.to, form.amount);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => state.gossip.broadcast(result.1).await,
        Err(error) => Err(error),
    }
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

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Mivora</title>
  <style>
    [x-cloak] { display: none !important; }
    :root {
      color-scheme: dark;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #0f1012;
      color: #e8edf0;
    }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; background: #0f1012; color: #e8edf0; }
    .app-shell { min-height: 100vh; display: grid; grid-template-columns: 84px minmax(0, 1fr); }
    .sidebar { position: sticky; top: 0; height: 100vh; display: flex; flex-direction: column; align-items: center; gap: 20px; padding: 16px 10px; background: #15171a; border-right: 1px solid #262b2f; }
    .brand-mark { width: 44px; height: 44px; display: grid; place-items: center; border-radius: 8px; background: #d5f55f; color: #11140c; font-size: 22px; font-weight: 900; }
    .side-nav { display: grid; gap: 10px; width: 100%; }
    .nav-button { width: 64px; min-height: 58px; display: grid; place-items: center; gap: 4px; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; background: transparent; color: #9fa8ad; }
    .nav-button svg { width: 21px; height: 21px; stroke: currentColor; stroke-width: 2; fill: none; }
    .nav-button svg.chain-icon { stroke-width: 1.35; }
    .nav-button span { font-size: 11px; font-weight: 800; }
    .nav-button:hover, .nav-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .content { width: 100%; min-width: 0; padding: 22px 24px 48px; }
    main { width: 100%; }
    main > section { width: 100%; }
    header { display: flex; justify-content: space-between; gap: 18px; align-items: flex-start; padding: 0 0 18px; }
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
    .flash { border-radius: 6px; padding: 10px 12px; margin: 12px 0; border: 1px solid; font-weight: 700; }
    .flash.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .flash.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .ok { color: #d5f55f; }
    .page-title { margin-bottom: 16px; }
    .setup-overlay { position: fixed; inset: 0; z-index: 30; display: grid; place-items: center; padding: 22px; background: rgba(8, 9, 10, .72); backdrop-filter: blur(8px); }
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
    .setup-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 14px; }
    .setup-peer-list { margin-top: 14px; display: grid; gap: 8px; }
    .setup-peer-row { display: flex; justify-content: space-between; gap: 12px; align-items: center; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #181b1f; }
    .segmented { display: inline-flex; gap: 4px; padding: 4px; border: 1px solid #2f363c; border-radius: 8px; background: #181b1f; }
    .segmented button { border-color: transparent; background: transparent; color: #9fa8ad; }
    .segmented button.active { background: #d5f55f; color: #15171a; }
    .seed-panel { display: grid; gap: 12px; }
    .seed-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(132px, 1fr)); gap: 8px; }
    .seed-word { display: grid; grid-template-columns: 30px minmax(0, 1fr); gap: 7px; align-items: center; border: 1px solid #2f363c; border-radius: 6px; padding: 7px 8px; background: #181b1f; }
    .seed-word .index { color: #8d989f; font-size: 11px; font-weight: 800; }
    .seed-word .word { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-weight: 800; color: #c7f5ea; }
    .verify-grid { display: grid; gap: 8px; }
    .setup-status { border: 1px solid #566d25; border-radius: 8px; padding: 10px; background: #1c2516; color: #d5f55f; font-weight: 800; }
    .wallet-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .wallet-actions { display: grid; gap: 12px; }
    .mining-grid { width: 100%; display: grid; grid-template-columns: minmax(0, .95fr) minmax(300px, .7fr); gap: 12px; align-items: start; }
    .config-grid { width: 100%; display: grid; grid-template-columns: minmax(0, .9fr) minmax(300px, .75fr); gap: 12px; align-items: start; }
    .receive-address { display: grid; gap: 8px; }
    .address-box { border: 1px solid #2f363c; border-radius: 8px; padding: 11px; background: #111316; }
    .panel-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; margin-bottom: 12px; }
    .panel-head h2, .panel-head h3 { margin-bottom: 0; }
    .switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 8px; color: #d6dee2; font-weight: 700; }
    .switch input { width: auto; min-width: 0; accent-color: #d5f55f; }
    .wallet-tx-list { display: grid; gap: 8px; }
    .wallet-tx-row { display: grid; grid-template-columns: minmax(88px, .35fr) minmax(0, 1fr) auto; gap: 12px; align-items: center; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .wallet-tx-row.pending { border-color: #3a4147; background: #191c20; box-shadow: inset 3px 0 0 #6f7880; }
    .wallet-tx-main { display: grid; gap: 4px; min-width: 0; }
    .wallet-tx-amount { font-weight: 900; }
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
    .detail-grid { display: grid; grid-template-columns: minmax(0, .9fr) minmax(0, 1.1fr); gap: 12px; }
    .detail-kv { display: grid; grid-template-columns: 90px minmax(0, 1fr); gap: 8px; font-size: 13px; margin: 7px 0; }
    .detail-kv .key { color: #8d989f; }
    .tx-list { display: grid; gap: 8px; }
    .tx-card { border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .tx-head { display: flex; justify-content: space-between; gap: 8px; margin-bottom: 6px; font-weight: 800; }
    .pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 2px 8px; font-size: 12px; font-weight: 800; background: #2b3136; color: #d6dee2; }
    .pill.burn { background: #332918; color: #ffd070; }
    .pill.transfer { background: #17312a; color: #8de9cd; }
    .mempool-strip { display: flex; gap: 8px; overflow-x: auto; padding-bottom: 4px; }
    .mempool-item { flex: 0 0 200px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    @media (max-width: 920px) { .setup-grid, .wallet-grid, .mining-grid, .config-grid, .detail-grid { grid-template-columns: 1fr; } }
    @media (max-width: 760px) {
      .app-shell { grid-template-columns: 1fr; }
      .sidebar { position: sticky; z-index: 5; bottom: 0; top: auto; height: auto; flex-direction: row; justify-content: space-between; padding: 8px; border-right: 0; border-bottom: 1px solid #262b2f; }
      .brand-mark { width: 38px; height: 38px; font-size: 19px; }
      .side-nav { display: flex; width: auto; gap: 8px; }
      .nav-button { width: 52px; min-height: 48px; }
      .nav-button span { font-size: 10px; }
      .content { padding: 16px 12px 36px; }
      header, .split, .setup-grid, .wallet-grid, .mining-grid, .config-grid, .detail-grid, .wallet-tx-row { grid-template-columns: 1fr; }
      header { display: grid; }
      input { min-width: 0; width: 100%; }
      .switch input { width: auto; }
      .block-card { flex-basis: 108px; }
    }
  </style>
  <script defer src="/assets/mivora-ui.js?v=24"></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="mivoraApp()" x-init="init()" x-cloak>
  <div class="app-shell">
    <aside class="sidebar" aria-label="Mivora navigation">
      <div class="brand-mark" title="Mivora">M</div>
      <nav class="side-nav">
        <button class="nav-button" :class="{ active: tab === 'wallet' }" @click="setTab('wallet')" type="button" title="Wallet" aria-label="Wallet">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3z"></path><path d="M3 7V5a2 2 0 0 1 2-2h12"></path><path d="M16 13h3"></path></svg>
          <span>Wallet</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'mining' }" @click="setTab('mining')" type="button" title="Mining" aria-label="Mining">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 19V5"></path><path d="M4 19h16"></path><path d="M7 15l4-4 3 3 5-7"></path></svg>
          <span>Mining</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'p2p' }" @click="setTab('p2p')" type="button" title="P2P" aria-label="P2P">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="12" r="3"></circle><circle cx="18" cy="6" r="3"></circle><circle cx="18" cy="18" r="3"></circle><path d="M8.5 10.5 15.5 7.5"></path><path d="M8.5 13.5 15.5 16.5"></path></svg>
          <span>P2P</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'chain' }" @click="setTab('chain')" type="button" title="Explorer" aria-label="Explorer">
          <svg class="chain-icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="1.5" y="9" width="5.5" height="5.5"></rect><rect x="9.25" y="9" width="5.5" height="5.5"></rect><rect x="17" y="9" width="5.5" height="5.5"></rect></svg>
          <span>Chain</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'config' }" @click="setTab('config')" type="button" title="Configuration" aria-label="Configuration">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 21v-7"></path><path d="M4 10V3"></path><path d="M12 21v-9"></path><path d="M12 8V3"></path><path d="M20 21v-5"></path><path d="M20 12V3"></path><path d="M2 14h4"></path><path d="M10 8h4"></path><path d="M18 16h4"></path></svg>
          <span>Config</span>
        </button>
      </nav>
    </aside>

    <main class="content">
    <header>
      <div>
        <h1 x-text="pageTitle()">Mivora</h1>
      </div>
      <div class="muted" x-text="lastUpdatedLabel()"></div>
    </header>

    <div class="flash" :class="flash?.kind" x-show="flash" x-transition x-text="flash?.message"></div>

    <section x-show="tab === 'wallet'">
      <div class="page-title">
        <div class="muted">Balance <strong x-text="status.wallet_balance ?? '-'"></strong></div>
      </div>
      <div class="wallet-grid">
        <div class="wallet-actions">
          <div class="panel">
            <h3>Send</h3>
            <form @submit.prevent="sendTransfer">
              <label>Recipient<input x-model="transferTo" autocomplete="off"></label>
              <label>Amount<input x-model.number="transferAmount" type="number" min="1"></label>
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
        <div class="panel">
          <div class="panel-head">
            <h3>Transactions</h3>
            <label class="switch"><input x-model="showBurnTransactions" type="checkbox">Show burns</label>
          </div>
          <div class="wallet-tx-list">
            <template x-for="tx in walletTransactions()" :key="tx.status + '-' + tx.signature">
              <div class="wallet-tx-row" :class="{ pending: tx.status === 'pending' }">
                <span class="pill" :class="tx.kind" x-text="tx.direction"></span>
                <div class="wallet-tx-main">
                  <div><span class="wallet-tx-amount" x-text="tx.amount"></span> coin(s)</div>
                  <div class="muted" x-text="txTitle(tx)"></div>
                  <div><span class="muted">from </span><code x-text="short(tx.from)"></code></div>
                  <div x-show="tx.to"><span class="muted">to </span><code x-text="short(tx.to)"></code></div>
                </div>
                <code x-text="short(tx.signature)"></code>
              </div>
            </template>
            <div class="muted" x-show="walletTransactions().length === 0">No wallet transactions</div>
          </div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'mining'">
      <div class="page-title">
        <div class="muted">Automatic VDF-paced block production</div>
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
      </div>
    </section>

    <section x-show="tab === 'p2p'">
      <div class="panel">
        <h2>Add Peer</h2>
        <form @submit.prevent="addPeer">
          <label>Peer address<input x-model="peerAddress" placeholder="127.0.0.1:9445"></label>
          <button class="primary" type="submit">Add</button>
        </form>
      </div>
      <div class="panel table-wrap">
        <table>
          <thead><tr><th>Address</th><th>Direction</th><th>Height</th><th>Tip</th><th>Sent</th><th>Received</th><th>Last Error</th></tr></thead>
          <tbody>
            <template x-for="peer in peers" :key="peer.address">
              <tr><td><code x-text="peer.address"></code></td><td x-text="peer.direction"></td><td x-text="peer.last_known_height ?? '-'"></td><td><code x-text="short(peer.last_known_tip_hash)"></code></td><td x-text="peer.messages_sent"></td><td x-text="peer.messages_received"></td><td x-text="peer.last_error || ''"></td></tr>
            </template>
            <tr x-show="peers.length === 0"><td colspan="7">No peers</td></tr>
          </tbody>
        </table>
      </div>
    </section>

    <section x-show="tab === 'config'">
      <div class="page-title">
        <div class="muted">Runtime node settings</div>
      </div>
      <div class="config-grid">
        <div class="panel">
          <h3>Mining</h3>
          <form @submit.prevent="saveBurn">
            <label>Coins per block<input x-model.number="burnAmountDraft" @input="burnAmountDirty = true" type="number" min="0"></label>
            <button class="primary" type="submit">Save</button>
          </form>
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
                </div>
                <div class="block-miner" x-text="blockMinerLabel(block)"></div>
              </button>
            </template>
            <template x-if="loadingOlder">
              <div class="block-card skeleton-card" aria-hidden="true">
                <div class="skeleton-line short"></div>
                <div class="skeleton-line medium"></div>
                <div class="skeleton-line long"></div>
              </div>
            </template>
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
                <div class="detail-kv"><div class="key">Miner</div><code x-text="short(selectedBlock.miner)"></code></div>
                <div class="detail-kv"><div class="key">Reward</div><div x-text="selectedBlock.reward"></div></div>
                <div class="detail-kv"><div class="key">Burns</div><div x-text="blockBurnCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Transfers</div><div x-text="blockTransferCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Total Burned</div><div x-text="blockBurned(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">VDF</div><div><span x-text="selectedBlock.vdf_rounds"></span> rounds</div></div>
              </div>
              <div class="tx-list">
                <h3>Transactions</h3>
                <template x-for="tx in selectedBlock.transactions" :key="tx.signature">
                  <div class="tx-card">
                    <div class="tx-head"><span class="pill" :class="tx.kind" x-text="tx.kind"></span><strong x-text="tx.amount"></strong></div>
                    <div><span class="muted">from </span><code x-text="short(tx.from)"></code></div>
                    <div x-show="tx.to"><span class="muted">to </span><code x-text="short(tx.to)"></code></div>
                    <div class="muted">nonce <span x-text="tx.nonce"></span></div>
                    <div><code x-text="short(tx.signature)"></code></div>
                  </div>
                </template>
                <div class="muted" x-show="selectedBlock.transactions.length === 0">No transactions</div>
              </div>
            </div>
          </template>
          <div class="muted" x-show="!selectedBlock">Select a block</div>
        </section>

        <section class="panel" x-show="mempool.length > 0">
          <h2>Mempool</h2>
          <div class="mempool-strip">
            <template x-for="tx in mempool" :key="tx.signature">
              <div class="mempool-item">
                <div class="tx-head"><span class="pill" :class="tx.kind" x-text="tx.kind"></span><strong x-text="tx.amount"></strong></div>
                <div><span class="muted">from </span><code x-text="short(tx.from)"></code></div>
                <div x-show="tx.to"><span class="muted">to </span><code x-text="short(tx.to)"></code></div>
                <div class="muted">nonce <span x-text="tx.nonce"></span></div>
              </div>
            </template>
          </div>
        </section>
      </div>
    </section>
    </main>
  </div>
  <div class="setup-overlay" x-show="showingSetup()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="setup-title">
    <section class="setup-modal">
      <div class="setup-modal-head">
        <div class="setup-welcome">Welcome to Mivora</div>
        <h2 id="setup-title">Initial Setup</h2>
        <div class="setup-copy">Confirm the local wallet address and add any peers before this node starts from a saved configuration.</div>
      </div>
      <div class="setup-feedback" :class="setupFeedback?.kind" x-show="setupFeedback" x-transition x-text="setupFeedback?.message"></div>
      <div class="setup-grid">
        <div class="setup-section seed-panel">
          <div class="panel-head">
            <h3>Wallet</h3>
            <button type="button" @click="copyAddress">Copy</button>
          </div>
          <div class="address-box"><code x-text="setupAddress()"></code></div>
          <div class="segmented" role="tablist" aria-label="Wallet setup mode">
            <button type="button" :class="{ active: setupWalletMode === 'create' }" @click="selectSetupWalletMode('create')">Create</button>
            <button type="button" :class="{ active: setupWalletMode === 'import' }" @click="selectSetupWalletMode('import')">Import</button>
          </div>
          <div x-show="setupWalletMode === 'create'" class="seed-panel">
            <template x-if="setupSeedWords().length > 0 && setupSeedStep === 'write'">
              <div class="seed-panel">
                <div class="seed-grid">
                  <template x-for="(word, index) in setupSeedWords()" :key="index">
                    <div class="seed-word">
                      <span class="index" x-text="index + 1"></span>
                      <span class="word" x-text="word"></span>
                    </div>
                  </template>
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
              <label>Recovery phrase<textarea x-model="importSeedPhrase" autocomplete="off" spellcheck="false"></textarea></label>
              <button class="primary" type="submit">Import</button>
            </form>
            <template x-if="walletVerified">
              <div class="setup-status">Recovery phrase imported</div>
            </template>
          </div>
        </div>
        <div class="setup-section">
          <h3>Peers</h3>
          <form @submit.prevent="addPeer">
            <label>Peer address<input x-model="peerAddress" placeholder="127.0.0.1:9445"></label>
            <button class="primary" type="submit">Add</button>
          </form>
          <div class="setup-peer-list">
            <template x-for="peer in peers" :key="peer.address">
              <div class="setup-peer-row">
                <code x-text="peer.address"></code>
                <span class="muted" x-text="peer.direction"></span>
              </div>
            </template>
            <div class="muted" x-show="peers.length === 0">No peers</div>
          </div>
        </div>
      </div>
      <div class="setup-actions">
        <button class="primary" type="button" :disabled="!walletVerified" @click="completeSetup">Continue</button>
      </div>
    </section>
  </div>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use crate::adapters::{config_store, config_store::UiConfig};

    use super::{dev_seed_verify_bypass_allowed, persist_burn_per_block_config};

    #[test]
    fn dev_seed_verify_bypass_requires_env_flag() {
        assert!(dev_seed_verify_bypass_allowed(true));
        assert!(!dev_seed_verify_bypass_allowed(false));
    }

    #[tokio::test]
    async fn burn_rate_config_persistence_updates_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let ui_config = Arc::new(Mutex::new(UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        }));
        let initial_config = ui_config.lock().await.clone();
        config_store::save(&config_path, &initial_config).expect("initial config should save");

        persist_burn_per_block_config(&ui_config, &config_path, 50)
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();

        assert_eq!(config.burn_per_block, 50);
    }
}
