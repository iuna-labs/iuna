use anyhow::Result;

use crate::domain::{
    BlindedReveal, BlindedTransaction, Block, BurnLeaderRank, Ledger, Transaction,
};

use super::NodeCore;

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
        self.queue_owned_blinded_payloads(&mut ledger)?;
        Ok(ledger)
    }

    pub fn chain(&self) -> &[Block] {
        self.ledger.chain()
    }

    pub fn chain_height(&self) -> u64 {
        self.ledger.height()
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

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        self.ledger.burn_leader_ranks_for_block(height)
    }

    pub fn pending_transactions(&self) -> Vec<Transaction> {
        self.ledger.pending().to_vec()
    }

    pub fn pending_blinded_transactions(&self) -> Vec<BlindedTransaction> {
        self.ledger.pending_blinded_transactions().to_vec()
    }

    pub fn pending_blinded_reveals(&self) -> Vec<BlindedReveal> {
        self.ledger.pending_blinded_reveals().to_vec()
    }

    pub fn pending_revealed_blinded_transactions(
        &self,
    ) -> Vec<crate::domain::RevealedBlindedTransaction> {
        self.ledger.pending_revealed_blinded_transactions()
    }
}
