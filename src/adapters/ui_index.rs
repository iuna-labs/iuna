use std::collections::BTreeMap;

use crate::domain::{
    Block, BurnLeaderRank, ChainSnapshot, Ledger, OutPoint, Transaction, TxOutput,
    genesis_allocation_outpoint, reward_outputs_for_block,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UiChainIndex {
    pub(crate) tip_hash: Option<String>,
    pub(crate) outputs: BTreeMap<OutPoint, TxOutput>,
    pub(crate) burn_leader_ranks_by_hash: BTreeMap<String, Vec<BurnLeaderRank>>,
}

pub(crate) fn build_ui_chain_index(snapshot: &ChainSnapshot) -> UiChainIndex {
    UiChainIndex {
        tip_hash: snapshot.blocks.last().map(|block| block.hash.clone()),
        outputs: known_chain_output_index(snapshot),
        burn_leader_ranks_by_hash: burn_leader_ranks_for_blocks(snapshot, &snapshot.blocks),
    }
}

pub(crate) fn burn_leader_ranks_for_blocks(
    snapshot: &ChainSnapshot,
    blocks: &[Block],
) -> BTreeMap<String, Vec<BurnLeaderRank>> {
    let Some(ranks_by_height) = Ledger::from_preverified_snapshot(snapshot.clone())
        .ok()
        .and_then(|ledger| {
            ledger
                .burn_leader_ranks_for_blocks(blocks.iter().map(|block| block.height))
                .ok()
        })
    else {
        return BTreeMap::new();
    };

    blocks
        .iter()
        .filter_map(|block| {
            ranks_by_height
                .get(&block.height)
                .cloned()
                .map(|ranks| (block.hash.clone(), ranks))
        })
        .collect()
}

fn known_chain_output_index(snapshot: &ChainSnapshot) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = BTreeMap::new();
    let mut running_ledger = snapshot.blocks.first().cloned().and_then(|genesis| {
        Ledger::from_preverified_snapshot(ChainSnapshot {
            genesis_allocations: snapshot.genesis_allocations.clone(),
            vdf_rounds: snapshot.vdf_rounds,
            launch_profile: snapshot.launch_profile.clone(),
            blocks: vec![genesis],
        })
        .ok()
    });
    for (address, amount) in &snapshot.genesis_allocations {
        if *amount == 0 {
            continue;
        }
        outputs.insert(
            genesis_allocation_outpoint(address),
            TxOutput {
                address: address.clone(),
                amount: *amount,
            },
        );
    }
    for block in &snapshot.blocks {
        for transaction in &block.transactions {
            index_transaction_outputs(&mut outputs, transaction);
        }
        let reward_committee = if block.height == 0 {
            Vec::new()
        } else {
            running_ledger
                .as_ref()
                .map(|ledger| ledger.burn_committee_for_block(block))
                .unwrap_or_default()
        };
        for (outpoint, output) in reward_outputs_for_block(block, &reward_committee) {
            outputs.insert(outpoint, output);
        }
        if block.height > 0 {
            if let Some(ledger) = running_ledger.as_mut() {
                let _ = ledger.apply_preverified_block_at(block.clone(), u64::MAX);
            }
        }
    }
    outputs
}

fn index_transaction_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &Transaction,
) {
    let tx_outputs = match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: crate::domain::MINE_REWARD,
        }],
    };
    for (index, output) in tx_outputs.into_iter().enumerate() {
        outputs.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::{ChainSnapshot, Ledger, MICRO_IUNA, Wallet};

    use super::{build_ui_chain_index, genesis_allocation_outpoint};

    #[test]
    fn ui_chain_index_keeps_genesis_allocation_outputs() {
        let wallet = Wallet::from_seed("ui-genesis-output-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), MICRO_IUNA);
        let ledger = Ledger::new(allocations.clone(), 1);
        let snapshot = ChainSnapshot {
            genesis_allocations: allocations,
            vdf_rounds: ledger.vdf_rounds(),
            launch_profile: ledger.launch_profile().clone(),
            blocks: ledger.chain().to_vec(),
        };

        let index = build_ui_chain_index(&snapshot);
        let outpoint = genesis_allocation_outpoint(wallet.address());

        assert_eq!(
            index.outputs.get(&outpoint).map(|output| output.amount),
            Some(MICRO_IUNA)
        );
    }
}
