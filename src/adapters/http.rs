use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::header,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    adapters::p2p::GossipNetwork,
    app::{NodeStatus, PeerInfo, SharedNode, SharedPeerBook},
    domain::{Amount, Block, Transaction},
};

const EXPLORER_LIMIT: usize = 50;
const EXPLORER_PAGE_LIMIT: usize = 30;

#[derive(Clone)]
struct HttpState {
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
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
struct BlocksQuery {
    before_height: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ActionResponse {
    ok: bool,
    error: Option<String>,
}

pub async fn serve(
    node: SharedNode,
    peers: SharedPeerBook,
    gossip: GossipNetwork,
    addr: SocketAddr,
) -> Result<()> {
    let state = HttpState {
        node,
        peers,
        gossip,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/alpine.min.js", get(alpine_js))
        .route("/assets/mivora-ui.js", get(app_js))
        .route("/api/status", get(api_status))
        .route("/api/blocks", get(api_blocks))
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

async fn api_mempool(State(state): State<HttpState>) -> Json<Vec<Transaction>> {
    Json(state.node.lock().await.pending_transactions())
}

async fn api_peers(State(state): State<HttpState>) -> Json<Vec<PeerInfo>> {
    Json(state.peers.lock().await.list())
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
    state.peers.lock().await.add_peer(form.peer);
    action_json(Ok(()))
}

async fn peer_form(State(state): State<HttpState>, Form(form): Form<PeerForm>) -> Redirect {
    state.peers.lock().await.add_peer(form.peer);
    Redirect::to("/")
}

async fn set_burn_per_block(state: &HttpState, amount: Amount) -> Result<()> {
    let result = {
        let mut node = state.node.lock().await;
        let result = node.set_burn_per_block(amount);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => state.gossip.broadcast(result.1).await,
        Err(error) => Err(error),
    }
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
    :root { color-scheme: light; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    body { margin: 0; background: #f7f8fa; color: #17202a; }
    main { max-width: 1180px; margin: 0 auto; padding: 18px 18px 48px; }
    header { display: flex; justify-content: space-between; gap: 18px; align-items: flex-start; padding: 0 0 16px; border-bottom: 1px solid #d9e0e7; }
    h1 { margin: 0 0 4px; font-size: 26px; }
    h2 { margin: 0 0 12px; font-size: 18px; }
    h3 { margin: 0 0 10px; font-size: 15px; }
    button { border: 1px solid #c9d2dc; border-radius: 6px; padding: 8px 11px; font: inherit; font-weight: 700; background: white; color: #17202a; cursor: pointer; }
    button:hover { border-color: #157a6e; color: #0f665d; }
    button.primary { background: #116149; border-color: #116149; color: white; }
    button.primary:hover { background: #0b4f3b; color: white; }
    button:disabled { cursor: default; opacity: .55; }
    .tabs { display: flex; flex-wrap: wrap; gap: 8px; margin: 18px 0; }
    .tabs button.active { background: #17202a; border-color: #17202a; color: white; }
    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; }
    .metric, .panel { background: white; border: 1px solid #dde3ea; border-radius: 8px; padding: 12px; }
    .metric .label { color: #667789; font-size: 12px; text-transform: uppercase; letter-spacing: .06em; }
    .metric .value { margin-top: 6px; font-weight: 800; overflow-wrap: anywhere; }
    .panel { margin-bottom: 12px; }
    .split { display: grid; grid-template-columns: minmax(0, 1fr) minmax(320px, .7fr); gap: 12px; }
    form { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    label { display: grid; gap: 5px; color: #465564; font-size: 13px; }
    input { min-width: 180px; border: 1px solid #b8c4cf; border-radius: 6px; padding: 9px 10px; font: inherit; background: white; }
    table { width: 100%; border-collapse: collapse; font-size: 13px; }
    th, td { text-align: left; border-bottom: 1px solid #e2e7ed; padding: 8px; vertical-align: top; }
    th { color: #667789; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; }
    code { overflow-wrap: anywhere; }
    .table-wrap { overflow-x: auto; }
    .muted { color: #667789; }
    .flash { border-radius: 6px; padding: 10px 12px; margin: 12px 0; border: 1px solid; font-weight: 700; }
    .flash.success { color: #0b5e43; background: #effbf4; border-color: #a7dfbd; }
    .flash.error { color: #9b1c1c; background: #fff1f1; border-color: #f0b7b7; }
    .ok { color: #0b5e43; }
    .summary-row { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 8px; }
    .explorer-shell { display: grid; gap: 12px; }
    .block-rail-wrap { background: white; border: 1px solid #dde3ea; border-radius: 8px; padding: 12px; overflow: hidden; }
    .block-rail-head { display: flex; justify-content: space-between; gap: 10px; align-items: center; margin-bottom: 10px; }
    .block-rail { display: flex; gap: 8px; overflow-x: auto; padding: 1px 0 10px; scroll-snap-type: x proximity; }
    .block-card { flex: 0 0 118px; min-height: 96px; display: grid; gap: 6px; border: 1px solid #dde3ea; border-radius: 8px; padding: 9px; background: #fbfcfd; color: #17202a; text-align: left; scroll-snap-align: start; }
    .block-card:hover { border-color: #8bbdb5; color: #0f665d; }
    .block-card.selected { background: #eef8f5; border-color: #157a6e; box-shadow: inset 0 0 0 1px #157a6e; }
    .block-card.new-block { animation: block-arrive .45s ease both; }
    @keyframes block-arrive { from { opacity: .2; transform: translateX(-12px); } to { opacity: 1; transform: translateX(0); } }
    .block-height { font-size: 18px; font-weight: 900; }
    .block-meta { display: flex; gap: 8px; color: #667789; font-size: 12px; }
    .block-hash { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11px; overflow-wrap: anywhere; color: #465564; }
    .rail-actions { display: flex; justify-content: flex-end; padding-top: 2px; }
    .detail-grid { display: grid; grid-template-columns: minmax(0, .9fr) minmax(0, 1.1fr); gap: 12px; }
    .detail-kv { display: grid; grid-template-columns: 90px minmax(0, 1fr); gap: 8px; font-size: 13px; margin: 7px 0; }
    .detail-kv .key { color: #667789; }
    .tx-list { display: grid; gap: 8px; }
    .tx-card { border: 1px solid #e2e7ed; border-radius: 8px; padding: 10px; background: #fbfcfd; }
    .tx-head { display: flex; justify-content: space-between; gap: 8px; margin-bottom: 6px; font-weight: 800; }
    .pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 2px 8px; font-size: 12px; font-weight: 800; background: #e8eef4; color: #34495e; }
    .pill.burn { background: #fff0d9; color: #845400; }
    .pill.transfer { background: #e5f5ee; color: #0b5e43; }
    .mempool-strip { display: flex; gap: 8px; overflow-x: auto; padding-bottom: 4px; }
    .mempool-item { flex: 0 0 200px; border: 1px solid #e2e7ed; border-radius: 8px; padding: 10px; background: white; }
    @media (max-width: 920px) { .summary-row, .detail-grid { grid-template-columns: 1fr 1fr; } }
    @media (max-width: 760px) { .split, .summary-row, .detail-grid { grid-template-columns: 1fr; } input { min-width: 0; width: 100%; } .block-card { flex-basis: 108px; } }
  </style>
  <script defer src="/assets/mivora-ui.js?v=9"></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="mivoraApp()" x-init="init()" x-cloak>
  <main>
    <header>
      <div>
        <h1>Mivora</h1>
        <div class="muted">Burn lottery devnet</div>
      </div>
      <div class="muted" x-text="lastUpdatedLabel()"></div>
    </header>

    <div class="flash" :class="flash?.kind" x-show="flash" x-transition x-text="flash?.message"></div>

    <section class="summary-row">
      <div class="metric"><div class="label">Node</div><div class="value" x-text="status.name || '-'"></div></div>
      <div class="metric"><div class="label">Local Height</div><div class="value" x-text="status.chain?.height ?? '-'"></div></div>
      <div class="metric"><div class="label">Shared Height</div><div class="value" x-text="sharedHeightLabel()"></div></div>
      <div class="metric"><div class="label">Wallet Balance</div><div class="value" x-text="status.wallet_balance ?? '-'"></div></div>
      <div class="metric"><div class="label">Mempool</div><div class="value" x-text="mempool.length"></div></div>
    </section>

    <nav class="tabs">
      <button :class="{ active: tab === 'wallet' }" @click="tab = 'wallet'">Wallet</button>
      <button :class="{ active: tab === 'p2p' }" @click="tab = 'p2p'">P2P</button>
      <button :class="{ active: tab === 'chain' }" @click="tab = 'chain'">Explorer</button>
    </nav>

    <section x-show="tab === 'wallet'">
      <div class="split">
        <div class="panel">
          <h2>Wallet</h2>
          <p><code x-text="status.wallet_address || '-'"></code></p>
          <div class="grid">
            <div class="metric"><div class="label">Balance</div><div class="value" x-text="status.wallet_balance ?? '-'"></div></div>
            <div class="metric"><div class="label">Current Leader</div><div class="value" x-text="isLeaderLabel()"></div></div>
            <div class="metric"><div class="label">Last Burn Height</div><div class="value" x-text="status.mining?.last_auto_burn_height ?? '-'"></div></div>
          </div>
        </div>
        <div class="panel">
          <h3>Burn Rate</h3>
          <form @submit.prevent="saveBurn">
            <label>Coins per block<input x-model.number="burnAmount" type="number" min="0"></label>
            <button class="primary" type="submit">Save</button>
          </form>
        </div>
      </div>
      <div class="panel">
        <h3>Send Coins</h3>
        <form @submit.prevent="sendTransfer">
          <label>Recipient<input x-model="transferTo" autocomplete="off"></label>
          <label>Amount<input x-model.number="transferAmount" type="number" min="1"></label>
          <button class="primary" type="submit">Send</button>
        </form>
      </div>
    </section>

    <section x-show="tab === 'p2p'">
      <div class="panel">
        <h2>P2P</h2>
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
                <div class="block-hash" x-text="short(block.hash)"></div>
              </button>
            </template>
          </div>
          <div class="rail-actions">
            <button @click="loadOlderBlocks" :disabled="loadingOlder || !hasMoreBlocks" x-text="olderButtonLabel()"></button>
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
</body>
</html>"#;
