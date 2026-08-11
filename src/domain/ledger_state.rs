use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    ActiveBlindedTransaction, Amount, BlindedReveal, BlindedTransaction, Block, BurnTicket,
    LaunchProfile, OutPoint, Transaction, TxOutput,
};

#[derive(Clone, Debug)]
pub struct Ledger {
    pub(super) chain: Vec<Block>,
    pub(super) genesis_allocations: BTreeMap<String, Amount>,
    pub(super) utxos: BTreeMap<OutPoint, TxOutput>,
    pub(super) tickets: Vec<BurnTicket>,
    pub(super) pending: Vec<Transaction>,
    pub(super) orphans: Vec<Transaction>,
    pub(super) pending_blinded: Vec<BlindedTransaction>,
    pub(super) pending_reveals: Vec<BlindedReveal>,
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
