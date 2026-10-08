use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SharedMap<K, V>(Arc<BTreeMap<K, V>>);

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self(Arc::new(BTreeMap::new()))
    }
}

impl<K, V> From<BTreeMap<K, V>> for SharedMap<K, V> {
    fn from(value: BTreeMap<K, V>) -> Self {
        Self(Arc::new(value))
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for SharedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        BTreeMap::from_iter(iter).into()
    }
}

impl<K: Clone, V: Clone> SharedMap<K, V> {
    pub(super) fn into_owned(self) -> BTreeMap<K, V> {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

#[cfg(test)]
impl<K, V> SharedMap<K, V> {
    pub(super) fn backing_ptr(&self) -> *const BTreeMap<K, V> {
        Arc::as_ptr(&self.0)
    }
}

impl<K, V> Deref for SharedMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Clone, V: Clone> DerefMut for SharedMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SharedSet<T>(Arc<BTreeSet<T>>);

impl<T> Default for SharedSet<T> {
    fn default() -> Self {
        Self(Arc::new(BTreeSet::new()))
    }
}

impl<T> From<BTreeSet<T>> for SharedSet<T> {
    fn from(value: BTreeSet<T>) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Deref for SharedSet<T> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Clone> DerefMut for SharedSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SharedChain(Arc<Vec<Block>>);

impl From<Vec<Block>> for SharedChain {
    fn from(value: Vec<Block>) -> Self {
        Self(Arc::new(value))
    }
}

impl FromIterator<Block> for SharedChain {
    fn from_iter<T: IntoIterator<Item = Block>>(iter: T) -> Self {
        Vec::from_iter(iter).into()
    }
}

impl Deref for SharedChain {
    type Target = Vec<Block>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SharedChain {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

use super::{
    Amount, Block, BurnTicket, FinalityCheckpoint, LaunchProfile, OutPoint, Transaction,
    TransactionV2, TxOutput, UtxoLineageRoot,
};
use crate::compact::CompactBlockContext;

#[derive(Clone, Debug)]
pub struct Ledger {
    pub(super) chain: SharedChain,
    pub(super) history_pruned: bool,
    pub(super) genesis_allocations: BTreeMap<String, Amount>,
    pub(super) utxos: SharedMap<OutPoint, TxOutput>,
    pub(super) utxo_lineage: SharedMap<OutPoint, UtxoLineageRoot>,
    pub(super) lineage_values: SharedMap<UtxoLineageRoot, Amount>,
    pub(super) lineage_owners:
        SharedMap<UtxoLineageRoot, BTreeMap<String, BTreeMap<OutPoint, Amount>>>,
    pub(super) hybrid_legacy_owners: SharedMap<String, String>,
    pub(super) tickets: Vec<BurnTicket>,
    pub(super) mined_transaction_ids: SharedSet<String>,
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
