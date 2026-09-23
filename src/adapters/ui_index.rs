use std::collections::BTreeMap;

use crate::domain::{
    AddressNetwork, Block, BurnCommitteeMember, BurnLeaderRank, ChainSnapshot, Ledger, OutPoint,
    Transaction, TransactionV2, TxOutput, decode_hex, encode_versioned_address,
    genesis_allocation_outpoint, hex_encode, reward_outputs_for_block,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct UiChainIndex {
    pub(crate) tip_hash: Option<String>,
    pub(crate) outputs: BTreeMap<OutPoint, TxOutput>,
    pub(crate) burn_leader_ranks_by_hash: BTreeMap<String, Vec<BurnLeaderRank>>,
}

#[cfg(test)]
pub(crate) fn build_ui_chain_index(snapshot: &ChainSnapshot) -> UiChainIndex {
    let Ok(ledger) = Ledger::from_preverified_snapshot(snapshot.clone()) else {
        return UiChainIndex::default();
    };
    build_ui_chain_index_for_blocks(snapshot, &ledger, &snapshot.blocks, true)
}

pub(crate) fn build_ui_chain_index_for_blocks(
    snapshot: &ChainSnapshot,
    ledger: &Ledger,
    blocks: &[Block],
    include_genesis_allocations: bool,
) -> UiChainIndex {
    UiChainIndex {
        tip_hash: snapshot.blocks.last().map(|block| block.hash.clone()),
        outputs: known_chain_output_index(snapshot, blocks, include_genesis_allocations),
        burn_leader_ranks_by_hash: burn_leader_ranks_for_blocks(ledger, blocks),
    }
}

pub(crate) fn burn_leader_ranks_for_blocks(
    ledger: &Ledger,
    blocks: &[Block],
) -> BTreeMap<String, Vec<BurnLeaderRank>> {
    let Ok(ranks_by_height) =
        ledger.burn_leader_ranks_for_blocks(blocks.iter().map(|block| block.height))
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

fn known_chain_output_index(
    snapshot: &ChainSnapshot,
    blocks: &[Block],
    include_genesis_allocations: bool,
) -> BTreeMap<OutPoint, TxOutput> {
    let mut outputs = BTreeMap::new();
    let network = AddressNetwork::from_profile_id(&snapshot.launch_profile.profile_id);
    if include_genesis_allocations {
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
    }
    for block in blocks {
        for transaction in &block.transactions {
            index_transaction_outputs(&mut outputs, transaction);
        }
        for envelope in &block.transactions_v2 {
            let Ok(bytes) = decode_hex(envelope) else {
                continue;
            };
            let Ok((domain, transaction)) = TransactionV2::decode(&bytes) else {
                continue;
            };
            index_transaction_v2_outputs(&mut outputs, &transaction, &domain, network);
        }
        for (outpoint, output) in projected_reward_outputs(block) {
            outputs.insert(outpoint, output);
        }
    }
    outputs
}

pub(crate) fn projected_reward_outputs(block: &Block) -> Vec<(OutPoint, TxOutput)> {
    let committee = block
        .burn_bundle_section
        .signatures
        .iter()
        .map(|signature| BurnCommitteeMember {
            slot: signature.slot,
            root: block.hash.clone(),
            owner: signature.member.clone(),
            weight: 0,
        })
        .collect::<Vec<_>>();
    reward_outputs_for_block(block, &committee)
}

fn index_transaction_v2_outputs(
    outputs: &mut BTreeMap<OutPoint, TxOutput>,
    transaction: &TransactionV2,
    domain: &crate::domain::TransactionV2Domain,
    network: AddressNetwork,
) {
    let transaction_outputs = match transaction {
        TransactionV2::Migration { outputs, .. } | TransactionV2::Transfer { outputs, .. } => {
            outputs.as_slice()
        }
        TransactionV2::Burn { change, .. } => change.as_slice(),
        TransactionV2::Mine { .. } => return,
    };
    let Ok(transaction_id) = transaction.transaction_id(domain).map(hex_encode) else {
        return;
    };
    for (index, output) in transaction_outputs.iter().enumerate() {
        let Ok(address) = encode_versioned_address(output.address, network) else {
            continue;
        };
        let Ok(index) = u32::try_from(index) else {
            continue;
        };
        outputs.insert(
            OutPoint {
                txid: transaction_id.clone(),
                index,
            },
            TxOutput {
                address,
                amount: output.amount,
            },
        );
    }
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
