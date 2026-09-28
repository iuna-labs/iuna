use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer};

use crate::{
    adapters::{config_store::UiConfig, ui_data_store::BlockMetricRow},
    app::PeerInfo,
    domain::{Amount, BurnLeaderRank, OutPoint, Transaction, TransactionV2, TxOutput},
    ip_geolocation::CountryCode,
};

#[derive(Debug, Deserialize)]
pub(super) struct AuthForm {
    pub(super) password: SecretString,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChangePasswordForm {
    pub(super) old_password: SecretString,
    pub(super) new_password: SecretString,
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
    pub(super) local_tip_hash: String,
    pub(super) finalized_height: Option<u64>,
    pub(super) finalized_hash: Option<String>,
    pub(super) last_block_age_ms: Option<u64>,
    pub(super) best_known_height: u64,
    pub(super) sync_start_height: Option<u64>,
    pub(super) sync_validated_height: Option<u64>,
    pub(super) sync_target_height: Option<u64>,
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
    pub(super) pending_v2_transactions: usize,
    pub(super) last_finalizer_mode: Option<String>,
    pub(super) last_finalizer_rank: Option<u32>,
    pub(super) last_block_finalizer: Option<String>,
    pub(super) current_leader: Option<String>,
    pub(super) wallet_is_current_leader: bool,
    pub(super) last_auto_finalization_status: Option<String>,
    pub(super) vdf_rounds: u64,
    pub(super) vdf_target_block_ms: u64,
    pub(super) rejected_blocks: u64,
    pub(super) rejected_block_batches: u64,
    pub(super) rejected_snapshots: u64,
    pub(super) rejected_chain_payloads: u64,
    pub(super) last_chain_payload_error: Option<String>,
    pub(super) network_time_offset_ms: Option<i64>,
    pub(super) bad_clock_peers: usize,
    pub(super) last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct PeerPresentation {
    #[serde(flatten)]
    pub(super) peer: PeerInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) country_code: Option<CountryCode>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MempoolCounts {
    pub(super) plain_transactions: usize,
    pub(super) v2_transactions: usize,
}

impl MempoolCounts {
    pub(super) fn total(&self) -> usize {
        self.plain_transactions.saturating_add(self.v2_transactions)
    }
}

#[derive(Clone, Debug)]
pub(super) struct NetworkHealthLocalState {
    pub(super) height: u64,
    pub(super) tip_hash: String,
    pub(super) finalized_height: Option<u64>,
    pub(super) finalized_hash: Option<String>,
    pub(super) tip_timestamp_ms: Option<u64>,
    pub(super) sync_start_height: Option<u64>,
    pub(super) sync_validated_height: Option<u64>,
    pub(super) sync_target_height: Option<u64>,
    pub(super) pending_transactions: usize,
    pub(super) last_finalizer_mode: Option<String>,
    pub(super) last_finalizer_rank: Option<u32>,
    pub(super) last_block_finalizer: Option<String>,
    pub(super) current_leader: Option<String>,
    pub(super) wallet_is_current_leader: bool,
    pub(super) last_auto_finalization_status: Option<String>,
    pub(super) vdf_rounds: u64,
    pub(super) vdf_target_block_ms: u64,
    pub(super) rejected_blocks: u64,
    pub(super) rejected_block_batches: u64,
    pub(super) rejected_snapshots: u64,
    pub(super) last_chain_payload_error: Option<String>,
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

#[derive(Debug, Deserialize)]
pub(super) struct StratumSettingsForm {
    pub(super) enabled: bool,
    pub(super) bind_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WalletEndpointSettingsForm {
    pub(super) enabled: bool,
    pub(super) bind_port: Option<u16>,
}

#[derive(Debug, Serialize)]
pub(super) struct ConfigResponse {
    #[serde(flatten)]
    pub(super) config: UiConfig,
    pub(super) p2p_inbound_runtime_active: bool,
    pub(super) p2p_runtime_bind_addr: String,
    pub(super) stratum_runtime_enabled: bool,
    pub(super) stratum_runtime_listen_addr: Option<String>,
    pub(super) wallet_endpoint_runtime_enabled: bool,
    pub(super) wallet_endpoint_runtime_listen_addr: Option<String>,
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
    pub(super) seed_phrase: SecretString,
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
    pub(super) reward: Option<bool>,
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
pub(crate) struct WalletTransactionFilters {
    pub(crate) transfer: bool,
    pub(crate) mine: bool,
    pub(crate) burn: bool,
    pub(crate) reward: bool,
}

impl Default for WalletTransactionFilters {
    fn default() -> Self {
        Self {
            transfer: true,
            mine: true,
            burn: true,
            reward: true,
        }
    }
}

impl WalletTransactionFilters {
    pub(super) fn from_query(query: WalletTransactionsQuery) -> Self {
        Self {
            transfer: query.tx.unwrap_or(true),
            mine: query.mine.unwrap_or(true),
            burn: query.burn.unwrap_or(true),
            reward: query.reward.unwrap_or(true),
        }
    }

    pub(super) fn allows(self, transaction: &Transaction) -> bool {
        match transaction {
            Transaction::Transfer { .. } => self.transfer,
            Transaction::Mine { .. } => self.mine,
            Transaction::Burn { .. } => self.burn,
        }
    }

    pub(super) fn allows_v2(self, transaction: &TransactionV2) -> bool {
        match transaction {
            TransactionV2::Migration { .. } | TransactionV2::Transfer { .. } => self.transfer,
            TransactionV2::Mine { .. } => self.mine,
            TransactionV2::Burn { .. } => self.burn,
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
    pub(super) preparing: bool,
    pub(super) latest: Option<BlockMetricRow>,
    pub(super) charts: Vec<MetricsChart>,
    pub(super) leaderboards: MetricsLeaderboards,
    pub(super) top_mine_proofs: Vec<MineProofLeaderboardEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricsLeaderboards {
    pub(super) balances: Vec<LeaderboardEntry>,
    pub(super) miners: Vec<LeaderboardEntry>,
    pub(super) burners: Vec<LeaderboardEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LeaderboardEntry {
    pub(super) address: String,
    pub(super) amount: Amount,
    pub(super) count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MineProofLeaderboardEntry {
    pub(super) height: u64,
    pub(super) address: String,
    pub(super) proof_bits: u32,
    pub(super) difficulty_bits: u32,
    pub(super) proof_hash: String,
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
    Bytes,
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
    #[serde(serialize_with = "serialize_optional_secret")]
    pub(super) seed_phrase: Option<SecretString>,
    pub(super) dev_verify_bypass: bool,
    pub(super) requires_peer: bool,
}

fn serialize_optional_secret<S>(
    value: &Option<SecretString>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(secret) => serializer.serialize_some(secret.expose_secret()),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod secret_tests {
    use super::{AuthForm, ChangePasswordForm};

    #[test]
    fn auth_form_debug_output_redacts_passwords() {
        let auth = AuthForm {
            password: "correct-horse-battery-staple".into(),
        };
        let change = ChangePasswordForm {
            old_password: "old-password-value".into(),
            new_password: "new-password-value".into(),
        };

        let debug = format!("{auth:?} {change:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("correct-horse-battery-staple"));
        assert!(!debug.contains("old-password-value"));
        assert!(!debug.contains("new-password-value"));
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WalletTransactionRow {
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
    pub(super) difficulty_bits: Option<u32>,
    pub(super) proof_bits: Option<u32>,
    pub(super) proof_hash: Option<String>,
    pub(super) reward_total: Option<Amount>,
    pub(super) reward_fee_inputs: Vec<UiRewardFeeInput>,
    pub(super) reward_outputs: Vec<UiRewardOutput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UiRewardFeeInput {
    pub(super) transaction_kind: &'static str,
    pub(super) amount: Amount,
    pub(super) owner: String,
    pub(super) signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UiRewardOutput {
    pub(super) label: String,
    pub(super) amount: Amount,
    pub(super) address: String,
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
pub(crate) struct WalletTransactionContext {
    pub(crate) status: &'static str,
    pub(crate) block_height: Option<u64>,
    pub(crate) timestamp_ms: Option<u64>,
    pub(crate) block_finalizer: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiBlock {
    pub(super) height: u64,
    pub(super) prev_hash: String,
    pub(super) timestamp_ms: u64,
    pub(super) miner: String,
    pub(super) reward_address: Option<String>,
    pub(super) finalizer_mode: crate::domain::FinalizerMode,
    pub(super) finalizer_rank: u32,
    pub(super) reward: Amount,
    pub(super) total_fees: Amount,
    pub(super) lost_iuna: Amount,
    pub(super) total_bytes: usize,
    pub(super) header_and_proof_bytes: usize,
    pub(super) transaction_bytes: usize,
    pub(super) transaction_byte_breakdown: Vec<UiByteBreakdown>,
    pub(super) burn_bundle_bytes: usize,
    pub(super) burn_bundle_quorum: UiBurnBundleQuorum,
    pub(super) vdf_rounds: u64,
    pub(super) vdf_output: String,
    pub(super) leader_proof: Option<crate::domain::LeaderProof>,
    pub(super) burn_leader_ranks: Vec<BurnLeaderRank>,
    pub(super) transactions: Vec<UiTransaction>,
    pub(super) burn_bundles: Vec<UiBurnBundle>,
    pub(super) hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiBurnBundleQuorum {
    pub(super) burn_bundles_included: usize,
    pub(super) committee_size: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiByteBreakdown {
    pub(super) label: &'static str,
    pub(super) bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UiBurnBundle {
    pub(super) slot: u8,
    pub(super) member: String,
    pub(super) reward_address: Option<String>,
    pub(super) hash: String,
    pub(super) byte_size: usize,
    pub(super) burns: Vec<UiTransaction>,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct UiTxInput {
    pub(super) outpoint: OutPoint,
    pub(super) owner: String,
    pub(super) signature: String,
    pub(super) amount: Option<Amount>,
    pub(super) address: Option<String>,
}
