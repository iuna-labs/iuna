use anyhow::Result;

use super::mine_policy::{MINE_RETARGET_WINDOW_BLOCKS, retarget_mine_difficulty_bits};
use super::ticket::{
    BurnTicket, base_vdf_rounds_for_finalizer_rank, mine_action_count, ranked_tickets_for_height,
    vdf_rounds_for_finalizer_rank,
};
use super::vdf::{VDF_RETARGET_WINDOW_BLOCKS, retarget_vdf_rounds, vdf_retarget_observed_block_ms};
use super::{Block, FinalizerMode, Ledger};

impl Ledger {
    pub(super) fn next_vdf_rounds_after_tip(&self) -> u64 {
        let Some(tip) = self.chain.last() else {
            return self.vdf_rounds;
        };
        if tip.height < 2 {
            return self.vdf_rounds;
        }

        let mut total_observed_ms = 0_u128;
        let mut observed_blocks = 0_u128;
        for pair in self
            .chain
            .windows(2)
            .rev()
            .filter(|pair| pair[0].height > 0)
            .take(VDF_RETARGET_WINDOW_BLOCKS)
        {
            let Some(observed_ms) = vdf_retarget_observed_block_ms(&pair[0], &pair[1]) else {
                continue;
            };
            total_observed_ms += u128::from(observed_ms);
            observed_blocks += 1;
        }
        if observed_blocks == 0 {
            return self.vdf_rounds;
        }

        let average_observed_ms = (total_observed_ms / observed_blocks) as u64;
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
        let mut difficulty = self.launch_profile.mine_difficulty_bits;
        let mut window_end = MINE_RETARGET_WINDOW_BLOCKS;
        while window_end <= anchor_height {
            let window_start = window_end + 1 - MINE_RETARGET_WINDOW_BLOCKS;
            let mine_actions = self
                .chain
                .iter()
                .filter(|block| window_start <= block.height && block.height <= window_end)
                .map(mine_action_count)
                .sum::<u64>();
            difficulty = retarget_mine_difficulty_bits(difficulty, mine_actions);
            window_end = window_end.saturating_add(MINE_RETARGET_WINDOW_BLOCKS);
        }
        difficulty
    }

    pub(super) fn tip(&self) -> &Block {
        self.chain
            .last()
            .expect("ledger is always initialized with genesis")
    }
}
