use std::{
    collections::BTreeMap,
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
    adapters::{
        config_store,
        config_store::UiConfig,
        p2p::{GossipNetwork, P2pMetrics},
        wallet_store,
    },
    app::{NodeStatus, PeerInfo, SharedNode, SharedPeerBook},
    domain::{Amount, Block, OutPoint, Transaction, TxInput, TxOutput, hex_hash},
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
struct BurnSettingsForm {
    amount: Amount,
    fee: Amount,
}

#[derive(Debug, Deserialize)]
struct TransferForm {
    to: String,
    amount: Amount,
    fee: Option<Amount>,
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
    block_miner: Option<String>,
    direction: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WalletUtxoRow {
    outpoint: OutPoint,
    address: String,
    amount: Amount,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UiBlock {
    height: u64,
    prev_hash: String,
    timestamp_ms: u64,
    miner: String,
    reward: Amount,
    vdf_rounds: u32,
    vdf_output: String,
    leader_proof: Option<crate::domain::LeaderProof>,
    transactions: Vec<UiTransaction>,
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
        .route("/assets/luun-ui.js", get(app_js))
        .route("/api/status", get(api_status))
        .route("/api/blocks", get(api_blocks))
        .route("/api/config", get(api_config).post(api_config_form))
        .route("/api/wallet/setup", get(api_wallet_setup))
        .route("/api/wallet/generate", post(api_wallet_generate_form))
        .route("/api/wallet/import", post(api_wallet_import_form))
        .route("/api/wallet/transactions", get(api_wallet_transactions))
        .route("/api/wallet/utxos", get(api_wallet_utxos))
        .route("/api/mempool", get(api_mempool))
        .route("/api/peers", get(api_peers).post(api_peer_form))
        .route("/api/p2p/metrics", get(api_p2p_metrics))
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
        include_str!("../../assets/luun-ui.js"),
    )
}

async fn api_status(State(state): State<HttpState>) -> Json<NodeStatus> {
    Json(state.node.lock().await.status())
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
    Json(ui_blocks(
        blocks,
        &snapshot.genesis_allocations,
        &snapshot.blocks,
        &pending,
    ))
}

async fn api_config(State(state): State<HttpState>) -> Json<UiConfig> {
    Json(state.ui_config.lock().await.clone())
}

async fn api_wallet_setup(State(state): State<HttpState>) -> Json<WalletSetupResponse> {
    wallet_setup_json(wallet_setup_response(&state).await)
}

async fn api_mempool(State(state): State<HttpState>) -> Json<Vec<UiTransaction>> {
    let node = state.node.lock().await;
    let snapshot = node.chain_snapshot();
    let pending = node.pending_transactions();
    let outputs = known_output_index(&snapshot.genesis_allocations, &snapshot.blocks, &pending);
    Json(
        pending
            .iter()
            .map(|tx| ui_transaction(tx, &outputs))
            .collect(),
    )
}

async fn api_wallet_transactions(
    State(state): State<HttpState>,
) -> Json<Vec<WalletTransactionRow>> {
    let node = state.node.lock().await;
    let snapshot = node.chain_snapshot();
    let pending = node.pending_transactions();
    let outputs = known_output_index(&snapshot.genesis_allocations, &snapshot.blocks, &pending);
    Json(wallet_transaction_rows(
        node.wallet_address(),
        pending,
        &snapshot.blocks,
        &outputs,
    ))
}

async fn api_wallet_utxos(State(state): State<HttpState>) -> Json<Vec<WalletUtxoRow>> {
    let node = state.node.lock().await;
    let wallet = node.wallet_address().to_string();
    let mut utxos = node
        .ledger()
        .available_utxos_for_address(&wallet)
        .unwrap_or_default()
        .into_iter()
        .map(|(outpoint, output)| WalletUtxoRow {
            outpoint,
            address: output.address,
            amount: output.amount,
        })
        .collect::<Vec<_>>();
    utxos.sort_by(|left, right| {
        right
            .amount
            .cmp(&left.amount)
            .then_with(|| left.outpoint.txid.cmp(&right.outpoint.txid))
            .then_with(|| left.outpoint.index.cmp(&right.outpoint.index))
    });
    Json(utxos)
}

async fn api_peers(State(state): State<HttpState>) -> Json<Vec<PeerInfo>> {
    Json(state.peers.lock().await.list())
}

async fn api_p2p_metrics(State(state): State<HttpState>) -> Json<P2pMetrics> {
    Json(state.gossip.metrics())
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
    Form(form): Form<BurnSettingsForm>,
) -> Json<ActionResponse> {
    let result = set_burn_settings(&state, form.amount, form.fee).await;
    action_json(result)
}

async fn burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Response {
    match set_burn_settings(&state, form.amount, form.fee).await {
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

async fn set_burn_settings(state: &HttpState, amount: Amount, fee: Amount) -> Result<()> {
    let result = {
        let mut node = state.node.lock().await;
        let result = node.set_automatic_burn(amount, fee);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => {
            persist_burn_settings_config(&state.ui_config, &state.config_path, amount, fee).await?;
            state.gossip.broadcast(result.1).await
        }
        Err(error) => Err(error),
    }
}

async fn persist_burn_settings_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    amount: Amount,
    fee: Amount,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.burn_per_block = amount;
    config.burn_fee = fee;
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

fn wallet_transaction_rows(
    wallet: &str,
    pending: Vec<Transaction>,
    chain: &[Block],
    outputs: &BTreeMap<OutPoint, TxOutput>,
) -> Vec<WalletTransactionRow> {
    let mut rows = Vec::new();

    for (index, tx) in pending.iter().enumerate() {
        if let Some(row) = wallet_transaction_row(wallet, tx, outputs, "pending", None, None) {
            rows.push((u128::MAX - index as u128, row));
        }
    }

    for block in chain {
        for (index, tx) in block.transactions.iter().rev().enumerate() {
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
    block_miner: Option<String>,
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
            block_miner,
            direction: if tx.to() == Some(wallet) {
                "received"
            } else {
                "sent"
            },
        }),
        _ => None,
    }
}

fn ui_blocks(
    blocks: Vec<Block>,
    genesis_allocations: &BTreeMap<String, Amount>,
    chain: &[Block],
    pending: &[Transaction],
) -> Vec<UiBlock> {
    let outputs = known_output_index(genesis_allocations, chain, pending);
    blocks
        .into_iter()
        .map(|block| ui_block(block, &outputs))
        .collect()
}

fn ui_block(block: Block, outputs: &BTreeMap<OutPoint, TxOutput>) -> UiBlock {
    UiBlock {
        height: block.height,
        prev_hash: block.prev_hash,
        timestamp_ms: block.timestamp_ms,
        miner: block.miner,
        reward: block.reward,
        vdf_rounds: block.vdf_rounds,
        vdf_output: block.vdf_output,
        leader_proof: block.leader_proof,
        transactions: block
            .transactions
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
        },
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

fn known_output_index(
    genesis_allocations: &BTreeMap<String, Amount>,
    chain: &[Block],
    pending: &[Transaction],
) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = BTreeMap::new();
    for (address, amount) in genesis_allocations {
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
    for block in chain {
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
        Transaction::Transfer { outputs, .. } => outputs,
        Transaction::Burn { change, .. } => change,
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
        txid: hex_hash(format!("luun-genesis-allocation:{address}")),
        index: 0,
    }
}

fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
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
    dev_seed_verify_bypass_allowed(std::env::var_os("LUUN_DEV_SKIP_SEED_VERIFY").is_some())
}

fn dev_seed_verify_bypass_allowed(env_present: bool) -> bool {
    env_present
}

async fn transfer(state: &HttpState, form: TransferForm) -> Result<()> {
    let (to, amount, fee, selected_utxos) = validate_transfer_form(form)?;

    let result = {
        let mut node = state.node.lock().await;
        let result = if selected_utxos.is_empty() {
            node.transfer_with_fee(to, amount, fee)
        } else {
            node.transfer_with_fee_spending(to, amount, fee, &selected_utxos)
        };
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
    let fee = form.fee.context("fee is required")?;
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
  <title>Luun</title>
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
    .brand-mark { width: 44px; height: 44px; display: grid; place-items: center; border-radius: 8px; background: #d5f55f; color: #11140c; font-size: 22px; font-weight: 900; user-select: none; cursor: default; }
    .brand-mark:hover { animation: brand-wink .7s ease both; }
    @keyframes brand-wink { 0%, 100% { transform: translateY(0) rotate(0deg); box-shadow: none; } 35% { transform: translateY(-2px) rotate(-3deg); box-shadow: 0 8px 20px rgba(213, 245, 95, .18); } 70% { transform: translateY(0) rotate(2deg); } }
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
    .setup-field { display: grid; gap: 6px; }
    .setup-field-label { color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; letter-spacing: 0; }
    .setup-address-box { display: flex; justify-content: space-between; gap: 10px; align-items: center; }
    .setup-address-box code { min-width: 0; }
    .setup-address-box button { flex: 0 0 auto; }
    .setup-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 14px; }
    .setup-peer-list { margin-top: 14px; display: grid; gap: 8px; }
    .setup-peer-row { display: flex; justify-content: space-between; gap: 12px; align-items: center; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #181b1f; }
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
    .wallet-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .wallet-actions { display: grid; gap: 12px; }
    .advanced-toggle { flex-basis: 100%; width: max-content; align-self: flex-start; border-color: #3a4248; padding: 4px 7px; background: #202328; color: #9fa8ad; font-size: 12px; }
    .advanced-toggle:hover { border-color: #5a646b; color: #d6dee2; }
    .send-utxo-list { display: grid; gap: 8px; max-height: 260px; overflow: auto; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #111316; }
    .send-utxo-option { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #181b1f; }
    .send-utxo-option input { min-width: auto; margin-top: 3px; }
    .send-utxo-summary { flex-basis: 100%; width: 100%; display: grid; gap: 5px; color: #9eb3bc; font-size: 13px; }
    .wallet-balance-line { display: inline-grid; grid-template-columns: auto auto; gap: 10px; align-items: baseline; padding: 8px 10px; border: 1px solid #2f363c; border-radius: 8px; background: #111316; color: inherit; cursor: pointer; }
    .wallet-balance-line:hover, .wallet-balance-line:focus-visible { border-color: #d5f55f; outline: none; }
    .wallet-balance-line .tx-value { font-size: 16px; font-weight: 850; }
    .mining-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; align-items: start; }
    .mining-form { width: 100%; display: grid; grid-template-columns: minmax(220px, .45fr) minmax(280px, 1fr); gap: 14px; align-items: end; }
    .burn-fields { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .burn-slider-panel { display: grid; gap: 8px; min-width: 0; }
    .burn-slider-head, .burn-slider-scale { display: flex; justify-content: space-between; gap: 10px; color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; }
    .burn-slider-track { position: relative; min-height: 28px; display: flex; align-items: center; }
    .burn-range { width: 100%; min-width: 0; accent-color: #d5f55f; }
    .break-even-marker { position: absolute; top: 2px; bottom: 2px; width: 2px; transform: translateX(-1px); background: #ffd070; box-shadow: 0 0 0 1px #111316, 0 0 0 4px rgba(255, 208, 112, .18); pointer-events: none; }
    .burn-slider-note { color: #a8b2b8; font-size: 12px; line-height: 1.4; }
    .receive-address { display: grid; gap: 8px; }
    .address-box { border: 1px solid #2f363c; border-radius: 8px; padding: 11px; background: #111316; }
    .panel-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; margin-bottom: 12px; }
    .panel-head h2, .panel-head h3 { margin-bottom: 0; }
    .switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 8px; color: #d6dee2; font-weight: 700; }
    .switch input { width: auto; min-width: 0; accent-color: #d5f55f; }
    .wallet-tx-list { display: grid; gap: 8px; }
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
    .tx-card, .mempool-item { position: relative; display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 12px; background: #111316; cursor: pointer; text-align: left; }
    .tx-card .pill, .mempool-item .pill { position: absolute; top: 10px; right: 10px; }
    .pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 2px 8px; font-size: 12px; font-weight: 800; background: #2b3136; color: #d6dee2; }
    .pill.burn { background: #332918; color: #ffd070; }
    .pill.transfer { background: #17312a; color: #8de9cd; }
    .mempool-strip { display: flex; gap: 8px; overflow-x: auto; padding-bottom: 4px; }
    .mempool-item { flex: 0 0 220px; }
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
    @media (max-width: 760px) { .utxo-flow { grid-template-columns: 1fr; } .utxo-arrow { min-height: 28px; transform: rotate(90deg); } .tx-modal-head { align-items: stretch; } }
    @media (max-width: 920px) { .setup-grid, .wallet-grid, .mining-grid, .detail-grid { grid-template-columns: 1fr; } }
    @media (max-width: 920px) { .mining-form { grid-template-columns: 1fr; } }
    @media (max-width: 760px) {
      .app-shell { grid-template-columns: 1fr; }
      .sidebar { position: sticky; z-index: 5; bottom: 0; top: auto; height: auto; flex-direction: row; justify-content: space-between; padding: 8px; border-right: 0; border-bottom: 1px solid #262b2f; }
      .brand-mark { width: 38px; height: 38px; font-size: 19px; }
      .side-nav { display: flex; width: auto; gap: 8px; }
      .nav-button { width: 52px; min-height: 48px; }
      .nav-button span { font-size: 10px; }
      .content { padding: 16px 12px 36px; }
      header, .split, .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .wallet-tx-row { grid-template-columns: 1fr; }
      header { display: grid; }
      input { min-width: 0; width: 100%; }
      .switch input { width: auto; }
      .seed-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .block-card { flex-basis: 108px; }
    }
  </style>
  <script defer src="/assets/luun-ui.js?v=43"></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="luunApp()" x-init="init()" @keydown.window.escape="closeModals()" x-cloak>
  <div class="app-shell">
    <aside class="sidebar" aria-label="Luun navigation">
      <div class="brand-mark" title="Luun">L</div>
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
      </nav>
    </aside>

    <main class="content">
    <header>
      <div>
        <h1 x-text="pageTitle()">Luun</h1>
      </div>
      <div class="muted" x-text="lastUpdatedLabel()"></div>
    </header>

    <div class="flash" :class="flash?.kind" x-show="flash" x-transition x-text="flash?.message"></div>

    <section x-show="tab === 'wallet'">
      <div class="page-title">
        <button class="wallet-balance-line" type="button" @click="openWalletUtxosModal" title="Show wallet UTXOs">
          <span class="tx-label">Balance</span>
          <span class="tx-value money">LUUN <span x-text="amountLabel(status.wallet_balance)"></span></span>
        </button>
      </div>
      <div class="wallet-grid">
        <div class="wallet-actions">
          <div class="panel">
            <h3>Send</h3>
            <form @submit.prevent="sendTransfer">
              <label>Recipient<input x-model="transferTo" autocomplete="off" required></label>
              <label>Amount<input x-model="transferAmount" type="number" min="0.000001" step="0.000001" required></label>
              <label>Fee<input x-model="transferFee" type="number" min="0" step="0.000001" required></label>
              <button class="advanced-toggle" type="button" @click="toggleSendAdvanced" x-text="showSendAdvanced ? 'Hide advanced' : 'Advanced'"></button>
              <div class="send-utxo-summary" x-show="showSendAdvanced">
                <div>Selected UTXOs: <span x-text="selectedTransferUtxos.length"></span></div>
                <div>Selected total: LUUN <span x-text="amountLabel(selectedTransferUtxoTotal())"></span></div>
                <div>Required: LUUN <span x-text="amountLabel(transferRequiredTotal())"></span></div>
                <div class="setup-feedback error" x-show="!selectedTransferUtxosCoverTransfer()">Selected UTXOs do not cover amount plus fee</div>
                <div class="send-utxo-list">
                  <template x-for="utxo in walletUtxos" :key="utxoOutpoint(utxo)">
                    <label class="send-utxo-option">
                      <input type="checkbox" :value="utxoOutpoint(utxo)" x-model="selectedTransferUtxos">
                      <span>
                        <span class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">LUUN <span x-text="amountLabel(utxo.amount)"></span></span></span>
                        <code class="tx-value hash" x-text="utxoOutpoint(utxo)"></code>
                      </span>
                    </label>
                  </template>
                  <div class="tx-modal-empty" x-show="walletUtxos.length === 0">No spendable UTXOs</div>
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
        <div class="panel">
          <div class="panel-head">
            <h3>Transactions</h3>
          </div>
          <div class="wallet-tx-list">
            <template x-for="tx in walletTransactions()" :key="tx.status + '-' + tx.signature">
              <div class="wallet-tx-row" :class="{ pending: tx.status === 'pending' }" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Wallet' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Wallet' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Wallet' })">
                <span class="pill" :class="tx.kind" x-text="tx.direction"></span>
                <div class="wallet-tx-main">
                  <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">LUUN <span x-text="amountLabel(tx.amount)"></span></span></div>
                  <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">LUUN <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                  <div class="tx-field"><span class="tx-label">Status</span><span class="tx-value text" x-text="txTitle(tx)"></span></div>
                  <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(tx.from)"></code></div>
                  <div class="tx-field" x-show="tx.to"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(tx.to)"></code></div>
                  <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                </div>
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
        <div class="panel">
          <h3>Mining</h3>
          <form class="mining-form" @submit.prevent="saveBurn">
            <div class="burn-fields">
              <label>LUUN per block<input x-model="burnAmountDraft" @input="burnAmountDirty = true" type="number" min="0" :max="burnSliderMax()" step="0.000001"></label>
              <label>Fee<input x-model="burnFeeDraft" @input="burnAmountDirty = true" type="number" min="0" step="0.000001"></label>
              <button class="primary" type="submit">Save</button>
            </div>
            <div class="burn-slider-panel">
              <div class="burn-slider-head"><span>Burn range</span><span><span x-text="burnAmountDraft"></span> LUUN</span></div>
              <div class="burn-slider-track">
                <input class="burn-range" x-model="burnAmountDraft" @input="burnAmountDirty = true" type="range" min="0" :max="burnSliderMax()" step="0.000001">
                <div class="break-even-marker" :style="breakEvenStyle()" title="Estimated break-even burn"></div>
              </div>
              <div class="burn-slider-scale"><span>0</span><span x-text="`${burnSliderMax()} LUUN`"></span></div>
              <div class="burn-slider-note" x-text="burnBreakEvenLabel()"></div>
            </div>
          </form>
        </div>
      </div>
    </section>

    <section x-show="tab === 'p2p'">
      <div class="panel">
        <h2>Metrics</h2>
        <div class="grid">
          <div class="metric"><div class="label">Inbound Sessions</div><div class="value" x-text="p2pMetrics.inbound_sessions_started ?? 0"></div></div>
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
          <div class="metric"><div class="label">Tx Ack Sent</div><div class="value" x-text="p2pMetrics.transaction_ack_envelopes_sent ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Ack Rx</div><div class="value" x-text="p2pMetrics.transaction_ack_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Accepted Sent</div><div class="value" x-text="p2pMetrics.transactions_accepted_sent ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Accepted Rx</div><div class="value" x-text="p2pMetrics.transactions_accepted_received ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Rejected Sent</div><div class="value" x-text="p2pMetrics.transactions_rejected_sent ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Rejected Rx</div><div class="value" x-text="p2pMetrics.transactions_rejected_received ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Retries</div><div class="value" x-text="p2pMetrics.transaction_retries_sent ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Ack Pending</div><div class="value" x-text="p2pMetrics.transaction_ack_pending ?? 0"></div></div>
        </div>
        <div class="metric-context">
          <div class="tx-field"><span class="tx-label">Last Failure</span><span class="tx-value text" x-text="p2pMetrics.last_session_failure || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Empty</span><span class="tx-value text" x-text="p2pMetrics.last_empty_frame_remote || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Parse</span><span class="tx-value text" x-text="p2pMetrics.last_parse_error || '-'"></span></div>
        </div>
      </div>
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
                <div class="detail-kv"><div class="key">Reward</div><div>LUUN <span x-text="amountLabel(selectedBlock.reward)"></span></div></div>
                <div class="detail-kv"><div class="key">Burns</div><div x-text="blockBurnCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Transfers</div><div x-text="blockTransferCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Total Burned</div><div>LUUN <span x-text="amountLabel(blockBurned(selectedBlock))"></span></div></div>
                <div class="detail-kv"><div class="key">VDF</div><div><span x-text="selectedBlock.vdf_rounds"></span> rounds</div></div>
              </div>
              <div class="tx-list">
                <h3>Transactions</h3>
                <template x-for="tx in selectedBlock.transactions" :key="tx.signature">
                  <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockMiner: selectedBlock.miner })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockMiner: selectedBlock.miner })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Block', blockHeight: selectedBlock.height, blockMiner: selectedBlock.miner })">
                    <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                    <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">LUUN <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                    <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">LUUN <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                    <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(txFrom(tx))"></code></div>
                    <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(txTo(tx))"></code></div>
                    <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
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
              <div class="mempool-item" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Mempool' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Mempool' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Mempool' })">
                <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">LUUN <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">LUUN <span x-text="amountLabel(tx.fee ?? 0)"></span></span></div>
                <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="short(txFrom(tx))"></code></div>
                <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="short(txTo(tx))"></code></div>
                <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
              </div>
            </template>
          </div>
        </section>
      </div>
    </section>
    </main>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showWalletUtxos" x-transition.opacity @click.self="closeWalletUtxosModal()" role="dialog" aria-modal="true" aria-labelledby="wallet-utxos-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="wallet-utxos-title">Wallet UTXOs</h2>
          <div class="tx-field"><span class="tx-label">Total</span><span class="tx-value money">LUUN <span x-text="amountLabel(status.wallet_balance)"></span></span></div>
        </div>
        <button type="button" @click="closeWalletUtxosModal">Close</button>
      </div>
      <div class="utxo-list">
        <template x-for="utxo in walletUtxos" :key="`${utxo.outpoint.txid}:${utxo.outpoint.index}`">
          <div class="wallet-utxo-row">
            <div class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">LUUN <span x-text="amountLabel(utxo.amount)"></span></span></div>
            <div class="tx-field"><span class="tx-label">Outpoint</span><code class="tx-value hash" x-text="txInputOutpoint({ outpoint: utxo.outpoint })"></code></div>
            <div class="tx-field"><span class="tx-label">Address</span><code class="tx-value hash" x-text="utxo.address"></code></div>
          </div>
        </template>
        <div class="tx-modal-empty" x-show="walletUtxos.length === 0">No wallet UTXOs</div>
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
        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">LUUN <span x-text="amountLabel(txAmount(selectedTransaction?.tx || {}))"></span></span></div>
        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money">LUUN <span x-text="amountLabel(selectedTransaction?.tx?.fee ?? 0)"></span></span></div>
        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="txFrom(selectedTransaction?.tx || {})"></code></div>
        <div class="tx-field" x-show="txTo(selectedTransaction?.tx || {})"><span class="tx-label">To</span><code class="tx-value hash" x-text="txTo(selectedTransaction?.tx || {})"></code></div>
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
              <div class="utxo-node-amount">LUUN <span x-text="amountLabel(output.amount)"></span></div>
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
        <div class="setup-welcome">Welcome to Luun</div>
        <h2 id="setup-title">Initial Setup</h2>
        <div class="setup-copy">Confirm the local wallet address and add any peers before this node starts from a saved configuration.</div>
      </div>
      <div class="setup-feedback" :class="setupFeedback?.kind" x-show="setupFeedback" x-transition x-text="setupFeedback?.message"></div>
      <div class="setup-grid">
        <div class="setup-section seed-panel">
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
    use std::{collections::BTreeMap, sync::Arc};

    use tokio::sync::Mutex;

    use crate::{
        adapters::{config_store, config_store::UiConfig},
        domain::{Block, MICRO_LUUN, OutPoint, Transaction, Wallet},
    };

    use super::{
        TransferForm, dev_seed_verify_bypass_allowed, persist_burn_settings_config,
        validate_transfer_form, wallet_transaction_rows,
    };

    #[test]
    fn dev_seed_verify_bypass_requires_env_flag() {
        assert!(dev_seed_verify_bypass_allowed(true));
        assert!(!dev_seed_verify_bypass_allowed(false));
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

        let outputs =
            super::known_output_index(&allocations, &chain, std::slice::from_ref(&pending_burn));
        let rows = wallet_transaction_rows(
            alice.address(),
            vec![pending_burn.clone()],
            &chain,
            &outputs,
        );

        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].signature, pending_burn.signature());
        assert_eq!(rows[0].signature, old_received.signature());
        assert_eq!(rows[0].inputs[0].amount, Some(100));
        assert_eq!(rows[0].status, "confirmed");
        assert_eq!(rows[0].block_height, Some(31));
        assert_eq!(rows[0].block_miner.as_deref(), Some("miner"));
        assert_eq!(rows[0].direction, "received");
    }

    fn fake_block(height: u64, transactions: Vec<Transaction>) -> Block {
        Block {
            height,
            prev_hash: format!("prev-{height}"),
            timestamp_ms: height,
            miner: "miner".to_string(),
            reward: 100,
            vdf_rounds: 0,
            vdf_output: "vdf".to_string(),
            leader_proof: None,
            transactions,
            hash: format!("hash-{height}"),
        }
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

        persist_burn_settings_config(&ui_config, &config_path, 50 * MICRO_LUUN, 3 * MICRO_LUUN)
            .await
            .unwrap();
        let config = config_store::load_or_create(&config_path).unwrap();

        assert_eq!(config.burn_per_block, 50 * MICRO_LUUN);
        assert_eq!(config.burn_fee, 3 * MICRO_LUUN);
    }

    #[test]
    fn transfer_form_requires_recipient_amount_and_fee() {
        let error = validate_transfer_form(TransferForm {
            to: " ".to_string(),
            amount: 1,
            fee: Some(1),
            utxos: String::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("recipient is required"));

        let error = validate_transfer_form(TransferForm {
            to: "abc".to_string(),
            amount: 0,
            fee: Some(1),
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
            fee: None,
            utxos: String::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("fee is required"));
    }

    #[test]
    fn transfer_form_trims_recipient() {
        let (to, amount, fee, utxos) = validate_transfer_form(TransferForm {
            to: "  abc  ".to_string(),
            amount: 2,
            fee: Some(3),
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
            fee: Some(3),
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
