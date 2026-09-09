use anyhow::Result;

use super::mine_policy::{MINE_RETARGET_WINDOW_BLOCKS, retarget_mine_difficulty_bits};
use super::ticket::{
    BurnTicket, base_vdf_rounds_for_finalizer_rank, mine_action_count, ranked_tickets_for_height,
    vdf_rounds_for_finalizer_rank,
};
use super::vdf::{recent_vdf_retarget_average_observed_block_ms, retarget_vdf_rounds};
use super::{Block, FinalizerMode, Ledger};

impl Ledger {
    pub(super) fn next_vdf_rounds_after_tip(&self) -> u64 {
        let Some(tip) = self.chain.last() else {
            return self.vdf_rounds;
        };
        if tip.height < 2 {
            return self.vdf_rounds;
        }

        let Some(average_observed_ms) = recent_vdf_retarget_average_observed_block_ms(&self.chain)
        else {
            return self.vdf_rounds;
        };
        let base_rounds = base_vdf_rounds_for_finalizer_rank(tip.vdf_rounds, tip.finalizer_rank);
        retarget_vdf_rounds(base_rounds, average_observed_ms)
    }

    pub fn expected_leader_for_next_block(&self) -> Option<String> {
        self.selected_ticket_for_height(self.tip().height + 1)
            .map(|ticket| ticket.owner)
    }

    pub fn finalizer_rank_for_next_block(&self, miner: &str) -> Option<u32> {
        self.finalizer_ticket_for_miner(self.tip().height + 1, miner)
            .map(|(rank, _)| rank)
    }

    pub fn finalizer_rank_count_for_next_block(&self) -> usize {
        ranked_tickets_for_height(self.tip(), self.tip().height + 1, &self.tickets).len()
    }

    pub(super) fn selected_ticket_for_height(&self, height: u64) -> Option<BurnTicket> {
        self.ticket_for_finalizer_rank(height, 0)
    }

    pub(super) fn ticket_for_finalizer_rank(&self, height: u64, rank: u32) -> Option<BurnTicket> {
        ranked_tickets_for_height(self.tip(), height, &self.tickets)
            .get(rank as usize)
            .cloned()
    }

    pub(super) fn finalizer_ticket_for_miner(
        &self,
        height: u64,
        miner: &str,
    ) -> Option<(u32, BurnTicket)> {
        ranked_tickets_for_height(self.tip(), height, &self.tickets)
            .into_iter()
            .enumerate()
            .find(|(_, ticket)| ticket.owner == miner)
            .and_then(|(rank, ticket)| {
                let rank = u32::try_from(rank).ok()?;
                Some((rank, ticket))
            })
    }

    pub(super) fn vdf_rounds_for_finalizer_rank(&self, rank: u32) -> Result<u64> {
        vdf_rounds_for_finalizer_rank(self.vdf_rounds, rank)
    }

    pub(super) fn recovery_vdf_rounds(&self) -> Result<u64> {
        vdf_rounds_for_finalizer_rank(self.vdf_rounds, 0)
    }

    pub(super) fn expected_vdf_rounds_for_block(&self, block: &Block) -> Result<u64> {
        match block.finalizer_mode {
            FinalizerMode::Ticket => self.vdf_rounds_for_finalizer_rank(block.finalizer_rank),
            FinalizerMode::Recovery => self.recovery_vdf_rounds(),
        }
    }

    pub(super) fn mine_difficulty_bits_for_anchor_height(&self, anchor_height: u64) -> u32 {
        let completed_windows = anchor_height / MINE_RETARGET_WINDOW_BLOCKS;
        if let Ok(index) = usize::try_from(completed_windows)
            && let Some(difficulty) = self.mine_difficulty_windows.get(index)
        {
            return *difficulty;
        }

        // Tests and migration helpers may construct synthetic chains directly.
        // Keep a linear fallback for those callers; production ledgers update
        // the cache as each block is applied.
        mine_difficulty_windows_for_chain(
            &self.chain,
            self.launch_profile.mine_difficulty_bits,
            anchor_height,
        )
        .last()
        .copied()
        .unwrap_or(self.launch_profile.mine_difficulty_bits)
    }

    pub(super) fn update_mine_difficulty_cache_after_tip(&mut self) {
        let height = self.tip().height;
        if height == 0 || !height.is_multiple_of(MINE_RETARGET_WINDOW_BLOCKS) {
            return;
        }
        let expected_len = usize::try_from(height / MINE_RETARGET_WINDOW_BLOCKS)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        if self.mine_difficulty_windows.len() >= expected_len {
            return;
        }
        let mine_actions = self
            .chain
            .iter()
            .rev()
            .take(MINE_RETARGET_WINDOW_BLOCKS as usize)
            .map(mine_action_count)
            .sum();
        let previous = self
            .mine_difficulty_windows
            .last()
            .copied()
            .unwrap_or(self.launch_profile.mine_difficulty_bits);
        self.mine_difficulty_windows
            .push(retarget_mine_difficulty_bits(previous, mine_actions));
    }

    pub(super) fn tip(&self) -> &Block {
        self.chain
            .last()
            .expect("ledger is always initialized with genesis")
    }
}

fn mine_difficulty_windows_for_chain(
    chain: &[Block],
    initial_difficulty: u32,
    anchor_height: u64,
) -> Vec<u32> {
    let completed_windows = anchor_height / MINE_RETARGET_WINDOW_BLOCKS;
    let mut difficulties = Vec::with_capacity(
        usize::try_from(completed_windows)
            .unwrap_or_default()
            .saturating_add(1),
    );
    difficulties.push(initial_difficulty);
    let mut difficulty = initial_difficulty;
    for window in 1..=completed_windows {
        let window_end = window.saturating_mul(MINE_RETARGET_WINDOW_BLOCKS);
        let window_start = window_end + 1 - MINE_RETARGET_WINDOW_BLOCKS;
        let mine_actions = chain
            .iter()
            .filter(|block| window_start <= block.height && block.height <= window_end)
            .map(mine_action_count)
            .sum();
        difficulty = retarget_mine_difficulty_bits(difficulty, mine_actions);
        difficulties.push(difficulty);
    }
    difficulties
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{BurnBundleSection, Transaction};

    fn mine(anchor: &str, nonce: u64) -> Transaction {
        Transaction::Mine {
            recipient: "1".repeat(64),
            anchor: anchor.to_string(),
            salt: 1,
            nonce,
            difficulty_bits: 12,
            proof_header: None,
            signature: format!("{nonce:064x}"),
        }
    }

    #[test]
    fn applied_window_cache_preserves_retarget_results_for_constant_time_lookup() {
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        for height in 1..=20 {
            let parent = ledger.tip().clone();
            let mut block = parent.clone();
            block.height = height;
            block.prev_hash = parent.hash.clone();
            block.hash = format!("{height:064x}");
            block.burn_bundle_section = BurnBundleSection::default();
            block.transactions = if height <= 10 {
                vec![mine(&parent.hash, height)]
            } else {
                Vec::new()
            };
            ledger.chain.push(block);
            ledger.update_mine_difficulty_cache_after_tip();
        }

        assert_eq!(ledger.mine_difficulty_windows, vec![12, 12, 10]);
        assert_eq!(ledger.mine_difficulty_bits_for_anchor_height(9), 12);
        assert_eq!(ledger.mine_difficulty_bits_for_anchor_height(10), 12);
        assert_eq!(ledger.mine_difficulty_bits_for_anchor_height(20), 10);

        let mut uncached = ledger.clone();
        uncached.mine_difficulty_windows.truncate(1);
        assert_eq!(uncached.mine_difficulty_bits_for_anchor_height(20), 10);
    }
}
