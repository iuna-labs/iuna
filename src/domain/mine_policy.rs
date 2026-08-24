use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::{Block, MINE_ACTIONS_PER_ANCHOR_LIMIT, Transaction};

pub(crate) const MINE_RETARGET_WINDOW_BLOCKS: u64 = 10;
pub(super) const MINE_MAX_RETARGET_STEP_BITS: u32 = 2;
pub(super) const MINE_MIN_DIFFICULTY_BITS: u32 = 10;
pub(super) const MINE_MAX_ANCHOR_AGE_BLOCKS: u64 = MINE_RETARGET_WINDOW_BLOCKS;

const MINE_TARGET_ACTIONS_PER_BLOCK: u64 = 1;

pub(crate) fn retarget_mine_difficulty_bits(current: u32, mine_actions: u64) -> u32 {
    let target = MINE_RETARGET_WINDOW_BLOCKS.saturating_mul(MINE_TARGET_ACTIONS_PER_BLOCK);
    if target == 0 || mine_actions == target {
        return current.max(MINE_MIN_DIFFICULTY_BITS);
    }

    let step = if mine_actions > target {
        floor_log2_ratio(mine_actions, target).min(MINE_MAX_RETARGET_STEP_BITS)
    } else if mine_actions == 0 {
        MINE_MAX_RETARGET_STEP_BITS
    } else {
        floor_log2_ratio(target, mine_actions).min(MINE_MAX_RETARGET_STEP_BITS)
    };

    if step == 0 {
        return current.max(MINE_MIN_DIFFICULTY_BITS);
    }
    if mine_actions > target {
        current.saturating_add(step).max(MINE_MIN_DIFFICULTY_BITS)
    } else {
        current.saturating_sub(step).max(MINE_MIN_DIFFICULTY_BITS)
    }
}

fn floor_log2_ratio(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 || numerator <= denominator {
        return 0;
    }
    let mut step = 0_u32;
    let mut threshold = denominator;
    while threshold <= numerator / 2 {
        threshold = threshold.saturating_mul(2);
        step = step.saturating_add(1);
    }
    step
}

pub(super) fn ensure_mine_anchor_limit(_height: u64, transactions: &[Transaction]) -> Result<()> {
    let mut anchor_counts = BTreeMap::new();
    for transaction in transactions {
        let Some(anchor) = mine_anchor(transaction) else {
            continue;
        };
        let count = anchor_counts.entry(anchor).or_insert(0usize);
        *count += 1;
        if *count > MINE_ACTIONS_PER_ANCHOR_LIMIT {
            bail!("block exceeds mine actions per anchor limit");
        }
    }
    Ok(())
}

pub(super) fn mine_anchor(transaction: &Transaction) -> Option<&str> {
    match transaction {
        Transaction::Mine { anchor, .. } => Some(anchor.as_str()),
        _ => None,
    }
}

pub(super) fn mine_anchor_count_before_height(chain: &[Block], anchor: &str, height: u64) -> usize {
    chain
        .iter()
        .take_while(|block| block.height <= height)
        .map(|block| {
            block
                .transactions
                .iter()
                .filter(|transaction| mine_anchor(transaction) == Some(anchor))
                .count()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MINE_DIFFICULTY_BITS;

    #[test]
    fn retarget_keeps_protocol_minimum() {
        assert_eq!(
            retarget_mine_difficulty_bits(MINE_DIFFICULTY_BITS, 0),
            MINE_DIFFICULTY_BITS - MINE_MAX_RETARGET_STEP_BITS
        );
        assert_eq!(
            retarget_mine_difficulty_bits(1, 0),
            MINE_MIN_DIFFICULTY_BITS
        );
    }

    #[test]
    fn retarget_does_not_cap_difficulty_at_32_bits() {
        assert_eq!(retarget_mine_difficulty_bits(33, 20), 34);
        assert_eq!(retarget_mine_difficulty_bits(40, 10), 40);
    }
}
