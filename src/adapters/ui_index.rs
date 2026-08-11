use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    Amount, BLINDED_COMMITTER_FEE_BPS, BLINDED_FEE_BPS_DENOMINATOR,
    BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS, BlindedTransaction, Block, BurnLeaderRank, ChainSnapshot,
    Ledger, MINE_REWARD, OutPoint, REVEAL_COMMITTEE_SIZE, RevealedBlindedTransaction, Transaction,
    TxOutput, blinded_reveal_finalizer_fee, hex_hash, reveal_committee_slot_count,
    revealed_blinded_transactions,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UiChainIndex {
    pub(crate) tip_hash: Option<String>,
    pub(crate) outputs: BTreeMap<OutPoint, TxOutput>,
    pub(crate) revealed_by_height: BTreeMap<u64, Vec<RevealedBlindedTransaction>>,
    pub(crate) burn_leader_ranks_by_hash: BTreeMap<String, Vec<BurnLeaderRank>>,
}

pub(crate) fn build_ui_chain_index(snapshot: &ChainSnapshot) -> UiChainIndex {
    UiChainIndex {
        tip_hash: snapshot.blocks.last().map(|block| block.hash.clone()),
        outputs: known_chain_output_index(snapshot),
        revealed_by_height: revealed_transactions_by_height(snapshot),
        burn_leader_ranks_by_hash: burn_leader_ranks_for_blocks(snapshot, &snapshot.blocks),
    }
}

pub(crate) fn revealed_transactions_by_height(
    snapshot: &ChainSnapshot,
) -> BTreeMap<u64, Vec<RevealedBlindedTransaction>> {
    revealed_blinded_transactions(snapshot)
        .unwrap_or_default()
        .into_iter()
        .fold(
            BTreeMap::<u64, Vec<RevealedBlindedTransaction>>::new(),
            |mut by_height, revealed| {
                by_height.entry(revealed.height).or_default().push(revealed);
                by_height
            },
        )
}

pub(crate) fn burn_leader_ranks_for_blocks(
    snapshot: &ChainSnapshot,
    blocks: &[Block],
) -> BTreeMap<String, Vec<BurnLeaderRank>> {
    let Some(ranks_by_height) = Ledger::from_persisted_snapshot(snapshot.clone())
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
    let revealed = revealed_blinded_transactions(snapshot).unwrap_or_default();
    let blocks_by_height = snapshot
        .blocks
        .iter()
        .map(|block| (block.height, block))
        .collect::<BTreeMap<_, _>>();
    let reveal_bundle_slots_by_height = reveal_bundle_slots_by_height(snapshot);
    let blinded_by_commitment = snapshot
        .blocks
        .iter()
        .flat_map(|block| block.blinded_transactions.iter())
        .map(|transaction| (transaction.commitment.clone(), transaction.clone()))
        .collect::<BTreeMap<_, _>>();
    for block in &snapshot.blocks {
        for transaction in &block.transactions {
            index_transaction_outputs(&mut outputs, transaction);
        }
        if block.reward > 0 {
            outputs.insert(
                reward_outpoint(&block.hash),
                TxOutput {
                    address: block.miner.clone(),
                    amount: block.reward,
                },
            );
        }
    }
    for revealed in revealed {
        index_transaction_outputs(&mut outputs, &revealed.transaction);
        let fee = revealed.transaction.fee();
        if matches!(revealed.transaction, Transaction::Mine { .. }) {
            if let Some(commit) = blinded_by_commitment.get(&revealed.commitment) {
                index_blinded_collateral_change(&mut outputs, commit, fee);
            }
        }
        if fee > 0 {
            let committer_fee = blinded_fee_share(fee, BLINDED_COMMITTER_FEE_BPS);
            if committer_fee > 0 {
                outputs.insert(
                    blinded_committer_fee_outpoint(&revealed.commitment),
                    TxOutput {
                        address: revealed.included_by,
                        amount: committer_fee,
                    },
                );
            }
            if let Some(block) = blocks_by_height.get(&revealed.height) {
                let reveal_finalizer_fee = blinded_reveal_finalizer_fee(
                    fee,
                    block.included_reveal_bundle_count(),
                    reveal_bundle_slots_by_height
                        .get(&revealed.height)
                        .copied()
                        .unwrap_or(REVEAL_COMMITTEE_SIZE),
                );
                if reveal_finalizer_fee > 0 {
                    outputs.insert(
                        blinded_executor_fee_outpoint(&revealed.commitment),
                        TxOutput {
                            address: block.miner.clone(),
                            amount: reveal_finalizer_fee,
                        },
                    );
                }
                let reveal_bundle_signer_fee =
                    blinded_fee_share(fee, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS);
                if reveal_bundle_signer_fee > 0 {
                    for signature in &block.reveal_bundle_section.signatures {
                        outputs.insert(
                            blinded_reveal_bundle_signer_fee_outpoint(
                                &revealed.commitment,
                                signature.slot,
                            ),
                            TxOutput {
                                address: signature.member.clone(),
                                amount: reveal_bundle_signer_fee,
                            },
                        );
                    }
                }
            }
        }
    }
    index_expired_blinded_outputs(&mut outputs, snapshot);
    outputs
}

fn reveal_bundle_slots_by_height(snapshot: &ChainSnapshot) -> BTreeMap<u64, usize> {
    Ledger::from_persisted_snapshot(snapshot.clone())
        .ok()
        .and_then(|ledger| {
            ledger
                .burn_leader_ranks_for_blocks(snapshot.blocks.iter().map(|block| block.height))
                .ok()
        })
        .map(|ranks_by_height| {
            ranks_by_height
                .into_iter()
                .map(|(height, ranks)| (height, reveal_committee_slot_count(ranks.len())))
                .collect()
        })
        .unwrap_or_default()
}

fn index_blinded_collateral_change(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &BlindedTransaction,
    fee: Amount,
) {
    let Some(first_input) = transaction.inputs.first() else {
        return;
    };
    let locked_total = transaction.inputs.iter().fold(0_u64, |total, input| {
        total.saturating_add(
            outputs
                .get(&input.outpoint)
                .map(|output| output.amount)
                .unwrap_or_default(),
        )
    });
    if fee >= locked_total {
        return;
    }
    outputs.insert(
        blinded_expiry_change_outpoint(&transaction.commitment),
        TxOutput {
            address: first_input.owner.clone(),
            amount: locked_total - fee,
        },
    );
}

fn index_expired_blinded_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    snapshot: &ChainSnapshot,
) {
    let mut active = BTreeMap::<String, (BlindedTransaction, Amount)>::new();
    for block in &snapshot.blocks {
        let revealed = block
            .all_blinded_reveals()
            .into_iter()
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();
        active.retain(|commitment, (transaction, locked_total)| {
            if revealed.contains(commitment) {
                return false;
            }
            if block.height >= transaction.expires_at_height {
                if let Some(first_input) = transaction.inputs.first() {
                    if transaction.fee <= *locked_total {
                        let change = *locked_total - transaction.fee;
                        if change > 0 {
                            outputs.insert(
                                blinded_expiry_change_outpoint(commitment),
                                TxOutput {
                                    address: first_input.owner.clone(),
                                    amount: change,
                                },
                            );
                        }
                    }
                }
                return false;
            }
            true
        });
        for transaction in &block.blinded_transactions {
            let locked_total = transaction.inputs.iter().fold(0_u64, |total, input| {
                total.saturating_add(
                    outputs
                        .get(&input.outpoint)
                        .map(|output| output.amount)
                        .unwrap_or_default(),
                )
            });
            active.insert(
                transaction.commitment.clone(),
                (transaction.clone(), locked_total),
            );
        }
    }
}

fn index_transaction_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &Transaction,
) {
    let created_outputs = match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        }],
    };
    for (index, output) in created_outputs.iter().enumerate() {
        outputs.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
}

fn genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
}

fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
}

fn blinded_committer_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

fn blinded_executor_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 2,
    }
}

fn blinded_reveal_bundle_signer_fee_outpoint(commitment: &str, slot: u8) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 3 - u32::from(slot),
    }
}

fn blinded_expiry_change_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: 0,
    }
}

fn blinded_fee_share(fee: Amount, bps: u64) -> Amount {
    ((fee as u128 * bps as u128) / BLINDED_FEE_BPS_DENOMINATOR as u128) as Amount
}
