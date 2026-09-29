use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    Amount, Block, BurnTicket, FinalityCheckpoint, LaunchProfile, LineageOwnerValues, OutPoint,
    Transaction, TransactionV2, TxOutput, UtxoLineageRoot,
};
use crate::compact::CompactBlockContext;

#[derive(Clone, Debug)]
pub struct Ledger {
    pub(super) chain: Vec<Block>,
    pub(super) genesis_allocations: BTreeMap<String, Amount>,
    pub(super) utxos: BTreeMap<OutPoint, TxOutput>,
    pub(super) utxo_lineage: BTreeMap<OutPoint, UtxoLineageRoot>,
    pub(super) lineage_values: BTreeMap<UtxoLineageRoot, Amount>,
    pub(super) lineage_owners: LineageOwnerValues,
    pub(super) hybrid_legacy_owners: BTreeMap<String, String>,
    pub(super) tickets: Vec<BurnTicket>,
    pub(super) mined_transaction_ids: BTreeSet<String>,
    pub(super) pending: Vec<Transaction>,
    pub(super) orphans: Vec<Transaction>,
    pub(super) pending_bytes: usize,
    pub(super) orphan_bytes: usize,
    pub(super) pending_v2: Vec<TransactionV2>,
    pub(super) pending_v2_bytes: usize,
    pub(super) mine_reward: Amount,
    /// PoW difficulty after each completed retarget window. Index zero is the
    /// launch difficulty; index `n` is the difficulty at anchor height
    /// `n * MINE_RETARGET_WINDOW_BLOCKS`.
    pub(super) mine_difficulty_windows: Vec<u32>,
    pub(super) initial_vdf_rounds: u64,
    pub(super) vdf_rounds: u64,
    pub(super) launch_profile: LaunchProfile,
    pub(super) compact_block_context: CompactBlockContext,
    pub(super) objective_finality_checkpoint: Option<FinalityCheckpoint>,
}

pub(super) fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
