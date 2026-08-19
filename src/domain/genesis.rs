use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::{
    Amount, BLOCK_REWARD, Block, BurnBundleSection, FinalizerMode, OutPoint, Transaction, TxOutput,
    apply_transaction, credit_reward_output, hex_hash, validate_genesis_burn_transaction,
};

pub(super) fn build_genesis_block(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: Vec<Transaction>,
) -> Block {
    let miner = genesis_miner(genesis_allocations, &transactions);
    let reward = genesis_reward(genesis_allocations, &transactions);
    let txs = transactions
        .iter()
        .map(Transaction::canonical)
        .collect::<Vec<_>>()
        .join("|");
    let vdf_output = hex_hash(format!("iuna-genesis-vdf:{genesis_allocations:?}:{txs}"));
    let mut genesis = Block {
        height: 0,
        prev_hash: "0".repeat(64),
        timestamp_ms: 0,
        miner,
        finalizer_mode: FinalizerMode::Ticket,
        finalizer_rank: 0,
        reward,
        vdf_rounds: 0,
        vdf_output,
        leader_proof: None,
        burn_bundle_section: BurnBundleSection::default(),
        transactions,
        hash: String::new(),
    };
    genesis.hash = genesis.compute_hash();
    genesis
}

pub(super) fn utxos_after_genesis(
    genesis_allocations: &BTreeMap<String, Amount>,
    genesis: &Block,
) -> Result<BTreeMap<OutPoint, TxOutput>> {
    let mut utxos = genesis_allocation_utxos(genesis_allocations);
    for transaction in &genesis.transactions {
        match transaction {
            Transaction::Burn { .. } => {
                validate_genesis_burn_transaction(transaction)?;
                apply_transaction(transaction, &mut utxos)?;
            }
            Transaction::Transfer { .. } | Transaction::Mine { .. } => {
                bail!("genesis only supports burn transactions")
            }
        }
    }
    credit_reward_output(&mut utxos, genesis)?;
    Ok(utxos)
}

fn genesis_allocation_utxos(
    genesis_allocations: &BTreeMap<String, Amount>,
) -> BTreeMap<OutPoint, TxOutput> {
    genesis_allocations
        .iter()
        .filter(|(_, amount)| **amount > 0)
        .map(|(address, amount)| {
            (
                genesis_allocation_outpoint(address),
                TxOutput {
                    address: address.clone(),
                    amount: *amount,
                },
            )
        })
        .collect()
}

pub(super) fn balances_from_utxos(
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> BTreeMap<String, Amount> {
    let mut balances = BTreeMap::new();
    for output in utxos.values() {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance.saturating_add(output.amount);
    }
    balances
}

pub(super) fn genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
        index: 0,
    }
}

pub(super) fn validate_genesis_block(block: &Block) -> Result<()> {
    if block.height != 0 {
        bail!("genesis block height must be 0");
    }
    if block.prev_hash != "0".repeat(64) {
        bail!("genesis block prev_hash must be all zeroes");
    }
    if block.timestamp_ms != 0 {
        bail!("genesis block timestamp must be 0");
    }
    if block.miner == "genesis" && block.reward != 0 {
        bail!("genesis placeholder miner must not receive a reward");
    }
    if block.miner != "genesis" && block.reward != 0 && block.reward != BLOCK_REWARD {
        bail!("genesis block reward is invalid");
    }
    if block.vdf_rounds != 0 {
        bail!("genesis block VDF rounds must be 0");
    }
    if block.leader_proof.is_some() {
        bail!("genesis block must not carry a leader proof");
    }
    if !block.burn_bundle_section.is_empty() {
        bail!("genesis block must not carry burn bundle attestations");
    }
    if block.compute_hash() != block.hash {
        bail!("genesis block hash is invalid");
    }
    Ok(())
}

fn genesis_miner(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: &[Transaction],
) -> String {
    transactions
        .iter()
        .filter_map(|transaction| match transaction {
            Transaction::Burn { inputs, .. } => inputs.first().map(|input| input.owner.as_str()),
            Transaction::Transfer { .. } | Transaction::Mine { .. } => None,
        })
        .find(|from| genesis_allocations.contains_key(*from))
        .or_else(|| genesis_allocations.keys().next().map(String::as_str))
        .unwrap_or("genesis")
        .to_string()
}

fn genesis_reward(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: &[Transaction],
) -> Amount {
    if genesis_allocations.is_empty() || transactions.is_empty() {
        0
    } else {
        BLOCK_REWARD
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{build_genesis_block, genesis_allocation_outpoint, validate_genesis_block};
    use crate::domain::{MICRO_IUNA, Transaction, Wallet};

    #[test]
    fn genesis_allocation_outpoint_is_address_bound() {
        let alice = Wallet::from_seed("genesis-outpoint-alice");
        let bob = Wallet::from_seed("genesis-outpoint-bob");

        assert_ne!(
            genesis_allocation_outpoint(alice.address()),
            genesis_allocation_outpoint(bob.address())
        );
        assert_eq!(genesis_allocation_outpoint(alice.address()).index, 0);
    }

    #[test]
    fn built_genesis_block_validates() {
        let wallet = Wallet::from_seed("genesis-module-validates");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), MICRO_IUNA);
        let burn = Transaction::genesis_burn(wallet.address(), MICRO_IUNA);

        let genesis = build_genesis_block(&allocations, vec![burn]);

        validate_genesis_block(&genesis).unwrap();
    }
}
