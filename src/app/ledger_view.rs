use anyhow::Result;

use std::collections::BTreeSet;

use crate::compact::CompactBlockSizeBreakdown;
use crate::domain::{Block, BurnLeaderRank, Ledger, OutPoint, Transaction, TransactionV2};
use std::collections::BTreeMap;

use super::{NodeCore, helpers::transaction_input_outpoints};

impl NodeCore {
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub(crate) fn clone_ledger(&self) -> Ledger {
        self.ledger.clone()
    }

    pub fn wallet_view_ledger(&self) -> Result<Ledger> {
        let mut ledger = self.ledger.clone();
        self.queue_local_block_anchor(&mut ledger)?;
        Ok(ledger)
    }

    pub fn wallet_pending_spent_outpoints(&self) -> BTreeSet<OutPoint> {
        let mut spent = self
            .ledger
            .pending()
            .iter()
            .flat_map(transaction_input_outpoints)
            .collect::<BTreeSet<_>>();
        if let Some((height, burn)) = &self.local_block_anchor_burn {
            if *height == self.ledger.height() && !self.ledger.has_transaction(burn.signature()) {
                spent.extend(transaction_input_outpoints(burn));
            }
        }
        spent
    }

    pub fn chain(&self) -> &[Block] {
        self.ledger.chain()
    }

    pub fn chain_height(&self) -> u64 {
        self.ledger.height()
    }

    pub fn chain_tip_hash(&self) -> String {
        self.ledger.tip_hash().to_string()
    }

    pub fn has_real_chain(&self) -> bool {
        !self.ledger.is_setup_placeholder()
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.ledger.recent_blocks(limit)
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_before(before_height, limit)
    }

    pub(crate) fn block_storage_size_breakdowns(
        &self,
        blocks: &[Block],
    ) -> BTreeMap<String, CompactBlockSizeBreakdown> {
        self.ledger.storage_size_breakdowns(blocks)
    }

    pub(crate) fn chain_storage_bytes_by_hash(&self) -> Result<BTreeMap<String, u64>> {
        self.ledger.chain_storage_bytes_by_hash()
    }

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        self.ledger.burn_leader_ranks_for_block(height)
    }

    pub fn pending_transactions(&self) -> Vec<Transaction> {
        self.ledger.pending().to_vec()
    }

    pub fn pending_transactions_v2(&self) -> Vec<TransactionV2> {
        self.ledger.pending_v2().to_vec()
    }
}
