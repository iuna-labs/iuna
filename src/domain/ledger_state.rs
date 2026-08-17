use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    ActiveBlindedTransaction, Amount, BlindedReveal, BlindedTransaction, Block, BurnTicket,
    LaunchProfile, LineageOwnerValues, OutPoint, Transaction, TxOutput, UtxoLineageRoot,
};

#[derive(Clone, Debug)]
pub struct Ledger {
    pub(super) chain: Vec<Block>,
    pub(super) genesis_allocations: BTreeMap<String, Amount>,
    pub(super) utxos: BTreeMap<OutPoint, TxOutput>,
    pub(super) utxo_lineage: BTreeMap<OutPoint, UtxoLineageRoot>,
    pub(super) lineage_values: BTreeMap<UtxoLineageRoot, Amount>,
    pub(super) lineage_owners: LineageOwnerValues,
    pub(super) tickets: Vec<BurnTicket>,
    pub(super) pending: Vec<Transaction>,
    pub(super) orphans: Vec<Transaction>,
    pub(super) pending_blinded: Vec<BlindedTransaction>,
    pub(super) pending_reveals: Vec<BlindedReveal>,
    pub(super) pending_bytes: usize,
    pub(super) orphan_bytes: usize,
    pub(super) pending_blinded_bytes: usize,
    pub(super) pending_reveal_bytes: usize,
    pub(super) active_blinded: BTreeMap<String, ActiveBlindedTransaction>,
    pub(super) mine_reward: Amount,
    pub(super) initial_vdf_rounds: u64,
    pub(super) vdf_rounds: u64,
    pub(super) launch_profile: LaunchProfile,
}

pub(super) fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
