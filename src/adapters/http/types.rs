use serde::{Deserialize, Serialize};

use crate::{
    adapters::{config_store::UiConfig, ui_data_store::BlockMetricRow},
    domain::{Amount, BurnLeaderRank, OutPoint, Transaction, TxOutput},
};

#[derive(Debug, Deserialize)]
pub(super) struct AuthForm {
    pub(super) password: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChangePasswordForm {
    pub(super) old_password: String,
    pub(super) new_password: String,
}

#[derive(Debug, Serialize)]
pub(super) struct AuthStatusResponse {
    pub(super) configured: bool,
    pub(super) authenticated: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct NetworkHealthResponse {
    pub(super) ok: bool,
    pub(super) state: String,
    pub(super) local_height: u64,
    pub(super) best_known_height: u64,
    pub(super) shared_height: u64,
    pub(super) lag_blocks: u64,
    pub(super) outbound_peers: usize,
    pub(super) inbound_peers: usize,
    pub(super) healthy_peers: usize,
    pub(super) failed_peers: usize,
    pub(super) stale_peers: usize,
    pub(super) banned_peers: usize,
    pub(super) pending_transactions: usize,
    pub(super) pending_plain_transactions: usize,
    pub(super) pending_blinded_transactions: usize,
    pub(super) pending_blinded_reveals: usize,
    pub(super) network_time_offset_ms: Option<i64>,
    pub(super) bad_clock_peers: usize,
    pub(super) last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MempoolCounts {
    pub(super) plain_transactions: usize,
    pub(super) blinded_transactions: usize,
    pub(super) blinded_reveals: usize,
}

impl MempoolCounts {
    pub(super) fn total(&self) -> usize {
        self.plain_transactions
            .saturating_add(self.blinded_transactions)
            .saturating_add(self.blinded_reveals)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct NetworkHealthLocalState {
    pub(super) height: u64,
    pub(super) pending_transactions: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct BurnSettingsForm {
    pub(super) enabled: Option<bool>,
    pub(super) amount: Amount,
    pub(super) fee_per_byte: Option<Amount>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RecoveryVdfSettingsForm {
    pub(super) top_rank_percent: u8,
}

#[derive(Debug, Deserialize)]
pub(super) struct PowMiningForm {
    pub(super) enabled: bool,
    pub(super) workers: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MetricsSettingsForm {
    pub(super) enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChainResetForm {
    pub(super) confirm: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct P2pAnnounceForm {
    pub(super) addr: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct P2pInboundForm {
    pub(super) enabled: bool,
    pub(super) bind_port: Option<u16>,
}

#[derive(Debug, Serialize)]
pub(super) struct ConfigResponse {
    #[serde(flatten)]
    pub(super) config: UiConfig,
    pub(super) p2p_inbound_runtime_active: bool,
    pub(super) p2p_runtime_bind_addr: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct TransferForm {
    pub(super) to: String,
    pub(super) amount: Amount,
    pub(super) fee_per_byte: Option<Amount>,
    #[serde(default)]
    pub(super) utxos: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PeerForm {
    pub(super) peer: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AddressBookForm {
    pub(super) address: String,
    pub(super) name: String,
    pub(super) old_address: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AddressBookDeleteForm {
    pub(super) address: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConfigForm {
    pub(super) setup_complete: bool,
    #[serde(default)]
    pub(super) peer: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct SeedPhraseForm {
    pub(super) seed_phrase: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct BlocksQuery {
    pub(super) before_height: Option<u64>,
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MetricsQuery {
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PageQuery {
    pub(super) offset: Option<usize>,
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct WalletTransactionsQuery {
    pub(super) tx: Option<bool>,
    pub(super) mine: Option<bool>,
    pub(super) burn: Option<bool>,
    pub(super) offset: Option<usize>,
    pub(super) limit: Option<usize>,
}

impl WalletTransactionsQuery {
    pub(super) fn page(&self) -> PageQuery {
        PageQuery {
            offset: self.offset,
            limit: self.limit,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WalletTransactionFilters {
    pub(super) transfer: bool,
    pub(super) mine: bool,
    pub(super) burn: bool,
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
    pub(super) fn from_query(query: WalletTransactionsQuery) -> Self {
        Self {
            transfer: query.tx.unwrap_or(true),
            mine: query.mine.unwrap_or(false),
            burn: query.burn.unwrap_or(false),
        }
    }

    pub(super) fn allows(self, transaction: &Transaction) -> bool {
        match transaction {
            Transaction::Transfer { .. } => self.transfer,
            Transaction::Mine { .. } => self.mine,
            Transaction::Burn { .. } => self.burn,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ActionResponse {
    pub(super) ok: bool,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page<T> {
    pub(super) items: Vec<T>,
    pub(super) offset: usize,
    pub(super) limit: usize,
    pub(super) total: usize,
    pub(super) has_more: bool,
    pub(super) next_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricsResponse {
    pub(super) enabled: bool,
    pub(super) latest: Option<BlockMetricRow>,
    pub(super) charts: Vec<MetricsChart>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricsChart {
    pub(super) id: &'static str,
    pub(super) title: &'static str,
    pub(super) unit: &'static str,
    pub(super) value_kind: MetricsValueKind,
    pub(super) points: Vec<MetricsPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricsPoint {
    pub(super) height: u64,
    pub(super) value: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum MetricsValueKind {
    Number,
    Seconds,
    Iuna,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FeeEstimateResponse {
    pub(super) ok: bool,
    pub(super) error: Option<String>,
    pub(super) bytes: Option<usize>,
    pub(super) fee: Option<Amount>,
}

#[derive(Debug, Serialize)]
pub(super) struct WalletSetupResponse {
    pub(super) ok: bool,
    pub(super) error: Option<String>,
    pub(super) address: Option<String>,
    pub(super) seed_phrase: Option<String>,
    pub(super) dev_verify_bypass: bool,
    pub(super) requires_peer: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WalletTransactionRow {
    pub(super) kind: &'static str,
    pub(super) from: String,
    pub(super) to: Option<String>,
    pub(super) amount: Amount,
    pub(super) fee: Amount,
    pub(super) inputs: Vec<UiTxInput>,
    pub(super) outputs: Vec<TxOutput>,
    pub(super) change: Vec<TxOutput>,
    pub(super) signature: String,
    pub(super) status: &'static str,
    pub(super) block_height: Option<u64>,
    pub(super) timestamp_ms: Option<u64>,
    pub(super) block_finalizer: Option<String>,
    pub(super) direction: &'static str,
    pub(super) blinded: bool,
    pub(super) difficulty_bits: Option<u32>,
    pub(super) proof_bits: Option<u32>,
    pub(super) proof_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WalletUtxoRow {
    pub(super) outpoint: OutPoint,
    pub(super) address: String,
    pub(super) amount: Amount,
    pub(super) spendable: bool,
}

#[derive(Clone, Debug)]
pub(super) struct WalletTransactionContext {
    pub(super) status: &'static str,
    pub(super) block_height: Option<u64>,
    pub(super) timestamp_ms: Option<u64>,
    pub(super) block_finalizer: Option<String>,
    pub(super) blinded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiBlock {
    pub(super) height: u64,
    pub(super) prev_hash: String,
    pub(super) timestamp_ms: u64,
    pub(super) miner: String,
    pub(super) finalizer_mode: crate::domain::FinalizerMode,
    pub(super) finalizer_rank: u32,
    pub(super) reward: Amount,
    pub(super) total_fees: Amount,
    pub(super) total_bytes: usize,
    pub(super) transaction_bytes: usize,
    pub(super) transaction_byte_breakdown: Vec<UiByteBreakdown>,
    pub(super) blinded_transaction_bytes: usize,
    pub(super) reveal_bundle_bytes: usize,
    pub(super) vdf_rounds: u64,
    pub(super) vdf_output: String,
    pub(super) leader_proof: Option<crate::domain::LeaderProof>,
    pub(super) burn_leader_ranks: Vec<BurnLeaderRank>,
    pub(super) transactions: Vec<UiTransaction>,
    pub(super) revealed_transactions: Vec<UiTransaction>,
    pub(super) reveal_bundles: Vec<UiRevealBundle>,
    pub(super) hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiByteBreakdown {
    pub(super) label: &'static str,
    pub(super) bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UiRevealBundle {
    pub(super) slot: u8,
    pub(super) member: String,
    pub(super) hash: String,
    pub(super) byte_size: usize,
    pub(super) reveals: Vec<UiTransaction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiTransaction {
    pub(super) kind: &'static str,
    pub(super) from: String,
    pub(super) to: Option<String>,
    pub(super) amount: Amount,
    pub(super) fee: Amount,
    pub(super) inputs: Vec<UiTxInput>,
    pub(super) outputs: Vec<TxOutput>,
    pub(super) change: Vec<TxOutput>,
    pub(super) signature: String,
    pub(super) difficulty_bits: Option<u32>,
    pub(super) proof_bits: Option<u32>,
    pub(super) proof_hash: Option<String>,
    pub(super) commitment: Option<String>,
    pub(super) encrypted_size: Option<u32>,
    pub(super) expires_at_height: Option<u64>,
    pub(super) revealed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiTxInput {
    pub(super) outpoint: OutPoint,
    pub(super) owner: String,
    pub(super) signature: String,
    pub(super) amount: Option<Amount>,
    pub(super) address: Option<String>,
}
