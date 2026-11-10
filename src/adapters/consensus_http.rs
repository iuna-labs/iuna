//! HTTP API for operator screens.

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use getrandom::getrandom;
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    adapters::{
        consensus_index_html::INDEX_HTML,
        consensus_local_state::{self, MiningPreference},
        consensus_network::{
            ConnectedPeer, ConsensusRelay, PeerAddress, PeerRegistry, PeerTarget,
            SharedConsensusNode,
        },
        consensus_ui_store::{ConsensusUiStore, UiHeartbeat},
        consensus_wallet_store,
    },
    app::{
        consensus_node::{ChainRepository, Node},
        wallet_commands::{TicketParameters, Wallet},
    },
    consensus::{
        Address, ConsensusError, ConsensusState, GENESIS_MINER, GENESIS_REWARD,
        HEARTBEAT_TARGET_INTERVAL_SECS, HeartbeatBlock, LEDGER_TARGET_INTERVAL_HEARTBEATS,
        MAX_TARGET, MAX_TICKET_DURATION, NETWORK_ID, OutPoint, PublicKey, TicketTransaction,
        Transaction, address_from_public_key, block_payouts, minimum_ticket_budget, sidecar_winner,
        split_transaction_fees, subsidy, tagged_hash, ticket_fee, ticket_input_state,
        ticket_lifecycle_length,
    },
};

#[derive(Clone)]
pub struct ConsensusHttpState {
    pub node_instance_id: String,
    pub node: SharedConsensusNode,
    pub ui_store: ConsensusUiStore,
    pub wallet: Arc<RwLock<WalletState>>,
    pub operator_token_hash: Option<[u8; 32]>,
    pub trusted_management: bool,
    pub relay: Option<ConsensusRelay>,
    pub peers: PeerRegistry,
    pub heartbeat_mining: Arc<AtomicBool>,
    pub heartbeat_mining_workers: Arc<AtomicUsize>,
    pub heartbeat_hashes: Arc<AtomicU64>,
    pub ledger_mining: Arc<AtomicBool>,
    pub ledger_last_ticket_height: Arc<AtomicU64>,
}

pub struct WalletState {
    pub path: PathBuf,
    pub unlocked: Option<Arc<Wallet>>,
    pub public_key: Option<[u8; 32]>,
}

#[derive(Debug, Serialize)]
pub struct OperatorStatus {
    pub app_version: &'static str,
    pub network: &'static str,
    pub node_instance_id: String,
    pub heartbeat_height: u64,
    pub tip_hash: String,
    pub ledger_blocks: u64,
    pub minted: u64,
    pub tracked_mining_tickets: usize,
    pub utxos: usize,
    pub pending_ledger_transactions: usize,
    pub pending_mining_tickets: usize,
    pub wallet_public_key: Option<String>,
    pub wallet_receive_address: Option<String>,
    pub wallet_balance: u64,
    pub wallet_spendable_balance: u64,
    pub wallet_utxos: usize,
    pub wallet_spendable_utxos: usize,
    pub wallet_unlocked: bool,
    pub management_enabled: bool,
    /// Management needs `Authorization: Bearer <operator token>`; otherwise it is open to the
    /// node's own same-origin local interface.
    pub management_requires_token: bool,
    pub heartbeat_mining_enabled: bool,
    pub heartbeat_mining_workers: usize,
    pub heartbeat_mining_max_workers: usize,
    /// Cumulative nonces attempted by this node since process start.
    pub heartbeat_hashes: u64,
    /// Active Proof-of-Expenditure weight owned by this node's wallet at the current height.
    pub ledger_ticket_power: u64,
    pub ledger_mining_enabled: bool,
    pub ledger_last_ticket_height: Option<u64>,
    pub connected_peers: usize,
    pub target: String,
    pub difficulty: String,
    /// Easiest allowed target (difficulty 1), so clients can chart difficulty per block.
    pub pow_limit: String,
}

#[derive(Debug, Deserialize)]
pub struct LedgerTransactionRequest {
    pub recipient: String,
    pub amount: u64,
    pub fee: u64,
}

#[derive(Debug, Deserialize)]
pub struct MiningTicketRequest {
    pub budget: u64,
    pub duration: u64,
    pub fee: u64,
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatMiningRequest {
    pub enabled: bool,
    #[serde(default = "default_mining_workers")]
    pub workers: usize,
}

#[derive(Debug, Serialize)]
pub struct HeartbeatMiningResponse {
    pub enabled: bool,
    pub workers: usize,
    pub bootstrap_ticket_submitted: bool,
}

#[derive(Debug, Deserialize)]
pub struct LedgerMiningRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct LedgerMiningResponse {
    pub enabled: bool,
}

const MINING_WORKER_HARD_LIMIT: usize = 256;

fn default_mining_workers() -> usize {
    1
}

fn max_mining_workers() -> usize {
    MINING_WORKER_HARD_LIMIT
}

#[derive(Debug, Deserialize)]
pub struct MiningTicketQuoteQuery {
    pub duration: u64,
}

#[derive(Debug, Deserialize)]
pub struct WalletAccessRequest {
    pub password: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub recovery_phrase: Option<String>,
    /// Required only for `change_password`.
    #[serde(default)]
    pub new_password: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletAccessResponse {
    pub created: bool,
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_phrase: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PublicWalletUtxo {
    pub transaction_id: String,
    pub output_index: u32,
    pub value: u64,
    pub reserved: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicWalletView {
    pub network: &'static str,
    pub heartbeat_height: u64,
    pub tip_hash: String,
    pub ledger_blocks: u64,
    pub address: String,
    /// True even if every output ever sent here has since been spent.
    pub used: bool,
    pub balance: u64,
    pub spendable_balance: u64,
    pub utxos: Vec<PublicWalletUtxo>,
    /// Ledger transactions in this node's mempool that spend outputs of this wallet.
    pub pending_transaction_ids: Vec<String>,
    /// Balances always cover every output; the list stops at `MAX_PUBLIC_WALLET_UTXOS`.
    pub utxos_truncated: bool,
    /// Offset for the next stable page of outputs at this Heartbeat tip.
    pub next_utxo_offset: Option<usize>,
}

/// One address in a [`PublicWalletAddressesView`]: enough for a deterministic wallet to find its
/// used addresses and balance without fetching the outputs of every empty one.
#[derive(Debug, Serialize)]
pub struct PublicWalletAddressSummary {
    pub address: String,
    /// True even if every output ever sent here has since been spent.
    pub used: bool,
    pub balance: u64,
    pub spendable_balance: u64,
    pub utxo_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PublicWalletAddressesView {
    pub network: &'static str,
    pub heartbeat_height: u64,
    pub tip_hash: String,
    pub ledger_blocks: u64,
    /// In request order.
    pub addresses: Vec<PublicWalletAddressSummary>,
    /// Ledger transactions in this node's mempool that spend outputs of any requested address.
    pub pending_transaction_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublicWalletAddressesRequest {
    pub addresses: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PublicWalletQuery {
    #[serde(default)]
    pub utxo_offset: usize,
}

#[derive(Debug, Serialize)]
pub struct WalletActivityView {
    pub public_key: String,
    pub scanned_heartbeats: u64,
    pub utxos: Vec<PublicWalletUtxo>,
    pub pending: Vec<WalletActivityItem>,
    pub confirmed: Vec<WalletActivityItem>,
}

#[derive(Debug, Serialize)]
pub struct WalletActivityItem {
    /// `sent`, `received`, `self`, `ticket`, `heartbeat_reward` or `ledger_reward`.
    pub kind: &'static str,
    pub id: String,
    pub heartbeat_height: Option<u64>,
    pub timestamp: Option<u64>,
    /// Value leaving the wallet (sent, ticket budget) or arriving in it (received, self).
    pub amount: u64,
    pub counterparty: Option<String>,
}

/// How far back the node wallet history looks: about one day of Heartbeats.
const WALLET_ACTIVITY_HEARTBEATS: u64 = 1_440;
/// The UI polls the mempool constantly and shows a strip; the status holds the full counts.
const MEMPOOL_PREVIEW_ITEMS: usize = 200;
const WALLET_ACTIVITY_ITEMS_PER_TYPE: usize = 200;

/// Bounds the public response size; such a wallet can still spend by consolidating outputs.
const MAX_PUBLIC_WALLET_UTXOS: usize = 1_000;
/// Fits the stable root address plus one 20-address gap window of both wallet branches.
const MAX_PUBLIC_WALLET_ADDRESSES: usize = 64;
/// JSON represents fixed-size byte arrays as decimal lists and is substantially larger than the
/// consensus encoding. This still bounds unauthenticated request memory while allowing a wallet
/// page of 1,000 outputs to be consolidated in one otherwise-valid transaction.
const MAX_PUBLIC_TRANSACTION_JSON_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct BlockInputView {
    pub transaction_id: String,
    pub output_index: u32,
    pub public_key: String,
    pub signature: String,
    pub value: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BlockOutputView {
    pub owner: String,
    pub value: u64,
}

#[derive(Debug, Serialize)]
pub struct TicketPurchaseView {
    pub id: String,
    pub version: u16,
    pub budget: u64,
    pub duration: u64,
    pub lifecycle_heartbeats: u64,
    pub fee: Option<u64>,
    /// The Heartbeat miner that received the inclusion fee.
    pub fee_recipient: String,
    pub mining_public_key: String,
    pub lottery_seed: String,
    pub inputs: Vec<BlockInputView>,
    pub change: Option<BlockOutputView>,
}

/// One recipient's part of a transaction fee. `owner` is unknown until the transaction is mined.
#[derive(Debug, Serialize)]
pub struct FeeShareView {
    pub value: u64,
    pub owner: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FeeSplitView {
    pub heartbeat_miner: FeeShareView,
    pub ledger_winner: FeeShareView,
}

#[derive(Debug, Serialize)]
pub struct LedgerTransactionView {
    pub id: String,
    pub version: u16,
    pub inputs: Vec<BlockInputView>,
    pub outputs: Vec<BlockOutputView>,
    pub total_output: u64,
    pub fee: Option<u64>,
    /// In a block the shares sum exactly to the block's paid fees, see `block_fee_split`. In the
    /// mempool this is the transaction's own split, which block rounding can shift by one unit.
    pub fee_split: Option<FeeSplitView>,
}

#[derive(Debug, Serialize)]
pub struct LedgerBlockView {
    pub candidate_hash: String,
    pub candidate_signature: Option<String>,
    pub deadline_fallback: bool,
    pub winning_ticket_id: String,
    pub lottery_heartbeat: String,
    pub lottery_height: u64,
    pub slot: u64,
    pub transactions: Vec<LedgerTransactionView>,
}

#[derive(Debug, Serialize)]
pub struct MempoolView {
    pub ledger_transactions: Vec<MempoolTransactionSummary>,
    pub mining_tickets: Vec<MempoolTicketSummary>,
}

#[derive(Debug, Serialize)]
pub struct MempoolTransactionSummary {
    pub id: String,
    pub version: u16,
    pub input_count: usize,
    pub output_count: usize,
    pub total_output: u64,
    pub fee: Option<u64>,
    pub fee_split: Option<FeeSplitView>,
}

#[derive(Debug, Serialize)]
pub struct MempoolTicketSummary {
    pub id: String,
    pub version: u16,
    pub budget: u64,
    pub duration: u64,
    pub lifecycle_heartbeats: u64,
    pub fee: Option<u64>,
    pub mining_public_key: String,
}

/// Full detail of one mempool item, including inputs and signatures.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum MempoolItemView {
    Transaction(LedgerTransactionView),
    Ticket(MempoolTicketView),
}

#[derive(Debug, Serialize)]
pub struct MempoolTicketView {
    pub id: String,
    pub version: u16,
    pub budget: u64,
    pub duration: u64,
    pub lifecycle_heartbeats: u64,
    pub fee: Option<u64>,
    pub mining_public_key: String,
    pub lottery_seed: String,
    pub inputs: Vec<BlockInputView>,
    pub change: Option<BlockOutputView>,
}

#[derive(Debug, Serialize)]
pub struct BlockDetailView {
    pub block_hash: String,
    pub height: u64,
    pub rewards: BlockRewardsView,
    pub tickets: Vec<TicketPurchaseView>,
    pub ledger: Option<LedgerBlockView>,
}

#[derive(Debug, Serialize)]
pub struct BlockRewardsView {
    pub heartbeat: HeartbeatRewardView,
    pub ledger: Option<LedgerRewardView>,
}

#[derive(Debug, Serialize)]
pub struct HeartbeatRewardView {
    pub total: u64,
    pub ledger_transaction_fees: u64,
    pub ticket_inclusion_fees: u64,
    pub expenditure: u64,
}

#[derive(Debug, Serialize)]
pub struct LedgerRewardView {
    pub total: u64,
    pub subsidy: u64,
    pub ledger_transaction_fees: u64,
}

#[derive(Debug, Serialize)]
pub struct MiningTicketQuote {
    pub duration: u64,
    pub current_next_ledger_reward: u64,
    pub current_minimum_budget: u64,
    pub minimum_budget_after_same_heartbeat_ledger: u64,
    pub recommended_budget: u64,
    pub heartbeat_target_seconds: u64,
    pub ledger_target_heartbeats: u64,
}

#[derive(Debug, Serialize)]
pub struct SubmissionResponse {
    pub id: String,
    pub fee: u64,
}

#[derive(Debug, Serialize)]
pub struct MinedHeartbeatResponse {
    pub hash: String,
    pub height: u64,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

type ApiResult<T> = std::result::Result<Json<T>, (StatusCode, Json<ApiError>)>;
type ApiErrorResponse = (StatusCode, Json<ApiError>);

/// The operator router for a listener on `address`. On a loopback listener every request must
/// name a loopback Host: a DNS-rebound page reaches the same socket but sends its own host name,
/// and reads carry no token. Other listeners sit behind an ingress with its own host names.
pub fn operator_router(state: ConsensusHttpState, address: SocketAddr) -> Router {
    let router = router(state);
    if address.ip().is_loopback() {
        router.layer(middleware::from_fn(require_loopback_host))
    } else {
        router
    }
}

async fn require_loopback_host(request: Request, next: Next) -> Response {
    let loopback = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_loopback_host);
    if loopback {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

fn is_loopback_host(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        Some((name, port))
            if !name.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    };
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

pub fn router(state: ConsensusHttpState) -> Router {
    let management = Router::new()
        .route("/api/wallet", post(access_wallet))
        .route("/api/ledger-transactions", post(submit_transaction))
        .route("/api/mining-tickets", post(submit_ticket))
        .route("/api/hash-mining", post(set_heartbeat_mining))
        .route("/api/ledger-mining", post(set_ledger_mining))
        .route("/api/peers", post(change_peer))
        .route("/api/hash-mining/heartbeat", post(mine_heartbeat))
        .route_layer(DefaultBodyLimit::max(4 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_operator_auth,
        ));
    Router::new()
        .route("/", get(index))
        .route("/assets/iuna-ui.js", get(iuna_ui_javascript))
        .route("/assets/alpine.min.js", get(alpine_javascript))
        .route("/api/status", get(status))
        .route("/api/heartbeats", get(heartbeats))
        .route("/api/metrics", get(metrics))
        .route("/api/blocks/{block_hash}", get(block_detail))
        .route("/api/mempool", get(mempool))
        .route("/api/mempool/{id}", get(mempool_item))
        .route("/api/peers", get(peers))
        .route("/api/wallet/activity", get(wallet_activity))
        .route("/api/peer-targets", get(peer_targets))
        .route("/api/mining-ticket-quote", get(mining_ticket_quote))
        .merge(management)
        .with_state(state)
}

/// Deliberately narrow router served only when the operator opts into a separate public listener.
pub fn public_wallet_router(state: ConsensusHttpState) -> Router {
    Router::new()
        .route("/api/public/wallet/{address}", get(public_wallet_view))
        .route(
            "/api/public/wallet-addresses",
            post(public_wallet_addresses).options(public_wallet_preflight),
        )
        .route(
            "/api/public/transactions",
            post(submit_signed_transaction).options(public_wallet_preflight),
        )
        .route_layer(DefaultBodyLimit::max(MAX_PUBLIC_TRANSACTION_JSON_BYTES))
        .route_layer(middleware::from_fn(public_wallet_cors))
        .with_state(state)
}

async fn public_wallet_view(
    State(state): State<ConsensusHttpState>,
    Path(address): Path<String>,
    Query(query): Query<PublicWalletQuery>,
) -> ApiResult<PublicWalletView> {
    let address = decode_hex_32(&address, "address")
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let mut node = state.node.lock().await;
    let owned = node.owned_outputs(&address);
    let used = node.address_used(&address);
    let chain = node.chain().tip();
    let mut balance = 0_u64;
    let mut spendable_balance = 0_u64;
    let mut utxos = Vec::new();
    let page_start = query.utxo_offset.min(owned.len());
    let page_end = page_start
        .saturating_add(MAX_PUBLIC_WALLET_UTXOS)
        .min(owned.len());
    let utxos_truncated = page_end < owned.len();
    for (index, (point, output)) in owned.iter().enumerate() {
        balance = balance.checked_add(output.value).ok_or_else(|| {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "wallet balance overflow")
        })?;
        let is_reserved = node.is_input_reserved(point);
        if !is_reserved {
            spendable_balance = spendable_balance.checked_add(output.value).ok_or_else(|| {
                api_error(StatusCode::INTERNAL_SERVER_ERROR, "wallet balance overflow")
            })?;
        }
        if index < page_start || index >= page_end {
            continue;
        }
        utxos.push(PublicWalletUtxo {
            transaction_id: hex(&point.transaction_id),
            output_index: point.output_index,
            value: output.value,
            reserved: is_reserved,
        });
    }
    let pending_transaction_ids = node
        .pending_transactions()
        .iter()
        .filter(|transaction| {
            transaction
                .inputs
                .iter()
                .any(|input| address_from_public_key(&input.public_key) == address)
        })
        .map(|transaction| hex(&transaction.id()))
        .collect();
    Ok(Json(PublicWalletView {
        network: NETWORK_ID,
        heartbeat_height: chain.height,
        tip_hash: hex(&chain.tip),
        ledger_blocks: chain.ledger_blocks,
        address: hex(&address),
        used,
        balance,
        spendable_balance,
        utxos,
        pending_transaction_ids,
        utxos_truncated,
        next_utxo_offset: utxos_truncated.then_some(page_end),
    }))
}

/// Summaries for a batch of addresses at one Heartbeat tip, so wallet discovery costs one
/// request per gap window instead of one per address.
async fn public_wallet_addresses(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<PublicWalletAddressesRequest>,
) -> ApiResult<PublicWalletAddressesView> {
    if request.addresses.is_empty() || request.addresses.len() > MAX_PUBLIC_WALLET_ADDRESSES {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("request between 1 and {MAX_PUBLIC_WALLET_ADDRESSES} addresses"),
        ));
    }
    let addresses = request
        .addresses
        .iter()
        .map(|address| decode_hex_32(address, "address"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let overflow = || api_error(StatusCode::INTERNAL_SERVER_ERROR, "wallet balance overflow");
    let mut node = state.node.lock().await;
    let mut summaries = Vec::with_capacity(addresses.len());
    for address in &addresses {
        let owned = node.owned_outputs(address);
        let mut balance = 0_u64;
        let mut spendable_balance = 0_u64;
        for (point, output) in &owned {
            balance = balance.checked_add(output.value).ok_or_else(overflow)?;
            if !node.is_input_reserved(point) {
                spendable_balance = spendable_balance
                    .checked_add(output.value)
                    .ok_or_else(overflow)?;
            }
        }
        summaries.push(PublicWalletAddressSummary {
            address: hex(address),
            used: node.address_used(address),
            balance,
            spendable_balance,
            utxo_count: owned.len(),
        });
    }
    let requested = addresses.iter().copied().collect::<BTreeSet<_>>();
    let pending_transaction_ids = node
        .pending_transactions()
        .iter()
        .filter(|transaction| {
            transaction
                .inputs
                .iter()
                .any(|input| requested.contains(&address_from_public_key(&input.public_key)))
        })
        .map(|transaction| hex(&transaction.id()))
        .collect();
    let chain = node.chain().tip();
    Ok(Json(PublicWalletAddressesView {
        network: NETWORK_ID,
        heartbeat_height: chain.height,
        tip_hash: hex(&chain.tip),
        ledger_blocks: chain.ledger_blocks,
        addresses: summaries,
        pending_transaction_ids,
    }))
}

fn transaction_activity(
    owners: &BTreeSet<Address>,
    transaction: &Transaction,
    height: Option<u64>,
    timestamp: Option<u64>,
) -> Option<WalletActivityItem> {
    let spends = transaction
        .inputs
        .iter()
        .any(|input| owners.contains(&address_from_public_key(&input.public_key)));
    let to_owner: u64 = transaction
        .outputs
        .iter()
        .filter(|output| owners.contains(&output.owner))
        .map(|output| output.value)
        .sum();
    let others = transaction
        .outputs
        .iter()
        .filter(|output| !owners.contains(&output.owner))
        .collect::<Vec<_>>();
    let (kind, amount, counterparty) = if spends && others.is_empty() {
        ("self", to_owner, None)
    } else if spends {
        (
            "sent",
            others.iter().map(|output| output.value).sum(),
            Some(hex(&others[0].owner)),
        )
    } else if to_owner > 0 {
        (
            "received",
            to_owner,
            transaction
                .inputs
                .first()
                .map(|input| hex(&address_from_public_key(&input.public_key))),
        )
    } else {
        return None;
    };
    Some(WalletActivityItem {
        kind,
        id: hex(&transaction.id()),
        heartbeat_height: height,
        timestamp,
        amount,
        counterparty,
    })
}

fn ticket_activity(
    owners: &BTreeSet<Address>,
    ticket: &crate::consensus::TicketTransaction,
    height: Option<u64>,
    timestamp: Option<u64>,
) -> Option<WalletActivityItem> {
    ticket
        .inputs
        .iter()
        .any(|input| owners.contains(&address_from_public_key(&input.public_key)))
        .then(|| WalletActivityItem {
            kind: "ticket",
            id: hex(&ticket.id()),
            heartbeat_height: height,
            timestamp,
            amount: ticket.budget,
            counterparty: None,
        })
}

fn push_wallet_activity(
    items: &mut Vec<WalletActivityItem>,
    counts: &mut [usize; 4],
    item: WalletActivityItem,
) {
    let category = match item.kind {
        "sent" | "received" | "self" => 0,
        "ticket" => 1,
        "heartbeat_reward" => 2,
        "ledger_reward" => 3,
        _ => 0,
    };
    if counts[category] < WALLET_ACTIVITY_ITEMS_PER_TYPE {
        counts[category] += 1;
        items.push(item);
    }
}

/// The node wallet's UTXOs, mempool entries and recent confirmed activity, newest first.
async fn wallet_activity(State(state): State<ConsensusHttpState>) -> ApiResult<WalletActivityView> {
    let (public_key, unlocked) = {
        let wallet = state.wallet.read().expect("wallet state lock poisoned");
        (
            wallet
                .public_key
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "this node has no wallet"))?,
            wallet.unlocked.clone(),
        )
    };
    // Rewards are read before the node lock: SQLite must not hold up the node.
    let store = state.ui_store.clone();
    let owners = unlocked.map_or_else(
        || BTreeSet::from([address_from_public_key(&public_key)]),
        |wallet| wallet.owned_addresses(),
    );
    let rewards_owner = hex(&public_key);
    let rewards = tokio::task::spawn_blocking(move || {
        store.wallet_rewards(&rewards_owner, WALLET_ACTIVITY_HEARTBEATS)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|rewards| rewards)
    .unwrap_or_else(|error| {
        // Screen data must not take the wallet's own outputs and transfers down with it.
        eprintln!("reading wallet rewards failed: {error:#}");
        Default::default()
    });
    let mut node = state.node.lock().await;
    let owned = owners
        .iter()
        .flat_map(|owner| node.owned_outputs(owner))
        .collect::<Vec<_>>();
    let utxos = owned
        .iter()
        .take(MAX_PUBLIC_WALLET_UTXOS)
        .map(|(point, output)| PublicWalletUtxo {
            transaction_id: hex(&point.transaction_id),
            output_index: point.output_index,
            value: output.value,
            reserved: node.is_input_reserved(point),
        })
        .collect();
    let mut pending = node
        .pending_transactions()
        .iter()
        .filter_map(|transaction| transaction_activity(&owners, transaction, None, None))
        .collect::<Vec<_>>();
    pending.extend(
        node.pending_tickets()
            .iter()
            .filter_map(|ticket| ticket_activity(&owners, ticket, None, None)),
    );
    let chain = node.chain();
    let mut confirmed = Vec::new();
    let mut confirmed_counts = [0; 4];
    let mut scanned = 0;
    let mut cursor = Some(chain.tip().tip);
    while let Some(hash) = cursor {
        if scanned >= WALLET_ACTIVITY_HEARTBEATS {
            break;
        }
        let Some(block) = chain.block(&hash) else {
            break;
        };
        let (height, time) = (Some(block.header.height), Some(block.header.timestamp));
        if let Some(reward) = rewards.get(&hex(&hash)) {
            if reward.heartbeat > 0 {
                push_wallet_activity(
                    &mut confirmed,
                    &mut confirmed_counts,
                    WalletActivityItem {
                        kind: "heartbeat_reward",
                        id: hex(&hash),
                        heartbeat_height: height,
                        timestamp: time,
                        amount: reward.heartbeat,
                        counterparty: None,
                    },
                );
            }
            if reward.ledger > 0 {
                push_wallet_activity(
                    &mut confirmed,
                    &mut confirmed_counts,
                    WalletActivityItem {
                        kind: "ledger_reward",
                        id: hex(&hash),
                        heartbeat_height: height,
                        timestamp: time,
                        amount: reward.ledger,
                        counterparty: None,
                    },
                );
            }
        }
        for transaction in block
            .sidecar
            .iter()
            .flat_map(|sidecar| sidecar.transactions.iter())
        {
            if let Some(item) = transaction_activity(&owners, transaction, height, time) {
                push_wallet_activity(&mut confirmed, &mut confirmed_counts, item);
            }
        }
        for ticket in &block.tickets {
            if let Some(item) = ticket_activity(&owners, ticket, height, time) {
                push_wallet_activity(&mut confirmed, &mut confirmed_counts, item);
            }
        }
        scanned += 1;
        cursor = block.header.parent;
    }
    Ok(Json(WalletActivityView {
        public_key: hex(&public_key),
        scanned_heartbeats: scanned,
        utxos,
        pending,
        confirmed,
    }))
}

async fn block_detail(
    State(state): State<ConsensusHttpState>,
    Path(block_hash): Path<String>,
) -> std::result::Result<Response, ApiErrorResponse> {
    let block_hash = decode_hex_32(&block_hash, "block hash")
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    // An old block's parent state can be a long replay away, which must not happen under the node
    // lock on a public route. Blocks that are not projected use a state at hand or none: the
    // modal then shows what the block itself holds, without input values and fees.
    let store = state.ui_store.clone();
    let projected_hash = hex(&block_hash);
    let projected = tokio::task::spawn_blocking(move || store.block_detail(&projected_hash))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|detail| detail)
        .unwrap_or_else(|error| {
            eprintln!("reading block detail failed: {error:#}");
            None
        });
    if let Some(detail) = projected {
        return Ok((
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            detail,
        )
            .into_response());
    }
    let node = state.node.lock().await;
    let chain = node.chain();
    let block = chain
        .block(&block_hash)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "unknown Heartbeat block"))?;
    let parent_state = block
        .header
        .parent
        .and_then(|parent| chain.kept_state(&parent));
    let detail = block_detail_view(block, parent_state)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(detail).into_response())
}

pub fn block_detail_view(
    block: &HeartbeatBlock,
    parent_state: Option<&ConsensusState>,
) -> std::result::Result<BlockDetailView, ConsensusError> {
    let payouts = parent_state
        .map(|parent| block_payouts(parent, block))
        .transpose()?
        .unwrap_or_default();
    let heartbeat_total = payouts.heartbeat_total()?;
    let ledger_total = payouts
        .subsidy
        .checked_add(payouts.winner_fees)
        .ok_or(ConsensusError::Arithmetic)?;
    let rewards = BlockRewardsView {
        heartbeat: HeartbeatRewardView {
            total: heartbeat_total,
            ledger_transaction_fees: payouts.heartbeat_fees,
            ticket_inclusion_fees: payouts.inclusion_fees,
            expenditure: payouts.expenditure,
        },
        ledger: block.sidecar.as_ref().map(|_| LedgerRewardView {
            total: ledger_total,
            subsidy: payouts.subsidy,
            ledger_transaction_fees: payouts.winner_fees,
        }),
    };
    let ticket_state = parent_state
        .map(|parent| ticket_input_state(parent, block))
        .transpose()?;
    let tickets = block
        .tickets
        .iter()
        .map(|ticket| {
            let parts = ticket_view_parts(ticket, ticket_state.as_ref());
            TicketPurchaseView {
                id: parts.id,
                version: ticket.version,
                budget: ticket.budget,
                duration: ticket.duration,
                lifecycle_heartbeats: ticket_lifecycle_length(ticket.duration),
                fee: parts.fee,
                fee_recipient: hex(&address_from_public_key(&block.header.miner_public_key)),
                mining_public_key: parts.mining_public_key,
                lottery_seed: parts.lottery_seed,
                inputs: parts.inputs,
                change: parts.change,
            }
        })
        .collect();
    let winner = parent_state
        .map(|parent| sidecar_winner(parent, block))
        .transpose()?
        .flatten();
    let mut fees_before = 0_u64;
    let ledger = block.sidecar.as_ref().map(|sidecar| LedgerBlockView {
        candidate_hash: hex(&sidecar.header.hash()),
        candidate_signature: (!sidecar.header.signature.is_empty())
            .then(|| hex(&sidecar.header.signature)),
        deadline_fallback: sidecar.transactions.is_empty() && sidecar.header.signature.is_empty(),
        winning_ticket_id: hex(&sidecar.header.ticket_id),
        lottery_heartbeat: hex(&sidecar.header.lottery_heartbeat),
        lottery_height: sidecar.header.lottery_height,
        slot: sidecar.header.slot,
        transactions: sidecar
            .transactions
            .iter()
            .map(|transaction| {
                let mut view = transaction_view(transaction, parent_state);
                view.fee_split = view.fee.map(|fee| {
                    let split = block_fee_split(
                        fees_before,
                        fee,
                        block.header.miner_public_key,
                        winner.as_ref().map(|winner| winner.mining_public_key),
                    );
                    fees_before = fees_before.saturating_add(fee);
                    split
                });
                view
            })
            .collect(),
    });
    Ok(BlockDetailView {
        block_hash: hex(&block.hash()),
        height: block.header.height,
        rewards,
        tickets,
        ledger,
    })
}

async fn mempool(State(state): State<ConsensusHttpState>) -> ApiResult<MempoolView> {
    let node = state.node.lock().await;
    let tip = node.chain().tip();
    let (transactions, tickets) = node.mempool_preview(MEMPOOL_PREVIEW_ITEMS);
    Ok(Json(MempoolView {
        ledger_transactions: transactions
            .into_iter()
            .map(|transaction| {
                let view = transaction_view(transaction, Some(tip));
                MempoolTransactionSummary {
                    id: view.id,
                    version: view.version,
                    input_count: view.inputs.len(),
                    output_count: view.outputs.len(),
                    total_output: view.total_output,
                    fee: view.fee,
                    fee_split: view.fee_split,
                }
            })
            .collect(),
        mining_tickets: tickets
            .into_iter()
            .map(|ticket| MempoolTicketSummary {
                id: hex(&ticket.id()),
                version: ticket.version,
                budget: ticket.budget,
                duration: ticket.duration,
                lifecycle_heartbeats: ticket_lifecycle_length(ticket.duration),
                fee: ticket_fee(&tip.utxos, ticket),
                mining_public_key: hex(&ticket.mining_public_key),
            })
            .collect(),
    }))
}

async fn mempool_item(
    State(state): State<ConsensusHttpState>,
    Path(id): Path<String>,
) -> ApiResult<MempoolItemView> {
    let id = decode_hex_32(&id, "mempool item id")
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let node = state.node.lock().await;
    let tip = node.chain().tip();
    if let Some(transaction) = node.pending_transaction(&id) {
        return Ok(Json(MempoolItemView::Transaction(transaction_view(
            transaction,
            Some(tip),
        ))));
    }
    let ticket = node
        .pending_ticket(&id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "not in the mempool"))?;
    let parts = ticket_view_parts(ticket, Some(tip));
    Ok(Json(MempoolItemView::Ticket(MempoolTicketView {
        id: parts.id,
        version: ticket.version,
        budget: ticket.budget,
        duration: ticket.duration,
        lifecycle_heartbeats: ticket_lifecycle_length(ticket.duration),
        fee: parts.fee,
        mining_public_key: parts.mining_public_key,
        lottery_seed: parts.lottery_seed,
        inputs: parts.inputs,
        change: parts.change,
    })))
}

fn transaction_view(
    transaction: &Transaction,
    parent_state: Option<&crate::consensus::ConsensusState>,
) -> LedgerTransactionView {
    let total_output = transaction
        .outputs
        .iter()
        .fold(0_u64, |total, output| total.saturating_add(output.value));
    let total_input = parent_state.and_then(|state| {
        transaction.inputs.iter().try_fold(0_u64, |total, input| {
            state
                .utxos
                .get(&input.previous_output)
                .and_then(|output| total.checked_add(output.value))
        })
    });
    let fee = total_input.and_then(|input| input.checked_sub(total_output));
    LedgerTransactionView {
        id: hex(&transaction.id()),
        version: transaction.version,
        inputs: transaction
            .inputs
            .iter()
            .map(|input| {
                input_view(
                    &input.previous_output,
                    &input.public_key,
                    &input.signature,
                    parent_state,
                )
            })
            .collect(),
        outputs: transaction.outputs.iter().map(output_view).collect(),
        total_output,
        fee,
        fee_split: fee.map(|fee| {
            let (heartbeat, winner) = split_transaction_fees(fee);
            FeeSplitView {
                heartbeat_miner: FeeShareView {
                    value: heartbeat,
                    owner: None,
                },
                ledger_winner: FeeShareView {
                    value: winner,
                    owner: None,
                },
            }
        }),
    }
}

/// Consensus splits only the block's fee total. Attributing the running total keeps each
/// transaction within one unit of an even split and makes the shares sum to the paid amounts.
fn block_fee_split(
    fees_before: u64,
    fee: u64,
    heartbeat_miner: PublicKey,
    ledger_winner: Option<PublicKey>,
) -> FeeSplitView {
    let (heartbeat_before, _) = split_transaction_fees(fees_before);
    let (heartbeat_after, _) = split_transaction_fees(fees_before.saturating_add(fee));
    let heartbeat = heartbeat_after - heartbeat_before;
    FeeSplitView {
        heartbeat_miner: FeeShareView {
            value: heartbeat,
            owner: Some(hex(&address_from_public_key(&heartbeat_miner))),
        },
        ledger_winner: FeeShareView {
            value: fee - heartbeat,
            owner: ledger_winner.map(|key| hex(&address_from_public_key(&key))),
        },
    }
}

/// Fields shared by the block and mempool ticket views. `state` is what the ticket's inputs resolve
/// against, so the fee comes from the same state and helper consensus uses.
struct TicketViewParts {
    id: String,
    fee: Option<u64>,
    mining_public_key: String,
    lottery_seed: String,
    inputs: Vec<BlockInputView>,
    change: Option<BlockOutputView>,
}

fn ticket_view_parts(
    ticket: &TicketTransaction,
    state: Option<&crate::consensus::ConsensusState>,
) -> TicketViewParts {
    TicketViewParts {
        id: hex(&ticket.id()),
        fee: state.and_then(|state| ticket_fee(&state.utxos, ticket)),
        mining_public_key: hex(&ticket.mining_public_key),
        lottery_seed: hex(&ticket.lottery_seed),
        inputs: ticket
            .inputs
            .iter()
            .map(|input| {
                input_view(
                    &input.previous_output,
                    &input.public_key,
                    &input.signature,
                    state,
                )
            })
            .collect(),
        change: ticket.change.as_ref().map(output_view),
    }
}

fn input_view(
    previous_output: &OutPoint,
    public_key: &PublicKey,
    signature: &[u8],
    state: Option<&crate::consensus::ConsensusState>,
) -> BlockInputView {
    BlockInputView {
        transaction_id: hex(&previous_output.transaction_id),
        output_index: previous_output.output_index,
        public_key: hex(public_key),
        signature: hex(signature),
        value: state
            .and_then(|state| state.utxos.get(previous_output))
            .map(|output| output.value),
    }
}

fn output_view(output: &crate::consensus::Output) -> BlockOutputView {
    BlockOutputView {
        owner: hex(&output.owner),
        value: output.value,
    }
}

async fn submit_signed_transaction(
    State(state): State<ConsensusHttpState>,
    Json(transaction): Json<Transaction>,
) -> ApiResult<SubmissionResponse> {
    let mut node = state.node.lock().await;
    let id = transaction.id();
    let fee = node
        .submit_transaction(transaction)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    if let (Some(relay), Some(transaction)) = (&state.relay, node.pending_transaction(&id).cloned())
    {
        relay.transactions(vec![transaction]);
    }
    Ok(Json(SubmissionResponse { id: hex(&id), fee }))
}

async fn public_wallet_preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn public_wallet_cors(request: Request, next: Next) -> Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    if let Some(value) = origin.as_ref()
        && !allowed_wallet_origin(value)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let preflight = request.method() == Method::OPTIONS;
    let mut response = next.run(request).await;
    if let Some(value) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    if preflight {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type"),
        );
    }
    response
}

fn allowed_wallet_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    origin == "https://wallet.getiuna.org"
        || ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|prefix| origin.starts_with(prefix))
}

async fn iuna_ui_javascript() -> impl IntoResponse {
    javascript(include_str!("../../www/assets/iuna-ui.js"))
}

async fn alpine_javascript() -> impl IntoResponse {
    javascript(include_str!("../../www/assets/alpine.min.js"))
}

fn javascript(source: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        source,
    )
}

async fn mining_ticket_quote(
    State(state): State<ConsensusHttpState>,
    Query(query): Query<MiningTicketQuoteQuery>,
) -> ApiResult<MiningTicketQuote> {
    if !(1..=MAX_TICKET_DURATION).contains(&query.duration) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("duration must be between 1 and {MAX_TICKET_DURATION} Heartbeats"),
        ));
    }
    let node = state.node.lock().await;
    let chain = node.chain().tip();
    let current_reward = subsidy(chain.ledger_blocks, chain.minted).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot calculate next Ledger reward",
        )
    })?;
    let current_minimum_budget =
        minimum_ticket_budget(query.duration, current_reward).map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot calculate minimum ticket budget",
            )
        })?;
    let after_ledger_blocks = chain.ledger_blocks.checked_add(1).ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Ledger block count overflow",
        )
    })?;
    let after_minted = chain
        .minted
        .checked_add(current_reward)
        .ok_or_else(|| api_error(StatusCode::INTERNAL_SERVER_ERROR, "minted supply overflow"))?;
    let reward_after_same_heartbeat = subsidy(after_ledger_blocks, after_minted).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot calculate post-Ledger reward",
        )
    })?;
    let minimum_budget_after_same_heartbeat_ledger =
        minimum_ticket_budget(query.duration, reward_after_same_heartbeat).map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot calculate post-Ledger ticket budget",
            )
        })?;
    let recommended_budget = current_minimum_budget.max(minimum_budget_after_same_heartbeat_ledger);
    Ok(Json(MiningTicketQuote {
        duration: query.duration,
        current_next_ledger_reward: current_reward,
        current_minimum_budget,
        minimum_budget_after_same_heartbeat_ledger,
        recommended_budget,
        heartbeat_target_seconds: HEARTBEAT_TARGET_INTERVAL_SECS,
        ledger_target_heartbeats: LEDGER_TARGET_INTERVAL_HEARTBEATS,
    }))
}

async fn index() -> impl IntoResponse {
    (
        [
            (
                "content-security-policy",
                "default-src 'self'; script-src 'self' 'unsafe-eval'; style-src 'unsafe-inline'; connect-src 'self' https://getiuna.org; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
            ),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
        ],
        Html(INDEX_HTML),
    )
}

async fn status(State(state): State<ConsensusHttpState>) -> Json<OperatorStatus> {
    let mut node = state.node.lock().await;
    let chain = node.status();
    let reserved_inputs = node.reserved_inputs();
    let wallet = state.wallet.read().expect("wallet state lock poisoned");
    let wallet_addresses = wallet.unlocked.as_ref().map_or_else(
        || {
            wallet
                .public_key
                .map(|key| address_from_public_key(&key))
                .into_iter()
                .collect::<BTreeSet<_>>()
        },
        |wallet| wallet.owned_addresses(),
    );
    let (wallet_balance, wallet_spendable_balance, wallet_utxos, wallet_spendable_utxos) =
        wallet.public_key.map_or((0, 0, 0, 0), |_| {
            node.chain()
                .tip()
                .utxos
                .iter()
                .filter(|(_, output)| wallet_addresses.contains(&output.owner))
                .fold(
                    (0, 0, 0, 0),
                    |(balance, spendable, outputs, spendable_outputs), (point, output)| {
                        let reserved = reserved_inputs.contains(point);
                        (
                            balance + output.value,
                            spendable + if reserved { 0 } else { output.value },
                            outputs + 1,
                            spendable_outputs + usize::from(!reserved),
                        )
                    },
                )
        });
    let ledger_ticket_power = local_ledger_ticket_power(node.chain().tip(), wallet.public_key);
    let wallet_receive_address = wallet.unlocked.as_ref().and_then(|wallet| {
        (0..crate::app::wallet_commands::WALLET_ADDRESS_SCAN_LIMIT)
            .map(|index| wallet.receive_address(index))
            .find(|address| !node.address_used(address))
    });
    Json(OperatorStatus {
        app_version: env!("CARGO_PKG_VERSION"),
        network: NETWORK_ID,
        node_instance_id: state.node_instance_id.clone(),
        heartbeat_height: chain.height,
        tip_hash: hex(&chain.tip),
        ledger_blocks: chain.ledger_blocks,
        minted: chain.minted,
        tracked_mining_tickets: chain.tracked_tickets,
        utxos: chain.utxos,
        pending_ledger_transactions: node.pending_transaction_count(),
        pending_mining_tickets: node.pending_ticket_count(),
        wallet_public_key: wallet.public_key.map(|key| hex(&key)),
        wallet_receive_address: wallet_receive_address.map(|address| hex(&address)),
        wallet_balance,
        wallet_spendable_balance,
        wallet_utxos,
        wallet_spendable_utxos,
        wallet_unlocked: wallet.unlocked.is_some(),
        management_enabled: state.trusted_management || state.operator_token_hash.is_some(),
        management_requires_token: state.operator_token_hash.is_some(),
        heartbeat_mining_enabled: state.heartbeat_mining.load(Ordering::Relaxed),
        heartbeat_mining_workers: state.heartbeat_mining_workers.load(Ordering::Relaxed),
        heartbeat_mining_max_workers: max_mining_workers(),
        heartbeat_hashes: state.heartbeat_hashes.load(Ordering::Relaxed),
        ledger_ticket_power,
        ledger_mining_enabled: state.ledger_mining.load(Ordering::Relaxed),
        ledger_last_ticket_height: decode_ledger_ticket_height(
            state.ledger_last_ticket_height.load(Ordering::Relaxed),
        ),
        connected_peers: state.peers.connected().len(),
        target: hex(&node.chain().tip().tip_target),
        difficulty: difficulty_label(node.chain().tip().tip_target),
        pow_limit: hex(&MAX_TARGET),
    })
}

fn local_ledger_ticket_power(state: &ConsensusState, public_key: Option<PublicKey>) -> u64 {
    let Some(public_key) = public_key else {
        return 0;
    };
    state
        .tickets
        .values()
        .filter(|ticket| ticket.mining_public_key == public_key)
        .filter_map(|ticket| ticket.weight_at(state.height))
        .sum()
}

async fn peers(State(state): State<ConsensusHttpState>) -> Json<Vec<ConnectedPeer>> {
    Json(state.peers.connected())
}

async fn peer_targets(State(state): State<ConsensusHttpState>) -> Json<Vec<PeerTarget>> {
    Json(state.peers.targets())
}

#[derive(Debug, Deserialize)]
pub struct PeerChangeRequest {
    /// `add` or `remove`.
    pub action: String,
    /// `IP:port` or `host:port`.
    pub address: String,
}

async fn change_peer(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<PeerChangeRequest>,
) -> ApiResult<Vec<PeerTarget>> {
    let address: PeerAddress = request
        .address
        .parse()
        .map_err(|error: anyhow::Error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let result = match request.action.as_str() {
        "add" => state.peers.add_target(address),
        "remove" => state.peers.remove_target(address),
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "peer action must be add or remove",
            ));
        }
    };
    result.map_err(|error| api_error(StatusCode::CONFLICT, format!("{error:#}")))?;
    Ok(Json(state.peers.targets()))
}

async fn set_heartbeat_mining(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<HeartbeatMiningRequest>,
) -> ApiResult<HeartbeatMiningResponse> {
    let maximum = max_mining_workers();
    if !(1..=maximum).contains(&request.workers) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("mining workers must be between 1 and {maximum}"),
        ));
    }
    let bootstrap_ticket_submitted = if request.enabled {
        let wallet = unlocked_wallet(&state)?;
        ensure_bootstrap_ticket(&state, wallet.public_key()).await?
    } else {
        false
    };
    state
        .heartbeat_mining
        .store(request.enabled, Ordering::Relaxed);
    state
        .heartbeat_mining_workers
        .store(request.workers, Ordering::Relaxed);
    save_mining_preference(&state);
    Ok(Json(HeartbeatMiningResponse {
        enabled: request.enabled,
        workers: request.workers,
        bootstrap_ticket_submitted,
    }))
}

async fn set_ledger_mining(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<LedgerMiningRequest>,
) -> ApiResult<LedgerMiningResponse> {
    if request.enabled {
        let _ = unlocked_wallet(&state)?;
    }
    state
        .ledger_mining
        .store(request.enabled, Ordering::Relaxed);
    if request.enabled {
        state.ledger_last_ticket_height.store(0, Ordering::Relaxed);
    }
    save_mining_preference(&state);
    Ok(Json(LedgerMiningResponse {
        enabled: request.enabled,
    }))
}

fn save_mining_preference(state: &ConsensusHttpState) {
    let preference_path = consensus_local_state::mining_path(
        &state
            .wallet
            .read()
            .expect("wallet state lock poisoned")
            .path,
    );
    if let Err(error) = consensus_local_state::save_mining_from(&preference_path, || {
        let last_height = state.ledger_last_ticket_height.load(Ordering::Relaxed);
        MiningPreference {
            enabled: state.heartbeat_mining.load(Ordering::Relaxed),
            workers: state.heartbeat_mining_workers.load(Ordering::Relaxed),
            ledger_enabled: state.ledger_mining.load(Ordering::Relaxed),
            last_ledger_ticket_height: decode_ledger_ticket_height(last_height),
        }
    }) {
        eprintln!("saving mining preference failed: {error:#}");
    }
}

fn decode_ledger_ticket_height(encoded: u64) -> Option<u64> {
    encoded.checked_sub(1)
}

/// Public RFC 8032 test-vector secret behind `GENESIS_MINER`. It lives in this adapter,
/// not the consensus core, because only the bootstrap below spends it; anyone can, so it
/// is never used on another network.
const GENESIS_SECRET: [u8; 32] = [
    0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
    0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60,
];

async fn ensure_bootstrap_ticket(
    state: &ConsensusHttpState,
    mining_public_key: [u8; 32],
) -> std::result::Result<bool, ApiErrorResponse> {
    let mut node = state.node.lock().await;
    let ticket = submit_bootstrap_ticket(&mut node, mining_public_key)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    let submitted = ticket.is_some();
    if let (Some(relay), Some(ticket)) = (&state.relay, ticket) {
        relay.tickets(vec![ticket]);
    }
    Ok(submitted)
}

/// Spends the public genesis output into the first Ledger ticket for `mining_public_key`, so a
/// fresh candidate chain can start its Ledger without anyone holding coins yet. It does nothing
/// once any ticket exists, is pending or the genesis output has moved, so the HTTP toggle and
/// the miner loop (which also covers mining resumed from a saved preference) can both call it.
pub fn submit_bootstrap_ticket<R: ChainRepository>(
    node: &mut Node<R>,
    mining_public_key: PublicKey,
) -> anyhow::Result<Option<TicketTransaction>> {
    if NETWORK_ID != "iuna-mainnet-candidate" {
        return Ok(None);
    }
    let chain = node.chain().tip();
    let genesis_owner = address_from_public_key(&GENESIS_MINER);
    if chain.minted != GENESIS_REWARD
        || !chain.tickets.is_empty()
        || node.pending_ticket_count() != 0
        || !chain
            .utxos
            .values()
            .any(|output| output.owner == genesis_owner)
    {
        return Ok(None);
    }
    let genesis_wallet = Wallet::from_seed(GENESIS_SECRET);
    let lottery_seed = tagged_hash(b"IUNA-BOOTSTRAP-TICKET-v1", &mining_public_key);
    let receipt = genesis_wallet.submit_ticket(
        node,
        TicketParameters {
            budget: GENESIS_REWARD - 1_000_000,
            duration: LEDGER_TARGET_INTERVAL_HEARTBEATS,
            fee: 1_000_000,
            mining_public_key,
            lottery_seed,
        },
    )?;
    Ok(node.pending_ticket(&receipt.id).cloned())
}

async fn access_wallet(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<WalletAccessRequest>,
) -> Result<impl IntoResponse, ApiErrorResponse> {
    // New passwords are checked by the wallet store; unlocking accepts any existing password.
    if request.password.is_empty()
        || request.password.len() > consensus_wallet_store::MAX_PASSWORD_BYTES
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "wallet password must not be empty or longer than 1024 bytes",
        ));
    }
    let path = state
        .wallet
        .read()
        .expect("wallet state lock poisoned")
        .path
        .clone();
    let wallet_exists = path.exists();
    let action =
        request
            .action
            .as_deref()
            .unwrap_or(if wallet_exists { "unlock" } else { "create" });
    if wallet_exists && !matches!(action, "unlock" | "backup" | "change_password") {
        return Err(api_error(
            StatusCode::CONFLICT,
            "a wallet already exists on this node",
        ));
    }
    if !wallet_exists && matches!(action, "unlock" | "backup" | "change_password") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "there is no wallet on this node yet",
        ));
    }
    if !matches!(
        action,
        "create" | "restore" | "unlock" | "backup" | "change_password"
    ) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "wallet action must be create, restore, unlock, backup, or change_password",
        ));
    }
    let password = request.password;
    let recovery_phrase = request.recovery_phrase;
    let new_password = request.new_password;
    let action = action.to_owned();
    let (wallet, generated_phrase) = tokio::task::spawn_blocking(move || match action.as_str() {
        "create" => consensus_wallet_store::create_recoverable(&path, &password)
            .map(|(wallet, phrase)| (wallet, Some(phrase))),
        "restore" => {
            let phrase = recovery_phrase.as_deref().unwrap_or("");
            consensus_wallet_store::restore(&path, &password, phrase).map(|wallet| (wallet, None))
        }
        "unlock" => consensus_wallet_store::load(&path, &password).map(|wallet| (wallet, None)),
        "change_password" => consensus_wallet_store::change_password(
            &path,
            &password,
            new_password.as_deref().unwrap_or(""),
        )
        .map(|wallet| (wallet, None)),
        "backup" => {
            let phrase = consensus_wallet_store::recovery_phrase(&path, &password)?;
            consensus_wallet_store::load(&path, &password).map(|wallet| (wallet, Some(phrase)))
        }
        _ => unreachable!(),
    })
    .await
    .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "wallet task failed"))?
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let wallet = Arc::new(wallet);
    let public_key = wallet.public_key();
    let mut wallet_state = state.wallet.write().expect("wallet state lock poisoned");
    wallet_state.public_key = Some(public_key);
    wallet_state.unlocked = Some(wallet);
    // Mining that was left on before a restart resumes as soon as the wallet is unlocked.
    if let Ok(Some(preference)) =
        consensus_local_state::load_mining(&consensus_local_state::mining_path(&wallet_state.path))
    {
        if preference.enabled {
            state.heartbeat_mining_workers.store(
                preference.workers.clamp(1, max_mining_workers()),
                Ordering::Relaxed,
            );
            state.heartbeat_mining.store(true, Ordering::Relaxed);
        }
        state
            .ledger_mining
            .store(preference.ledger_enabled, Ordering::Relaxed);
        state.ledger_last_ticket_height.store(
            preference
                .last_ledger_ticket_height
                .and_then(|height| height.checked_add(1))
                .unwrap_or(0),
            Ordering::Relaxed,
        );
    }
    drop(wallet_state);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid cache header"),
    );
    Ok((
        headers,
        Json(WalletAccessResponse {
            created: !wallet_exists,
            public_key: hex(&public_key),
            recovery_phrase: generated_phrase,
        }),
    ))
}

async fn submit_transaction(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<LedgerTransactionRequest>,
) -> ApiResult<SubmissionResponse> {
    let wallet = unlocked_wallet(&state)?;
    let recipient = decode_hex_32(&request.recipient, "recipient")
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let mut node = state.node.lock().await;
    let receipt = wallet
        .submit_transaction(&mut node, recipient, request.amount, request.fee)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    if let (Some(relay), Some(transaction)) =
        (&state.relay, node.pending_transaction(&receipt.id).cloned())
    {
        relay.transactions(vec![transaction]);
    }
    Ok(Json(SubmissionResponse {
        id: hex(&receipt.id),
        fee: receipt.fee,
    }))
}

async fn submit_ticket(
    State(state): State<ConsensusHttpState>,
    Json(request): Json<MiningTicketRequest>,
) -> ApiResult<SubmissionResponse> {
    let wallet = unlocked_wallet(&state)?;
    let mut lottery_seed = [0; 32];
    getrandom(&mut lottery_seed).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secure randomness is unavailable",
        )
    })?;
    let mut node = state.node.lock().await;
    let receipt = wallet
        .submit_ticket(
            &mut node,
            TicketParameters {
                budget: request.budget,
                duration: request.duration,
                fee: request.fee,
                mining_public_key: wallet.public_key(),
                lottery_seed,
            },
        )
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    if let (Some(relay), Some(ticket)) = (&state.relay, node.pending_ticket(&receipt.id).cloned()) {
        relay.tickets(vec![ticket]);
    }
    Ok(Json(SubmissionResponse {
        id: hex(&receipt.id),
        fee: receipt.fee,
    }))
}

async fn mine_heartbeat(
    State(state): State<ConsensusHttpState>,
) -> ApiResult<MinedHeartbeatResponse> {
    let wallet = unlocked_wallet(&state)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "system clock is invalid"))?
        .as_secs();
    let prepared = state
        .node
        .lock()
        .await
        .prepare_next()
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    let timestamp = prepared
        .timestamp(timestamp)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    let mining_public_key = wallet.public_key();
    let workers = state.heartbeat_mining_workers.load(Ordering::Relaxed);
    let block = tokio::task::spawn_blocking(move || {
        prepared.mine_with_workers(timestamp, mining_public_key, workers)
    })
    .await
    .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "Hash mining task failed"))?
    .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    let mut node = state.node.lock().await;
    let activated = node
        .submit_block(block.clone(), timestamp)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    if !activated {
        return Err(api_error(
            StatusCode::CONFLICT,
            "a competing Heartbeat reached the node first",
        ));
    }
    drop(node);
    if let Some(relay) = &state.relay {
        relay.heartbeat(&block);
    }
    Ok(Json(MinedHeartbeatResponse {
        hash: hex(&block.hash()),
        height: block.header.height,
    }))
}

pub fn operator_token_hash(token: &str) -> Result<[u8; 32]> {
    if !(32..=1_024).contains(&token.len()) {
        bail!("IUNA_OPERATOR_TOKEN must contain between 32 and 1024 bytes");
    }
    let mut hasher = Sha256::new();
    hasher.update(b"IUNA-OPERATOR-TOKEN-v1");
    hasher.update(token.as_bytes());
    Ok(hasher.finalize().into())
}

fn authorize(
    state: &ConsensusHttpState,
    headers: &HeaderMap,
) -> std::result::Result<(), ApiErrorResponse> {
    if state.trusted_management && state.operator_token_hash.is_none() {
        // Browsers always send Origin on POST. Requiring it to name this node's own
        // loopback Host rejects other localhost ports and DNS-rebound pages.
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        return match (origin, host) {
            (Some(origin), Some(host)) if is_same_local_origin(origin, host) => Ok(()),
            _ => Err(api_error(StatusCode::FORBIDDEN, "untrusted request origin")),
        };
    }
    let expected = state.operator_token_hash.ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "management API is disabled",
        )
    })?;
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| operator_token_hash(token).ok())
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "authentication required"))?;
    if !constant_time_eq(&expected, &supplied) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication required",
        ));
    }
    Ok(())
}

fn is_local_origin(origin: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| origin.starts_with(prefix))
}

fn is_same_local_origin(origin: &str, host: &str) -> bool {
    is_local_origin(origin)
        && origin
            .strip_prefix("http://")
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

async fn require_operator_auth(
    State(state): State<ConsensusHttpState>,
    request: Request,
    next: Next,
) -> Response {
    match authorize(&state, request.headers()) {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

fn unlocked_wallet(
    state: &ConsensusHttpState,
) -> std::result::Result<Arc<Wallet>, ApiErrorResponse> {
    state
        .wallet
        .read()
        .expect("wallet state lock poisoned")
        .unlocked
        .clone()
        .ok_or_else(|| api_error(StatusCode::LOCKED, "wallet is locked"))
}

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
}

fn difficulty_label(target: [u8; 32]) -> String {
    let maximum = BigUint::from_bytes_be(&MAX_TARGET);
    let target = BigUint::from_bytes_be(&target);
    let ratio = (maximum + 1_u8).to_f64().unwrap_or(0.0) / (target + 1_u8).to_f64().unwrap_or(1.0);
    if ratio >= 100.0 {
        format!("{ratio:.0}×")
    } else {
        format!("{ratio:.2}×")
    }
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn decode_hex_32(value: &str, label: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        bail!("{label} must be a 32-byte hexadecimal value");
    }
    let mut decoded = [0; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        decoded[index] = (hex_nibble(pair[0], label)? << 4) | hex_nibble(pair[1], label)?;
    }
    Ok(decoded)
}

fn hex_nibble(value: u8, label: &str) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("{label} contains invalid hexadecimal data"),
    }
}

#[derive(Debug, Default, Deserialize)]
struct HeartbeatsQuery {
    before_height: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct MetricsQuery {
    limit: Option<usize>,
}

async fn metrics(
    State(state): State<ConsensusHttpState>,
    Query(query): Query<MetricsQuery>,
) -> Result<Json<Vec<UiHeartbeat>>, (StatusCode, String)> {
    let store = state.ui_store.clone();
    let limit = query.limit.map(|limit| limit.clamp(1, 1_000));
    match tokio::task::spawn_blocking(move || store.metrics(limit)).await {
        Ok(Ok(rows)) => Ok(Json(rows)),
        Ok(Err(error)) => Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("metrics query task failed: {error}"),
        )),
    }
}

async fn heartbeats(
    State(state): State<ConsensusHttpState>,
    Query(query): Query<HeartbeatsQuery>,
) -> Result<Json<Vec<UiHeartbeat>>, (StatusCode, String)> {
    let store = state.ui_store.clone();
    let limit = query.limit.unwrap_or(200).clamp(1, 200);
    let before_height = query.before_height;
    let heartbeats =
        match tokio::task::spawn_blocking(move || store.heartbeats_before(before_height, limit))
            .await
        {
            Ok(Ok(heartbeats)) => heartbeats,
            Ok(Err(error)) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
            Err(error) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("screen-data query task failed: {error}"),
                ));
            }
        };
    Ok(Json(heartbeats))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::Body, http::Request};
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        adapters::consensus_store::ConsensusChainStore,
        app::consensus_node::Node,
        consensus::{PROTOCOL_VERSION, TrackedTicket, pinned_genesis},
    };

    #[test]
    fn local_ticket_power_counts_only_active_tickets_owned_by_this_wallet() {
        let (_, mut state) = pinned_genesis().unwrap();
        state.height = 10;
        state.tickets.insert(
            [1; 32],
            TrackedTicket {
                id: [1; 32],
                included_height: 4,
                budget: 100,
                duration: 10,
                mining_public_key: [7; 32],
                lottery_seed: [11; 32],
            },
        );
        state.tickets.insert(
            [2; 32],
            TrackedTicket {
                id: [2; 32],
                included_height: 4,
                budget: 200,
                duration: 10,
                mining_public_key: [8; 32],
                lottery_seed: [12; 32],
            },
        );
        state.tickets.insert(
            [3; 32],
            TrackedTicket {
                id: [3; 32],
                included_height: 10,
                budget: 1_000,
                duration: 10,
                mining_public_key: [7; 32],
                lottery_seed: [13; 32],
            },
        );

        assert_eq!(local_ledger_ticket_power(&state, Some([7; 32])), 10);
        assert_eq!(local_ledger_ticket_power(&state, Some([8; 32])), 20);
        assert_eq!(local_ledger_ticket_power(&state, None), 0);
    }

    #[test]
    fn mining_activity_does_not_crowd_transfers_out_of_wallet_history() {
        let mut items = Vec::new();
        let mut counts = [0; 4];
        for height in 0..=WALLET_ACTIVITY_ITEMS_PER_TYPE {
            push_wallet_activity(
                &mut items,
                &mut counts,
                WalletActivityItem {
                    kind: "heartbeat_reward",
                    id: height.to_string(),
                    heartbeat_height: Some(height as u64),
                    timestamp: Some(height as u64),
                    amount: 1,
                    counterparty: None,
                },
            );
        }
        push_wallet_activity(
            &mut items,
            &mut counts,
            WalletActivityItem {
                kind: "sent",
                id: "sent-transaction".to_owned(),
                heartbeat_height: Some(1),
                timestamp: Some(1),
                amount: 1,
                counterparty: Some(hex(&[7; 32])),
            },
        );

        assert_eq!(
            items
                .iter()
                .filter(|item| item.kind == "heartbeat_reward")
                .count(),
            WALLET_ACTIVITY_ITEMS_PER_TYPE
        );
        assert!(items.iter().any(|item| item.kind == "sent"));
    }

    #[tokio::test]
    async fn status_uses_public_heartbeat_and_ledger_vocabulary() {
        let directory = tempfile::tempdir().unwrap();
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        let ui_store = ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap();
        ui_store.project(node.chain()).unwrap();
        let state = ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store,
            wallet: wallet_state(&directory, None),
            operator_token_hash: None,
            trusted_management: false,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        };
        let response = router(state.clone())
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("content-security-policy"));
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        // Alpine needs eval; the page has no inline scripts, so inline script stays blocked.
        let script_src = policy
            .split(';')
            .find(|directive| directive.trim_start().starts_with("script-src"))
            .unwrap();
        assert!(script_src.contains("'unsafe-eval'"));
        assert!(!script_src.contains("'unsafe-inline'"));
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .contains("Heartbeat chain")
        );

        let response = router(state.clone())
            .oneshot(
                Request::get("/assets/iuna-ui.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .contains("setHeartbeatMining")
        );

        let response = router(state.clone())
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["network"], NETWORK_ID);
        assert_eq!(json["heartbeat_height"], 0);
        assert_eq!(json["ledger_blocks"], 0);
        assert_eq!(json["wallet_unlocked"], false);
        assert_eq!(json["wallet_balance"], 0);
        assert_eq!(json["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["wallet_utxos"], 0);
        assert_eq!(json["wallet_spendable_utxos"], 0);
        assert_eq!(json["wallet_spendable_balance"], 0);
        assert_eq!(json["heartbeat_hashes"], 0);
        assert_eq!(json["ledger_ticket_power"], 0);
        assert!(json.get("l1_blocks").is_none());
        assert!(json.get("l2_blocks").is_none());

        let response = router(state.clone())
            .oneshot(Request::get("/api/heartbeats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json[0]["height"], 0);
        assert_eq!(json[0]["has_ledger_block"], false);

        let response = router(state.clone())
            .oneshot(
                Request::get("/api/heartbeats?limit=1&before_height=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json, serde_json::json!([]));

        let response = router(state.clone())
            .oneshot(
                Request::get("/api/metrics?limit=100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json[0]["height"], 0);
        assert!(json[0]["heartbeat_bytes"].as_u64().unwrap() > 0);
        assert!(json[0].get("chain_bytes").is_some());
        assert!(json[0].get("minted").is_some());
        assert!(json[0].get("utxos").is_some());

        let response = router(state.clone())
            .oneshot(
                Request::get("/api/mining-ticket-quote?duration=60")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let quote: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(quote["duration"], 60);
        assert_eq!(quote["heartbeat_target_seconds"], 60);
        assert_eq!(quote["ledger_target_heartbeats"], 10);
        assert_eq!(
            quote["current_minimum_budget"],
            minimum_ticket_budget(60, quote["current_next_ledger_reward"].as_u64().unwrap())
                .unwrap()
        );
        assert!(
            quote["recommended_budget"].as_u64().unwrap()
                >= quote["current_minimum_budget"].as_u64().unwrap()
        );

        let response = router(state)
            .oneshot(
                Request::get("/api/mining-ticket-quote?duration=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn management_routes_require_authentication_and_an_unlocked_wallet() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let directory = tempfile::tempdir().unwrap();
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        let state = ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store: ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap(),
            wallet: wallet_state(&directory, None),
            operator_token_hash: Some(operator_token_hash(TOKEN).unwrap()),
            trusted_management: false,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        };
        let body = serde_json::json!({
            "recipient": hex(&[91; 32]),
            "amount": 1,
            "fee": 0
        })
        .to_string();

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/ledger-transactions",
                "not-json".to_string(),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mut disabled = state.clone();
        disabled.operator_token_hash = None;
        let response = router(disabled)
            .oneshot(json_request(
                "/api/ledger-transactions",
                body.clone(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let oversized = serde_json::json!({
            "recipient": "a".repeat(5_000),
            "amount": 1,
            "fee": 0
        })
        .to_string();
        let response = router(state.clone())
            .oneshot(json_request(
                "/api/ledger-transactions",
                oversized,
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let response = router(state)
            .oneshot(json_request("/api/ledger-transactions", body, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::LOCKED);
    }

    #[tokio::test]
    async fn trusted_local_setup_creates_and_unlocks_an_encrypted_wallet() {
        let directory = tempfile::tempdir().unwrap();
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        let state = ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store: ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap(),
            wallet: wallet_state(&directory, None),
            operator_token_hash: None,
            trusted_management: true,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        };
        let response = router(state.clone())
            .oneshot(local_json_request(
                "/api/wallet",
                serde_json::json!({ "password": "correct horse", "action": "create" }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let wallet: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wallet["created"], true);
        assert_eq!(wallet["public_key"].as_str().unwrap().len(), 64);
        assert_eq!(
            wallet["recovery_phrase"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .count(),
            24
        );
        assert!(directory.path().join("wallet.json").exists());

        let response = router(state.clone())
            .oneshot(local_json_request(
                "/api/wallet",
                serde_json::json!({ "password": "correct horse", "action": "backup" }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let backup: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            backup["recovery_phrase"].as_str().unwrap(),
            wallet["recovery_phrase"].as_str().unwrap()
        );

        let response = router(state.clone())
            .oneshot(local_json_request(
                "/api/wallet",
                serde_json::json!({
                    "password": "correct horse",
                    "action": "change_password",
                    "new_password": "battery staple",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router(state.clone())
            .oneshot(local_json_request(
                "/api/wallet",
                serde_json::json!({ "password": "correct horse", "action": "backup" }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "old password no longer works"
        );

        let response = router(state)
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["wallet_unlocked"], true);
        assert_eq!(status["wallet_receive_address"].as_str().unwrap().len(), 64);
        assert_ne!(
            status["wallet_receive_address"],
            status["wallet_public_key"]
        );
        assert_eq!(status["management_enabled"], true);
        assert_eq!(status["management_requires_token"], false);
    }

    #[tokio::test]
    async fn trusted_local_setup_restores_a_recovery_phrase() {
        let directory = tempfile::tempdir().unwrap();
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        let state = ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store: ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap(),
            wallet: wallet_state(&directory, None),
            operator_token_hash: None,
            trusted_management: true,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        };
        let phrase = format!("{} art", vec!["abandon"; 23].join(" "));
        let response = router(state)
            .oneshot(local_json_request(
                "/api/wallet",
                serde_json::json!({
                    "password": "restored password",
                    "action": "restore",
                    "recovery_phrase": phrase,
                })
                .to_string(),
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let wallet: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            wallet["public_key"],
            "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29"
        );
        assert!(wallet.get("recovery_phrase").is_none());
    }

    #[tokio::test]
    async fn trusted_local_management_rejects_cross_origin_browser_requests() {
        let directory = tempfile::tempdir().unwrap();
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        let state = ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store: ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap(),
            wallet: wallet_state(&directory, None),
            operator_token_hash: None,
            trusted_management: true,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        };
        for (origin, host) in [
            (Some("https://malicious.example"), "127.0.0.1:8080"),
            // Another local web server on a different port.
            (Some("http://localhost:5173"), "127.0.0.1:8080"),
            // DNS rebinding: the page origin matches Host, but Host is not loopback.
            (Some("http://rebind.example:8080"), "rebind.example:8080"),
            // Requests without Origin are not browser requests from this interface.
            (None, "127.0.0.1:8080"),
        ] {
            let mut builder =
                Request::post("/api/hash-mining/heartbeat").header(header::HOST, host);
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            let response = router(state.clone())
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "accepted origin {origin:?} for host {host}"
            );
        }
    }

    #[tokio::test]
    async fn public_wallet_api_views_utxos_and_accepts_signed_transactions() {
        use crate::consensus::{GENESIS_REWARD, Input, OutPoint, Output, PROTOCOL_VERSION};

        const GENESIS_SEED: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        let directory = tempfile::tempdir().unwrap();
        let wallet = Arc::new(Wallet::from_seed(GENESIS_SEED));
        let state = management_state(
            &directory,
            Arc::clone(&wallet),
            "unused-token-value-32-bytes-long",
        );
        let build = |fee| {
            let state = state.clone();
            let wallet = Arc::clone(&wallet);
            async move {
                let node = state.node.lock().await;
                wallet
                    .build_transaction(node.chain().tip(), &node.reserved_inputs(), [7; 32], 1, fee)
                    .unwrap()
            }
        };
        let unavailable = router(state.clone())
            .oneshot(
                Request::get(format!("/api/public/wallet/{}", hex(&wallet.address())))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);

        let unavailable = public_wallet_router(state.clone())
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);

        // Free transactions are below the relay minimum and never reach the mempool.
        let free = build(0).await;
        let request = Request::post("/api/public/transactions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://wallet.getiuna.org")
            .body(Body::from(serde_json::to_vec(&free).unwrap()))
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // A fragmented but consensus-sized transaction is larger in JSON than the previous
        // 64-KiB HTTP cap. It must reach validation rather than fail in Axum's body extractor.
        let fragmented = Transaction {
            version: PROTOCOL_VERSION,
            inputs: (0_u32..150)
                .map(|index| Input {
                    previous_output: OutPoint {
                        transaction_id: [u8::try_from(index % 251).unwrap(); 32],
                        output_index: index,
                    },
                    public_key: [255; 32],
                    signature: vec![255; 64],
                })
                .collect(),
            outputs: vec![Output {
                owner: [7; 32],
                value: 1,
            }],
        };
        let body = serde_json::to_vec(&fragmented).unwrap();
        assert!(body.len() > 64 * 1024);
        assert!(body.len() < MAX_PUBLIC_TRANSACTION_JSON_BYTES);
        let request = Request::post("/api/public/transactions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://wallet.getiuna.org")
            .body(Body::from(body))
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let transaction = build(1).await;
        let request = Request::post("/api/public/transactions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "https://wallet.getiuna.org")
            .body(Body::from(serde_json::to_vec(&transaction).unwrap()))
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://wallet.getiuna.org"
        );

        let request = Request::get(format!("/api/public/wallet/{}", hex(&wallet.address())))
            .body(Body::empty())
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let view: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view["pending_transaction_ids"][0], hex(&transaction.id()));

        let addresses = |addresses: Vec<String>| {
            Request::post("/api/public/wallet-addresses")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "https://wallet.getiuna.org")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "addresses": addresses })).unwrap(),
                ))
                .unwrap()
        };
        let response = public_wallet_router(state.clone())
            .oneshot(addresses(vec![
                hex(&wallet.address()),
                hex(&transaction.outputs[1].owner),
                hex(&wallet.receive_address(0)),
            ]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let batch: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let summaries = batch["addresses"].as_array().unwrap();
        assert_eq!(summaries[0]["address"], hex(&wallet.address()));
        assert_eq!(summaries[0]["balance"], GENESIS_REWARD);
        assert_eq!(summaries[0]["spendable_balance"], 0);
        assert_eq!(summaries[0]["utxo_count"], 1);
        // The pending change address already counts as used, so a second send moves on.
        assert_eq!(summaries[1]["used"], true);
        assert_eq!(summaries[1]["utxo_count"], 0);
        assert_eq!(summaries[2]["used"], false);
        assert_eq!(batch["pending_transaction_ids"][0], hex(&transaction.id()));
        assert_eq!(batch["tip_hash"], view["tip_hash"]);

        for rejected in [
            Vec::new(),
            vec![hex(&wallet.address()); MAX_PUBLIC_WALLET_ADDRESSES + 1],
            vec!["not-an-address".to_owned()],
        ] {
            let response = public_wallet_router(state.clone())
                .oneshot(addresses(rejected))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/public/transactions")
            .header(header::ORIGIN, "https://wallet.getiuna.org")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .body(Body::empty())
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            "GET, POST, OPTIONS"
        );

        let request = Request::get(format!("/api/public/wallet/{}", hex(&wallet.address())))
            .header(header::ORIGIN, "https://wallet.getiuna.org")
            .body(Body::empty())
            .unwrap();
        let response = public_wallet_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(view["address"], hex(&wallet.address()));
        assert_eq!(view["used"], true);
        assert_eq!(view["balance"], GENESIS_REWARD);
        assert_eq!(view["spendable_balance"], 0);
        assert_eq!(view["utxos"][0]["reserved"], true);
        assert_eq!(view["utxos_truncated"], false);
        assert_eq!(view["next_utxo_offset"], serde_json::Value::Null);

        let request = Request::get(format!("/api/public/wallet/{}", hex(&wallet.address())))
            .header(header::ORIGIN, "https://malicious.example")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            public_wallet_router(state)
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn authenticated_management_routes_submit_and_reserve_wallet_objects() {
        use crate::consensus::{GENESIS_REWARD, INITIAL_LEDGER_SUBSIDY, minimum_ticket_budget};

        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        const GENESIS_SEED: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];

        let transaction_directory = tempfile::tempdir().unwrap();
        let wallet = Arc::new(Wallet::from_seed(GENESIS_SEED));
        let transaction_state =
            management_state(&transaction_directory, Arc::clone(&wallet), TOKEN);
        let response = router(transaction_state.clone())
            .oneshot(json_request(
                "/api/ledger-transactions",
                serde_json::json!({
                    "recipient": hex(&[92; 32]),
                    "amount": 10_000_000,
                    "fee": 7
                })
                .to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            transaction_state
                .node
                .lock()
                .await
                .pending_transaction_count(),
            1
        );
        assert_eq!(
            transaction_state.node.lock().await.reserved_inputs().len(),
            1
        );
        let response = router(transaction_state.clone())
            .oneshot(Request::get("/api/mempool").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mempool: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(mempool["ledger_transactions"].as_array().unwrap().len(), 1);
        assert_eq!(
            mempool["ledger_transactions"][0]["version"],
            PROTOCOL_VERSION
        );
        let detail = mempool_detail(
            &transaction_state,
            mempool["ledger_transactions"][0]["id"].as_str().unwrap(),
        )
        .await;
        assert_eq!(mempool["ledger_transactions"][0]["input_count"], 1);
        assert_balanced(&detail);
        assert_eq!(
            mempool["ledger_transactions"][0]["fee_split"],
            detail["fee_split"]
        );
        assert_eq!(
            detail["inputs"][0]["signature"].as_str().unwrap().len(),
            128
        );
        assert_eq!(detail["inputs"][0]["value"], GENESIS_REWARD);
        assert!(mempool["mining_tickets"].as_array().unwrap().is_empty());
        let response = router(transaction_state)
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["wallet_balance"], GENESIS_REWARD);
        assert_eq!(status["wallet_spendable_balance"], 0);
        assert_eq!(status["wallet_utxos"], 1);
        assert_eq!(status["wallet_spendable_utxos"], 0);
        assert_eq!(status["wallet_public_key"], hex(&wallet.public_key()));

        let ticket_directory = tempfile::tempdir().unwrap();
        let ticket_state = management_state(&ticket_directory, wallet, TOKEN);
        let response = router(ticket_state.clone())
            .oneshot(json_request(
                "/api/mining-tickets",
                serde_json::json!({
                    "budget": minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap(),
                    "duration": 1,
                    "fee": 3
                })
                .to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(ticket_state.node.lock().await.pending_ticket_count(), 1);
        assert_eq!(ticket_state.node.lock().await.reserved_inputs().len(), 1);
        let response = router(ticket_state.clone())
            .oneshot(Request::get("/api/mempool").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mempool: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(mempool["mining_tickets"][0]["fee"], 3);
        assert_eq!(
            mempool["mining_tickets"][0]["lifecycle_heartbeats"],
            mempool["mining_tickets"][0]["duration"].as_u64().unwrap() + 9
        );
        assert_eq!(mempool["mining_tickets"][0]["version"], PROTOCOL_VERSION);
        let detail = mempool_detail(
            &ticket_state,
            mempool["mining_tickets"][0]["id"].as_str().unwrap(),
        )
        .await;
        assert_balanced(&detail);
        assert_eq!(detail["lottery_seed"].as_str().unwrap().len(), 64);
        assert_eq!(detail["inputs"].as_array().unwrap().len(), 1);
        assert_eq!(
            detail["inputs"][0]["signature"].as_str().unwrap().len(),
            128
        );
        assert!(detail.get("change").is_some());
        assert!(
            mempool["ledger_transactions"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let response = router(ticket_state.clone())
            .oneshot(authenticated_post("/api/hash-mining/heartbeat", TOKEN))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mined: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(mined["height"], 1);
        assert_eq!(mined["hash"].as_str().unwrap().len(), 64);
        assert_eq!(ticket_state.node.lock().await.status().height, 1);
        assert_eq!(ticket_state.node.lock().await.pending_ticket_count(), 0);
    }

    #[tokio::test]
    async fn resumed_mining_bootstraps_the_first_ticket_exactly_once() {
        // The miner loop calls this on every iteration, including after a restart that resumed
        // mining from a saved preference without going through the HTTP toggle.
        let directory = tempfile::tempdir().unwrap();
        let state = management_state(
            &directory,
            Arc::new(Wallet::from_seed([42; 32])),
            "0123456789abcdef0123456789abcdef",
        );
        let mut node = state.node.lock().await;
        let ticket = submit_bootstrap_ticket(&mut node, [42; 32])
            .unwrap()
            .unwrap();
        assert_eq!(ticket.mining_public_key, [42; 32]);
        assert_eq!(node.pending_ticket_count(), 1);
        assert!(
            submit_bootstrap_ticket(&mut node, [43; 32])
                .unwrap()
                .is_none()
        );
        assert_eq!(node.pending_ticket_count(), 1);
    }

    #[tokio::test]
    async fn enabling_first_miner_bootstraps_expenditure_and_pays_its_inclusion_fee() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let directory = tempfile::tempdir().unwrap();
        let wallet = Arc::new(Wallet::from_seed([42; 32]));
        let state = management_state(&directory, wallet, TOKEN);
        let worker_count = max_mining_workers().min(4);

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/hash-mining",
                serde_json::json!({ "enabled": true, "workers": 0 }).to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/hash-mining",
                serde_json::json!({ "enabled": true, "workers": worker_count }).to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let enabled: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(enabled["bootstrap_ticket_submitted"], true);
        assert_eq!(enabled["workers"], worker_count);
        assert_eq!(
            state.heartbeat_mining_workers.load(Ordering::Relaxed),
            worker_count
        );
        assert_eq!(state.node.lock().await.pending_ticket_count(), 1);

        let response = router(state.clone())
            .oneshot(authenticated_post("/api/hash-mining/heartbeat", TOKEN))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mined: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let response = router(state.clone())
            .oneshot(
                Request::get(format!("/api/blocks/{}", mined["hash"].as_str().unwrap()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail["tickets"].as_array().unwrap().len(), 1);
        assert_eq!(detail["tickets"][0]["budget"], GENESIS_REWARD - 1_000_000);
        assert_eq!(detail["tickets"][0]["version"], PROTOCOL_VERSION);
        assert_eq!(detail["tickets"][0]["fee"], 1_000_000);
        assert_balanced(&detail["tickets"][0]);
        assert_eq!(
            detail["tickets"][0]["inputs"][0]["signature"]
                .as_str()
                .unwrap()
                .len(),
            128
        );

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/ledger-mining",
                serde_json::json!({ "enabled": true }).to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.ledger_mining.load(Ordering::Relaxed));

        let response = router(state)
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(status["wallet_balance"].as_u64().unwrap() >= 1_000_000);
        assert_eq!(status["heartbeat_mining_enabled"], true);
        assert_eq!(status["heartbeat_mining_workers"], worker_count);
        assert_eq!(status["heartbeat_mining_max_workers"], max_mining_workers());
        assert_eq!(status["ledger_mining_enabled"], true);
    }

    /// What the transaction modal shows: everything on the right must equal the inputs.
    fn assert_balanced(view: &serde_json::Value) {
        let value = |field: &serde_json::Value| field.as_u64().unwrap();
        let inputs = view["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| value(&input["value"]))
            .sum::<u64>();
        let outputs = view["outputs"].as_array().map_or(0, |outputs| {
            outputs.iter().map(|output| value(&output["value"])).sum()
        });
        let fees = if view["fee_split"].is_null() {
            value(&view["fee"])
        } else {
            let split = &view["fee_split"];
            value(&split["heartbeat_miner"]["value"]) + value(&split["ledger_winner"]["value"])
        };
        let locked = view["budget"].as_u64().unwrap_or(0);
        let change = view["change"]["value"].as_u64().unwrap_or(0);
        assert!(inputs > 0);
        assert_eq!(inputs, outputs + locked + change + fees, "{view}");
    }

    async fn mempool_detail(state: &ConsensusHttpState, id: &str) -> serde_json::Value {
        let response = router(state.clone())
            .oneshot(
                Request::get(format!("/api/mempool/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn loopback_hosts_are_recognized_with_and_without_a_port() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:8080",
            "localhost:8080",
            "[::1]:8080",
            "[::1]",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "evil.example",
            "evil.example:8080",
            "127.0.0.1.evil.example:80",
            "",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[tokio::test]
    async fn loopback_listeners_reject_rebound_hosts_on_reads() {
        let application = Router::new()
            .route("/api/status", get(|| async { "ok" }))
            .layer(middleware::from_fn(require_loopback_host));
        let request = |host: &str| {
            Request::get("/api/status")
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap()
        };
        let allowed = application
            .clone()
            .oneshot(request("127.0.0.1:8080"))
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        let rejected = application
            .oneshot(request("evil.example:8080"))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn block_fee_shares_sum_to_the_split_of_the_block_total() {
        let fees = [1_u64, 1, 3, 4, 7];
        let total = fees.iter().sum::<u64>();
        let mut fees_before = 0;
        let (mut heartbeat, mut winner) = (0, 0);
        for fee in fees {
            let split = block_fee_split(fees_before, fee, [1; 32], Some([2; 32]));
            assert_eq!(split.heartbeat_miner.value + split.ledger_winner.value, fee);
            assert!(
                split
                    .heartbeat_miner
                    .value
                    .abs_diff(split.ledger_winner.value)
                    <= 1
            );
            heartbeat += split.heartbeat_miner.value;
            winner += split.ledger_winner.value;
            fees_before += fee;
        }
        assert_eq!((heartbeat, winner), split_transaction_fees(total));
    }

    #[test]
    fn projected_block_details_match_the_view_built_from_chain_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap();
        let mut chain = crate::consensus::Chain::from_genesis().unwrap();
        let timestamp = chain.tip().tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (block, _) =
            crate::consensus::mine_child(chain.tip(), timestamp, [42; 32], Vec::new(), None)
                .unwrap();
        let parent = chain.tip().clone();
        chain.submit(block.clone(), timestamp).unwrap();
        store.project(&chain).unwrap();

        let stored: serde_json::Value =
            serde_json::from_str(&store.block_detail(&hex(&block.hash())).unwrap().unwrap())
                .unwrap();

        let built = block_detail_view(&block, Some(&parent)).unwrap();
        assert_eq!(stored, serde_json::to_value(built).unwrap());
        assert_eq!(store.block_detail(&hex(&[9; 32])).unwrap(), None);
    }

    #[tokio::test]
    async fn block_details_expose_transactions_from_a_ledger_sidecar() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        let directory = tempfile::tempdir().unwrap();
        let wallet = Arc::new(Wallet::from_seed([42; 32]));
        let state = management_state(&directory, Arc::clone(&wallet), TOKEN);

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/hash-mining",
                serde_json::json!({ "enabled": true }).to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        {
            let mut node = state.node.lock().await;
            let timestamp = node
                .chain()
                .tip()
                .tip_timestamp
                .saturating_add(HEARTBEAT_TARGET_INTERVAL_SECS);
            for draft in node.candidate_drafts(wallet.public_key()).unwrap() {
                let candidate = wallet.sign_candidate(draft).unwrap();
                node.submit_candidate(candidate).unwrap();
            }
            node.mine_next(timestamp, wallet.public_key()).unwrap();
        }

        let response = router(state.clone())
            .oneshot(json_request(
                "/api/ledger-transactions",
                serde_json::json!({
                    "recipient": hex(&[77; 32]),
                    "amount": 1_000_000,
                    "fee": 100,
                })
                .to_string(),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for attempt in 0_u64..200 {
            let mut node = state.node.lock().await;
            if node.status().tracked_tickets == 0 && node.pending_ticket_count() == 0 {
                let tip = node.chain().tip();
                let budget = minimum_ticket_budget(
                    LEDGER_TARGET_INTERVAL_HEARTBEATS,
                    subsidy(tip.ledger_blocks, tip.minted).unwrap(),
                )
                .unwrap();
                wallet
                    .submit_ticket(
                        &mut node,
                        TicketParameters {
                            budget,
                            duration: LEDGER_TARGET_INTERVAL_HEARTBEATS,
                            fee: 1,
                            mining_public_key: wallet.public_key(),
                            lottery_seed: tagged_hash(
                                b"IUNA-HTTP-BLOCK-DETAIL-TEST-v1",
                                &attempt.to_be_bytes(),
                            ),
                        },
                    )
                    .unwrap();
            }
            let timestamp = node
                .chain()
                .tip()
                .tip_timestamp
                .saturating_add(HEARTBEAT_TARGET_INTERVAL_SECS);
            for draft in node.candidate_drafts(wallet.public_key()).unwrap() {
                let candidate = wallet.sign_candidate(draft).unwrap();
                node.submit_candidate(candidate).unwrap();
            }
            node.mine_next(timestamp, wallet.public_key()).unwrap();
            if node.pending_transaction_count() == 0 {
                break;
            }
        }
        // The bootstrap ticket is signed by the genesis key, so buy one with this wallet.
        {
            let mut node = state.node.lock().await;
            let tip = node.chain().tip();
            let budget = minimum_ticket_budget(
                LEDGER_TARGET_INTERVAL_HEARTBEATS,
                subsidy(tip.ledger_blocks, tip.minted).unwrap(),
            )
            .unwrap();
            let timestamp = tip
                .tip_timestamp
                .saturating_add(HEARTBEAT_TARGET_INTERVAL_SECS);
            wallet
                .submit_ticket(
                    &mut node,
                    TicketParameters {
                        budget,
                        duration: LEDGER_TARGET_INTERVAL_HEARTBEATS,
                        fee: 1,
                        mining_public_key: wallet.public_key(),
                        lottery_seed: [9; 32],
                    },
                )
                .unwrap();
            node.mine_next(timestamp, wallet.public_key()).unwrap();
        }
        let ledger_hash = {
            let node = state.node.lock().await;
            let ledger_hash = node
                .chain()
                .canonical_blocks()
                .into_iter()
                .find(|block| {
                    block
                        .sidecar
                        .as_ref()
                        .is_some_and(|sidecar| !sidecar.transactions.is_empty())
                })
                .map(|block| block.hash())
                .expect("a Ledger block should include the pending transaction");
            state.ui_store.project(node.chain()).unwrap();
            ledger_hash
        };
        let response = router(state.clone())
            .oneshot(Request::get("/api/heartbeats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let heartbeats: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ledger_summary = heartbeats
            .as_array()
            .unwrap()
            .iter()
            .find(|heartbeat| heartbeat["block_hash"] == hex(&ledger_hash))
            .unwrap();
        assert_eq!(
            ledger_summary["ledger_miner_public_key"],
            hex(&wallet.public_key())
        );

        let response = router(state.clone())
            .oneshot(
                Request::get(format!("/api/blocks/{}", hex(&ledger_hash)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            detail["ledger"]["transactions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(detail["ledger"]["deadline_fallback"], false);
        assert_eq!(
            detail["ledger"]["candidate_signature"]
                .as_str()
                .unwrap()
                .len(),
            128
        );
        assert!(
            detail["ledger"]["transactions"][0]["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|output| output["owner"] == hex(&[77; 32]) && output["value"] == 1_000_000)
        );
        assert_eq!(detail["ledger"]["transactions"][0]["fee"], 100);
        assert_balanced(&detail["ledger"]["transactions"][0]);
        let split = &detail["ledger"]["transactions"][0]["fee_split"];
        assert_eq!(split["heartbeat_miner"]["value"], 50);
        assert_eq!(split["ledger_winner"]["value"], 50);
        assert_eq!(split["ledger_winner"]["owner"], hex(&wallet.address()));
        assert_eq!(
            split["heartbeat_miner"]["value"],
            detail["rewards"]["heartbeat"]["ledger_transaction_fees"]
        );
        assert_eq!(
            split["ledger_winner"]["value"],
            detail["rewards"]["ledger"]["ledger_transaction_fees"]
        );
        assert_eq!(detail["rewards"]["ledger"]["ledger_transaction_fees"], 50);
        assert_eq!(
            detail["rewards"]["heartbeat"]["ledger_transaction_fees"],
            50
        );
        assert_eq!(
            detail["rewards"]["ledger"]["total"].as_u64().unwrap(),
            detail["rewards"]["ledger"]["subsidy"].as_u64().unwrap() + 50
        );
        assert!(detail["rewards"]["heartbeat"]["total"].as_u64().unwrap() > 0);

        let response = router(state.clone())
            .oneshot(
                Request::get("/api/wallet/activity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let activity: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let confirmed = activity["confirmed"].as_array().unwrap();
        assert!(confirmed.iter().any(|item| item["kind"] == "sent"
            && item["amount"] == 1_000_000
            && item["counterparty"] == hex(&[77; 32])
            && item["id"] == detail["ledger"]["transactions"][0]["id"]));
        assert!(confirmed.iter().any(|item| item["kind"] == "ticket"));
        assert!(
            confirmed
                .iter()
                .any(|item| item["kind"] == "heartbeat_reward"
                    && item["amount"] == detail["rewards"]["heartbeat"]["total"])
        );
        assert!(confirmed.iter().any(|item| item["kind"] == "ledger_reward"
            && item["amount"] == detail["rewards"]["ledger"]["total"]));
        assert!(!activity["utxos"].as_array().unwrap().is_empty());
    }

    fn management_state(
        directory: &tempfile::TempDir,
        wallet: Arc<Wallet>,
        token: &str,
    ) -> ConsensusHttpState {
        let node =
            Node::open(ConsensusChainStore::open(directory.path().join("chain.sqlite3")).unwrap())
                .unwrap();
        ConsensusHttpState {
            node_instance_id: "00112233445566778899aabbccddeeff".to_owned(),
            node: Arc::new(Mutex::new(node)),
            ui_store: ConsensusUiStore::open(directory.path().join("ui.sqlite3")).unwrap(),
            wallet: wallet_state(directory, Some(wallet)),
            operator_token_hash: Some(operator_token_hash(token).unwrap()),
            trusted_management: false,
            relay: None,
            peers: PeerRegistry::default(),
            heartbeat_mining: Arc::new(AtomicBool::new(false)),
            heartbeat_mining_workers: Arc::new(AtomicUsize::new(1)),
            heartbeat_hashes: Arc::new(AtomicU64::new(0)),
            ledger_mining: Arc::new(AtomicBool::new(false)),
            ledger_last_ticket_height: Arc::new(AtomicU64::new(0)),
        }
    }

    fn wallet_state(
        directory: &tempfile::TempDir,
        wallet: Option<Arc<Wallet>>,
    ) -> Arc<RwLock<WalletState>> {
        let public_key = wallet.as_ref().map(|wallet| wallet.public_key());
        Arc::new(RwLock::new(WalletState {
            path: directory.path().join("wallet.json"),
            unlocked: wallet,
            public_key,
        }))
    }

    fn json_request(path: &str, body: String, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::from(body)).unwrap()
    }

    fn local_json_request(path: &str, body: String) -> Request<Body> {
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::HOST, "127.0.0.1:8080")
            .header(header::ORIGIN, "http://127.0.0.1:8080")
            .body(Body::from(body))
            .unwrap()
    }

    fn authenticated_post(path: &str, token: &str) -> Request<Body> {
        Request::post(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }
}
