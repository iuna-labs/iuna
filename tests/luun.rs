use std::collections::BTreeMap;

use luun::{
    adapters::chain_store::SqliteChainStore,
    app::{DEFAULT_BURN_PER_BLOCK, InMemoryNetwork, NodeConfig, NodeCore, PeerBook, PeerDirection},
    domain::{
        Amount, BLOCK_REWARD, GenesisBurn, Ledger, MAX_BLOCK_BYTES, MICRO_LUUN, MINE_REWARD,
        VDF_TARGET_BLOCK_MS, Wallet, run_vdf, verify_vdf,
    },
};
use tempfile::tempdir;

fn luun(amount: Amount) -> Amount {
    amount * MICRO_LUUN
}

fn node(_network_key: &str, wallet: Wallet, allocations: BTreeMap<String, Amount>) -> NodeCore {
    NodeCore::new(NodeConfig {
        wallet,
        genesis_allocations: allocations,
        vdf_rounds: 25,
        burn_per_block: DEFAULT_BURN_PER_BLOCK,
        burn_fee: 1,
    })
}

fn wallets(names: &[&str]) -> Vec<Wallet> {
    names.iter().map(|name| Wallet::from_seed(name)).collect()
}

fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
    wallets
        .iter()
        .map(|wallet| (wallet.address().to_string(), amount))
        .collect()
}

fn mine_wallet_burn_block(ledger: &mut Ledger, wallet: &Wallet, timestamp_ms: u64) -> String {
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(wallet, timestamp_ms).unwrap();
    let hash = block.hash.clone();
    ledger.apply_block(block).unwrap();
    hash
}

fn submit_burn(ledger: &mut Ledger, wallet: &Wallet, amount: Amount) {
    let tx = ledger.build_burn(wallet, amount, 0).unwrap();
    ledger.submit_transaction(tx).unwrap();
}

fn burn_tx(ledger: &Ledger, wallet: &Wallet, amount: Amount) -> luun::domain::Transaction {
    ledger.build_burn(wallet, amount, 0).unwrap()
}

fn transfer_tx(
    ledger: &Ledger,
    wallet: &Wallet,
    to: impl Into<String>,
    amount: Amount,
) -> luun::domain::Transaction {
    ledger.build_transfer(wallet, to, amount, 0).unwrap()
}

fn transfer_fee_tx(
    ledger: &Ledger,
    wallet: &Wallet,
    to: impl Into<String>,
    amount: Amount,
    fee: Amount,
) -> luun::domain::Transaction {
    ledger.build_transfer(wallet, to, amount, fee).unwrap()
}

fn fork_with_better_vrf_block(
    base: &Ledger,
    wallet: &Wallet,
    local_fork_block_hash: &str,
    first_timestamp_ms: u64,
) -> Option<Ledger> {
    for offset in 0..10_000 {
        let mut candidate = base.clone();
        let hash = mine_wallet_burn_block(&mut candidate, wallet, first_timestamp_ms + offset);
        if hash.as_str() < local_fork_block_hash {
            return Some(candidate);
        }
    }
    None
}

fn fork_with_worse_vrf_block(
    base: &Ledger,
    wallet: &Wallet,
    local_fork_block_hash: &str,
    first_timestamp_ms: u64,
) -> Option<Ledger> {
    for offset in 0..10_000 {
        let mut candidate = base.clone();
        let hash = mine_wallet_burn_block(&mut candidate, wallet, first_timestamp_ms + offset);
        if hash.as_str() > local_fork_block_hash {
            return Some(candidate);
        }
    }
    None
}

fn starter_node(wallet: Wallet) -> NodeCore {
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 25)
            .unwrap();
    NodeCore::from_ledger_with_burn_fee_and_enabled(
        wallet,
        ledger,
        true,
        DEFAULT_BURN_PER_BLOCK,
        MICRO_LUUN,
    )
}

#[test]
fn genesis_burn_starts_chain_with_reward_and_first_leader() {
    let alice = Wallet::from_seed("alice");
    let node = starter_node(alice.clone());

    let genesis = &node.ledger().chain()[0];
    assert_eq!(node.ledger().balance_of(alice.address()), BLOCK_REWARD);
    assert_eq!(genesis.height, 0);
    assert_eq!(genesis.miner, alice.address());
    assert_eq!(genesis.reward, BLOCK_REWARD);
    assert_eq!(genesis.transactions.len(), 1);
    assert!(genesis.transactions[0].is_burn());
    assert_eq!(genesis.transactions[0].amount(), 1);
    assert_eq!(
        node.ledger().expected_leader_for_next_block().as_deref(),
        Some(alice.address())
    );
}

#[test]
fn burn_amount_weights_leader_selection() {
    let mut high_weight_leaders = 0;
    for sample in 0..100 {
        let low = Wallet::from_seed(&format!("weighted-low-{sample}"));
        let high = Wallet::from_seed(&format!("weighted-high-{sample}"));
        let mut allocations = BTreeMap::new();
        allocations.insert(low.address().to_string(), 1_000);
        allocations.insert(high.address().to_string(), 1_000);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![
                GenesisBurn::new(low.address(), 1),
                GenesisBurn::new(high.address(), 99),
            ],
            25,
        )
        .unwrap();

        if ledger.expected_leader_for_next_block().as_deref() == Some(high.address()) {
            high_weight_leaders += 1;
        }
    }

    assert!(
        high_weight_leaders >= 90,
        "high burn ticket should win most weighted draws, won {high_weight_leaders}/100"
    );
}

#[test]
fn starter_node_waits_for_a_burn_before_vdf_work() {
    let alice = Wallet::from_seed("alice");
    let mut node = starter_node(alice.clone());

    let outcome = node.automatic_mine_once(1);
    assert!(outcome.burned.is_none());
    assert!(outcome.block.is_none());
    assert!(
        outcome
            .skipped_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("at least one burn"))
    );
    assert_eq!(node.ledger().status().height, 0);
    assert_eq!(node.ledger().balance_of(alice.address()), BLOCK_REWARD);
}

#[test]
fn burn_in_block_creates_ticket_after_maturity_delay() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);

    let mut ledger = Ledger::new(allocations, 10);
    submit_burn(&mut ledger, &bob, 80);

    let launch_leader =
        if ledger.expected_leader_for_next_block().as_deref() == Some(alice.address()) {
            &alice
        } else {
            &bob
        };
    let first = ledger.mine_next_block(launch_leader, 1).unwrap();
    ledger.apply_block(first).unwrap();

    for height in 2..=3 {
        let leader_wallet =
            if ledger.expected_leader_for_next_block().as_deref() == Some(alice.address()) {
                &alice
            } else {
                &bob
            };
        submit_burn(&mut ledger, leader_wallet, 1);
        let block = ledger.mine_next_block(leader_wallet, height).unwrap();
        ledger.apply_block(block).unwrap();
    }

    assert_eq!(
        ledger.expected_leader_for_next_block().as_deref(),
        Some(bob.address())
    );

    assert!(
        ledger.mine_next_block(&alice, 4).is_err(),
        "the block 1 burn should not be eligible before its maturity delay, and only Bob should hold it at block 4"
    );

    submit_burn(&mut ledger, &bob, 1);
    assert!(ledger.mine_next_block(&bob, 4).is_ok());
}

#[test]
fn transfer_and_burn_update_balances_when_block_is_applied() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), luun(1_000));
    allocations.insert(bob.address().to_string(), luun(100));

    let mut ledger = Ledger::new(allocations, 10);
    let transfer = transfer_tx(&ledger, &alice, bob.address(), luun(125));
    ledger.submit_transaction(transfer).unwrap();
    let burn = burn_tx(&ledger, &alice, luun(25));
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(&alice, 1).unwrap();
    ledger.apply_block(block).unwrap();

    assert_eq!(ledger.balance_of(alice.address()), luun(850));
    assert_eq!(ledger.balance_of(bob.address()), luun(225));
}

#[test]
fn forged_transaction_is_rejected() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);
    let mut ledger = Ledger::new(allocations, 10);

    let mut forged = burn_tx(&ledger, &bob, 10);
    if let luun::domain::Transaction::Burn { inputs, .. } = &mut forged {
        inputs[0].owner = alice.address().to_string();
    }

    let error = ledger.submit_transaction(forged).unwrap_err();
    assert!(error.to_string().contains("signature"));
}

#[test]
fn block_with_forged_transaction_is_rejected() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    let mut ledger = Ledger::new(allocations, 10);
    submit_burn(&mut ledger, &alice, 1);

    let mut block = ledger.mine_next_block(&alice, 1).unwrap();
    if let luun::domain::Transaction::Burn { signature, .. } = &mut block.transactions[0] {
        signature.push_str("00");
    }
    block.vdf_output = run_vdf(&block.vdf_seed(), block.vdf_rounds);
    block.hash = block.compute_hash();

    let error = ledger.apply_block(block).unwrap_err();
    assert!(error.to_string().contains("signature"));
}

#[test]
fn mine_action_introduces_one_luun_and_block_author_gets_fees_only() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 2 * MICRO_LUUN);

    let mut ledger = Ledger::new(allocations, 10);
    let mine = ledger.build_mine(alice.address()).unwrap();
    assert_eq!(mine.amount(), MINE_REWARD);
    ledger.submit_transaction(mine.clone()).unwrap();
    submit_burn(&mut ledger, &alice, MICRO_LUUN);
    let block = ledger.mine_next_block(&alice, 1).unwrap();
    assert_eq!(block.reward, 0);

    ledger.apply_block(block).unwrap();
    assert_eq!(ledger.balance_of(alice.address()), 2 * MICRO_LUUN);
    assert!(!ledger.submit_transaction(mine).unwrap());
    assert_eq!(ledger.balance_of(alice.address()), 2 * MICRO_LUUN);
}

#[test]
fn mine_action_fee_is_chosen_by_pow_miner_and_paid_to_block_finalizer() {
    let alice = Wallet::from_seed("mine-fee-alice");
    let bob = Wallet::from_seed("mine-fee-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(bob.address().to_string(), 2 * MICRO_LUUN);

    let mut ledger = Ledger::new(allocations, 10);
    let mine_fee = MICRO_LUUN / 4;
    let mine = ledger
        .build_mine_with_fee(alice.address(), mine_fee)
        .unwrap();
    assert_eq!(mine.amount(), MINE_REWARD - mine_fee);
    assert_eq!(mine.fee(), mine_fee);
    ledger.submit_transaction(mine).unwrap();
    submit_burn(&mut ledger, &bob, MICRO_LUUN);

    let block = ledger.mine_next_block(&bob, 1).unwrap();
    assert_eq!(block.reward, mine_fee);
    ledger.apply_block(block).unwrap();

    assert_eq!(ledger.balance_of(alice.address()), MINE_REWARD - mine_fee);
    assert_eq!(ledger.balance_of(bob.address()), MICRO_LUUN + mine_fee);
}

#[test]
fn forged_mine_action_cannot_introduce_luun() {
    let alice = Wallet::from_seed("forged-mine-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), MICRO_LUUN);
    let mut ledger = Ledger::new(allocations, 10);

    let mut forged = ledger.build_mine(alice.address()).unwrap();
    if let luun::domain::Transaction::Mine { nonce, .. } = &mut forged {
        *nonce += 1;
    }

    let error = ledger.submit_transaction(forged).unwrap_err();
    assert!(format!("{error:#}").contains("mine transaction proof hash is invalid"));
    assert_eq!(ledger.balance_of(alice.address()), MICRO_LUUN);
}

#[test]
fn burn_larger_than_mine_reward_is_paid_from_existing_utxos() {
    let alice = Wallet::from_seed("large-burn-existing-utxos-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), BLOCK_REWARD + luun(50));

    let mut ledger = Ledger::new(allocations, 10);
    let burn_amount = BLOCK_REWARD + luun(25);
    let burn = ledger.build_burn(&alice, burn_amount, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();

    let block = ledger.mine_next_block(&alice, 1).unwrap();
    assert_eq!(block.transactions[0].amount(), burn_amount);
    assert_eq!(block.reward, 0);

    ledger.apply_block(block).unwrap();
    assert_eq!(ledger.balance_of(alice.address()), luun(25));
}

#[test]
fn burn_larger_than_existing_balance_cannot_use_next_block_reward() {
    let alice = Wallet::from_seed("large-burn-cannot-use-next-reward-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), BLOCK_REWARD);

    let ledger = Ledger::new(allocations, 10);
    let error = ledger
        .build_burn(&alice, BLOCK_REWARD + MICRO_LUUN, 0)
        .unwrap_err();

    assert!(format!("{error:#}").contains("insufficient funds"));
}

#[test]
fn transaction_fees_are_paid_to_the_block_miner() {
    let alice = Wallet::from_seed("fee-miner-alice");
    let bob = Wallet::from_seed("fee-payer-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), MICRO_LUUN);
    allocations.insert(bob.address().to_string(), luun(200));

    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(alice.address(), MICRO_LUUN)],
        10,
    )
    .unwrap();
    submit_burn(&mut ledger, &alice, MICRO_LUUN);
    let tx = transfer_fee_tx(&ledger, &bob, alice.address(), luun(10), luun(7));
    ledger.submit_transaction(tx).unwrap();

    let block = ledger.mine_next_block(&alice, 1).unwrap();
    assert_eq!(block.reward, luun(7));
    ledger.apply_block(block).unwrap();

    assert_eq!(ledger.balance_of(alice.address()), luun(116));
    assert_eq!(ledger.balance_of(bob.address()), luun(183));
}

#[test]
fn miner_orders_block_transactions_by_fee_rate_after_required_burn() {
    let wallets = wallets(&["fee-order-alice", "fee-order-bob", "fee-order-carol"]);
    let alice = &wallets[0];
    let bob = &wallets[1];
    let carol = &wallets[2];
    let mut allocations = allocations(&wallets, 1_000);
    allocations.insert(alice.address().to_string(), 1);
    let mut ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    let required_burn = burn_tx(&ledger, alice, 1);
    let low_fee = transfer_fee_tx(&ledger, bob, alice.address(), 1, 1);
    let high_fee = transfer_fee_tx(&ledger, carol, alice.address(), 1, 20);
    ledger.submit_transaction(low_fee.clone()).unwrap();
    ledger.submit_transaction(high_fee.clone()).unwrap();
    ledger.submit_transaction(required_burn.clone()).unwrap();

    let block = ledger.mine_next_block(alice, 1).unwrap();
    let signatures = block
        .transactions
        .iter()
        .map(|tx| tx.signature().to_string())
        .collect::<Vec<_>>();

    assert_eq!(signatures[0], required_burn.signature());
    assert!(
        signatures
            .iter()
            .position(|signature| signature == high_fee.signature())
            < signatures
                .iter()
                .position(|signature| signature == low_fee.signature())
    );
}

#[test]
fn oversized_blocks_are_rejected() {
    let alice = Wallet::from_seed("oversized-block-alice");
    let bob = Wallet::from_seed("oversized-block-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1);
    allocations.insert(bob.address().to_string(), 1_000);
    let mut ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    submit_burn(&mut ledger, &alice, 1);
    let mut block = ledger.mine_next_block(&alice, 1).unwrap();
    let oversized_transfer = transfer_fee_tx(&ledger, &bob, "x".repeat(MAX_BLOCK_BYTES), 1, 3);
    block.transactions.push(oversized_transfer);
    block.reward = 3;
    block.hash = block.compute_hash();

    let error = ledger.apply_block(block).unwrap_err();

    assert!(format!("{error:#}").contains("max block size"));
}

#[test]
fn miner_skips_oversized_pending_transaction_and_keeps_fitting_fee_transaction() {
    let wallets = wallets(&[
        "oversized-select-alice",
        "oversized-select-bob",
        "oversized-select-carol",
    ]);
    let alice = &wallets[0];
    let bob = &wallets[1];
    let carol = &wallets[2];
    let mut allocations = allocations(&wallets, 200_000);
    allocations.insert(alice.address().to_string(), 1);
    let mut ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    submit_burn(&mut ledger, alice, 1);
    let oversized = transfer_fee_tx(&ledger, bob, "x".repeat(MAX_BLOCK_BYTES), 1, 100_000);
    let fitting = transfer_fee_tx(&ledger, carol, alice.address(), 1, 5);
    ledger.submit_transaction(oversized.clone()).unwrap();
    ledger.submit_transaction(fitting.clone()).unwrap();

    let block = ledger.mine_next_block(alice, 1).unwrap();
    let signatures = block
        .transactions
        .iter()
        .map(|tx| tx.signature().to_string())
        .collect::<Vec<_>>();

    assert!(!signatures.contains(&oversized.signature().to_string()));
    assert!(signatures.contains(&fitting.signature().to_string()));
    assert!(block.serialized_size_bytes().unwrap() <= MAX_BLOCK_BYTES);
}

#[test]
fn transfer_that_would_overflow_recipient_balance_is_rejected() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1);
    allocations.insert(bob.address().to_string(), Amount::MAX);

    let ledger = Ledger::new(allocations, 10);
    let error = ledger
        .build_transfer(&alice, bob.address(), 1, 0)
        .unwrap_err();

    assert!(format!("{error:#}").contains("balance overflow"));
}

#[test]
fn mine_reward_that_would_overflow_recipient_balance_is_rejected() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), Amount::MAX);

    let ledger = Ledger::new(allocations, 10);
    let error = ledger.build_mine(alice.address()).unwrap_err();

    assert!(format!("{error:#}").contains("balance overflow"));
}

#[test]
fn block_without_mature_ticket_cannot_be_mined() {
    let alice = Wallet::from_seed("alice");
    let allocations = BTreeMap::new();

    let ledger = Ledger::new(allocations, 10);
    let error = ledger.mine_next_block(&alice, 1).unwrap_err();
    assert!(error.to_string().contains("mature burn ticket"));
}

#[test]
fn vdf_work_requires_at_least_one_pending_burn() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);

    let ledger = Ledger::new(allocations, 10);
    let error = ledger.prepare_next_block(alice.address(), 1).unwrap_err();

    assert!(format!("{error:#}").contains("at least one burn"));
}

#[test]
fn leader_block_without_burn_is_rejected() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);

    let mut ledger = Ledger::new(allocations, 10);
    submit_burn(&mut ledger, &alice, 1);
    let mut block = ledger.mine_next_block(&alice, 1).unwrap();
    block.transactions.clear();
    block.hash = block.compute_hash();

    let error = ledger.apply_block(block).unwrap_err();
    assert!(format!("{error:#}").contains("at least one burn"));
}

#[test]
fn block_hash_is_bound_to_block_contents() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);

    let mut ledger = Ledger::new(allocations, 10);
    submit_burn(&mut ledger, &alice, 1);
    let mut block = ledger.mine_next_block(&alice, 1).unwrap();
    block.timestamp_ms += 1;

    let error = ledger.apply_block(block).unwrap_err();
    assert!(error.to_string().contains("block hash is invalid"));
}

#[test]
fn automatic_mining_burns_configured_amount_once_per_height() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), luun(1_000));

    let mut node = NodeCore::new(NodeConfig {
        wallet: alice.clone(),
        genesis_allocations: allocations,
        vdf_rounds: 10,
        burn_per_block: luun(25),
        burn_fee: MICRO_LUUN,
    });

    let first = node.automatic_mine_once(1);
    assert!(first.burned.is_some());
    assert!(first.block.is_some());
    assert_eq!(node.ledger().chain().len(), 2);
    assert_eq!(node.ledger().balance_of(alice.address()), luun(975));

    let second = node.automatic_mine_once(2);
    assert!(second.burned.is_some());
    assert_eq!(second.burned.as_ref().map(|tx| tx.amount()), Some(luun(25)));
    assert_eq!(second.burned.as_ref().map(|tx| tx.fee()), Some(MICRO_LUUN));
}

#[test]
fn default_automatic_mining_does_not_burn() {
    assert_eq!(DEFAULT_BURN_PER_BLOCK, 0);

    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    let mut node = node("alice", alice.clone(), allocations);

    let outcome = node.automatic_mine_once(1);
    assert!(outcome.burned.is_none());
    assert!(outcome.block.is_none());
    assert!(
        outcome
            .skipped_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("automatic mining is off"))
    );
    assert_eq!(node.ledger().balance_of(alice.address()), 1_000);
}

#[test]
fn burn_per_block_can_be_set_to_zero() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    let mut node = node("alice", alice, allocations);

    let burned = node.set_burn_per_block(25).unwrap();
    assert!(burned.is_some());
    assert!(node.status().mining.automatic);
    assert_eq!(node.status().mining.burn_per_block, 25);
    let burned = node.set_burn_per_block(0).unwrap();
    assert!(burned.is_none());
    assert!(!node.status().mining.automatic);
    assert_eq!(node.status().mining.burn_per_block, 0);
}

#[test]
fn automatic_burn_status_shows_configured_fee() {
    let alice = Wallet::from_seed("auto-fee-status-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    let mut node = node("alice", alice, allocations);

    assert_eq!(node.status().mining.automatic_burn_fee, 1);

    node.set_burn_per_block(1).unwrap();
    assert_eq!(node.status().mining.burn_per_block, 1);
    assert_eq!(node.status().mining.automatic_burn_fee, 1);

    node.set_automatic_burn(50, 3).unwrap();
    assert_eq!(node.status().mining.burn_per_block, 50);
    assert_eq!(node.status().mining.automatic_burn_fee, 3);
}

#[test]
fn automatic_mining_uses_configured_burn_fee() {
    let alice = Wallet::from_seed("auto-fee-burn-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    let mut node = node("alice", alice, allocations);

    let burned = node.set_automatic_burn(50, 3).unwrap().unwrap();
    assert_eq!(burned.amount(), 50);
    assert_eq!(burned.fee(), 3);
}

#[test]
fn automatic_mining_caps_burn_to_spendable_balance_after_fee() {
    let alice = Wallet::from_seed("auto-burn-cap-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), BLOCK_REWARD);
    let mut node = NodeCore::new(NodeConfig {
        wallet: alice.clone(),
        genesis_allocations: allocations,
        vdf_rounds: 10,
        burn_per_block: BLOCK_REWARD + luun(50),
        burn_fee: MICRO_LUUN,
    });

    let outcome = node.automatic_mine_once(1);

    assert_eq!(
        outcome.burned.as_ref().map(|tx| tx.amount()),
        Some(BLOCK_REWARD - MICRO_LUUN)
    );
    assert_eq!(outcome.burned.as_ref().map(|tx| tx.fee()), Some(MICRO_LUUN));
    assert!(outcome.block.is_some());
    assert_eq!(node.ledger().balance_of(alice.address()), MICRO_LUUN);
}

#[test]
fn setting_burn_rate_after_running_at_zero_adds_mempool_burn() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 100);

    let mut ledger = Ledger::new(allocations.clone(), 25);
    submit_burn(&mut ledger, &alice, 1);
    let first = ledger.mine_next_block(&alice, 1).unwrap();
    ledger.apply_block(first).unwrap();

    let mut bob_node = node("bob", bob.clone(), allocations);
    bob_node
        .receive(luun::app::GossipEnvelope::ChainSnapshot(ledger.snapshot()))
        .unwrap();

    let skipped = bob_node.automatic_mine_once(2);
    assert!(skipped.burned.is_none());
    assert!(skipped.block.is_none());

    let burned = bob_node.set_burn_per_block(1).unwrap();

    assert!(burned.is_some());
    assert_eq!(bob_node.ledger().pending().len(), 1);
    assert_eq!(bob_node.ledger().pending()[0].sender(), bob.address());
    assert_eq!(bob_node.ledger().pending()[0].amount(), 1);
}

#[test]
fn automatic_mining_waits_when_wallet_is_not_selected_leader() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);

    let mut ledger = Ledger::new(allocations.clone(), 25);
    submit_burn(&mut ledger, &alice, 1);
    let first = ledger.mine_next_block(&alice, 1).unwrap();
    ledger.apply_block(first).unwrap();

    let mut bob_node = node("bob", bob.clone(), allocations);
    bob_node.set_burn_per_block(10).unwrap();
    bob_node
        .receive(luun::app::GossipEnvelope::Block(ledger.chain()[1].clone()))
        .unwrap();

    let outcome = bob_node.automatic_mine_once(2);
    assert!(outcome.burned.is_some());
    assert!(outcome.block.is_none());
    assert!(outcome.skipped_reason.unwrap().contains(alice.address()));
    assert_eq!(bob_node.ledger().chain().len(), 2);
}

#[test]
fn waiting_wallet_gossips_pending_burn_to_selected_leader() {
    let alice = Wallet::from_seed("deadlock-alice");
    let bob = Wallet::from_seed("deadlock-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), luun(1_000));
    allocations.insert(bob.address().to_string(), luun(1_000));
    let ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(bob.address(), 1)], 25)
            .unwrap();

    assert_eq!(
        ledger.expected_leader_for_next_block().as_deref(),
        Some(bob.address())
    );
    let mut alice_node = NodeCore::from_ledger(alice.clone(), ledger.clone(), 1);
    let mut bob_node = NodeCore::from_ledger_with_burn_fee_and_enabled(
        bob.clone(),
        ledger,
        true,
        DEFAULT_BURN_PER_BLOCK,
        MICRO_LUUN,
    );

    let alice_outcome = alice_node.automatic_mine_once(1);
    assert!(alice_outcome.burned.is_some());
    assert!(alice_outcome.block.is_none());
    assert!(
        alice_outcome
            .skipped_reason
            .unwrap()
            .contains(bob.address())
    );
    assert!(bob_node.ledger().pending().is_empty());

    for envelope in alice_node.mempool_gossip() {
        bob_node.receive(envelope).unwrap();
    }

    assert_eq!(bob_node.ledger().pending().len(), 1);
    assert_eq!(bob_node.ledger().pending()[0].sender(), alice.address());
    let bob_outcome = bob_node.automatic_mine_once(1);
    assert!(bob_outcome.skipped_reason.is_none());
    assert!(bob_outcome.block.is_some());
}

#[test]
fn pow_only_node_gossips_mine_action_to_pob_only_finalizer() {
    let alice = Wallet::from_seed("pob-only-finalizer-alice");
    let bob = Wallet::from_seed("pow-only-miner-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 2 * MICRO_LUUN);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(alice.address(), MICRO_LUUN)],
        25,
    )
    .unwrap();

    assert_eq!(
        ledger.expected_leader_for_next_block().as_deref(),
        Some(alice.address())
    );
    let mut alice_node = NodeCore::from_ledger_with_burn_fee_and_enabled(
        alice,
        ledger.clone(),
        true,
        DEFAULT_BURN_PER_BLOCK,
        MICRO_LUUN,
    );
    let mut bob_node = NodeCore::from_ledger_with_burn_fee_and_enabled(
        bob.clone(),
        ledger,
        false,
        DEFAULT_BURN_PER_BLOCK,
        MICRO_LUUN,
    );
    bob_node
        .set_pow_mining_settings(true, MICRO_LUUN / 100)
        .unwrap();

    let bob_plan = bob_node.prepare_automatic_mining(1);
    let mine = bob_plan.pow_mined.expect("B should queue a mine action");
    assert_eq!(
        bob_plan.skipped_reason.as_deref(),
        Some("automatic mining is off")
    );
    assert!(alice_node.ledger().pending().is_empty());

    for envelope in bob_node.drain_outbox() {
        alice_node.receive(envelope).unwrap();
    }

    assert!(
        alice_node
            .ledger()
            .pending()
            .iter()
            .any(|pending| pending.signature() == mine.signature()),
        "A did not receive B's mine action"
    );
}

#[test]
fn block_with_wrong_vdf_rounds_is_rejected() {
    let wallet = Wallet::from_seed("alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1_000);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 25)
            .unwrap();
    submit_burn(&mut ledger, &wallet, 1);

    let mut block = ledger.mine_next_block(&wallet, 1).unwrap();
    block.vdf_rounds = 1;
    block.hash = block.compute_hash();
    block.vdf_output = run_vdf(&block.vdf_seed(), block.vdf_rounds);

    assert!(ledger.apply_block(block).is_err());
}

#[test]
fn vdf_solution_verifies_without_rerunning_delay() {
    let solution = run_vdf("test-seed", 128);

    assert!(verify_vdf("test-seed", 128, &solution));
    assert!(!verify_vdf("other-seed", 128, &solution));
    assert!(!verify_vdf("test-seed", 129, &solution));
    assert!(!verify_vdf("test-seed", 128, "not-a-vdf-solution"));
}

#[test]
fn vdf_rounds_retarget_toward_one_minute_blocks() {
    let wallet = Wallet::from_seed("alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1_000);
    let mut ledger = Ledger::new(genesis, 100);

    submit_burn(&mut ledger, &wallet, 1);
    let block1 = ledger
        .mine_next_block(&wallet, VDF_TARGET_BLOCK_MS)
        .unwrap();
    assert_eq!(block1.vdf_rounds, 100);
    ledger.apply_block(block1).unwrap();
    assert_eq!(ledger.vdf_rounds(), 100);

    submit_burn(&mut ledger, &wallet, 1);
    let block2 = ledger
        .mine_next_block(&wallet, VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS / 2)
        .unwrap();
    assert_eq!(block2.vdf_rounds, 100);
    ledger.apply_block(block2).unwrap();
    assert_eq!(ledger.vdf_rounds(), 110);

    submit_burn(&mut ledger, &wallet, 1);
    let block3 = ledger
        .mine_next_block(
            &wallet,
            VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS / 2 + VDF_TARGET_BLOCK_MS * 2,
        )
        .unwrap();
    assert_eq!(block3.vdf_rounds, 110);
    ledger.apply_block(block3).unwrap();
    assert_eq!(ledger.vdf_rounds(), 99);
}

#[test]
fn conflicting_utxo_spends_are_not_accepted_together() {
    let wallet = Wallet::from_seed("alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 1_000);
    let mut ledger = Ledger::new(genesis, 25);

    let first = burn_tx(&ledger, &wallet, 3);
    let conflicting = burn_tx(&ledger, &wallet, 4);
    ledger.submit_transaction(first.clone()).unwrap();
    assert!(!ledger.submit_transaction(conflicting).unwrap());
    assert_eq!(ledger.pending().len(), 1);

    let block = ledger
        .prepare_next_block(wallet.address(), 1)
        .unwrap()
        .finish(&wallet, "test-vdf".to_string());
    assert_eq!(block.transactions.len(), 1);
    assert!(block.transactions.contains(&first));
}

#[test]
fn in_memory_network_syncs_nodes_without_tcp() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);

    let mut network = InMemoryNetwork::default();
    network.insert("alice", node("alice", alice.clone(), allocations.clone()));
    network.insert("bob", node("bob", bob.clone(), allocations));

    network.node_mut("alice").unwrap().burn(10).unwrap();
    network.deliver_until_idle().unwrap();
    assert_eq!(network.node("bob").unwrap().ledger().pending().len(), 1);

    network.node_mut("alice").unwrap().mine_one().unwrap();
    network.deliver_until_idle().unwrap();

    let alice_tip = network.node("alice").unwrap().ledger().status().tip_hash;
    let bob_tip = network.node("bob").unwrap().ledger().status().tip_hash;
    assert_eq!(alice_tip, bob_tip);
    assert_eq!(network.node("bob").unwrap().ledger().chain().len(), 2);
}

#[test]
fn in_memory_network_delivers_transaction_to_multiple_peers() {
    let wallets = wallets(&["alice", "bob", "carol", "dave"]);
    let allocations = allocations(&wallets, 1_000);
    let mut network = InMemoryNetwork::default();

    for (name, wallet) in ["alice", "bob", "carol", "dave"]
        .iter()
        .zip(wallets.clone())
    {
        network.insert(*name, node(name, wallet, allocations.clone()));
    }

    network.node_mut("alice").unwrap().burn(15).unwrap();
    network.deliver_until_idle().unwrap();

    for name in ["bob", "carol", "dave"] {
        let pending = network.node(name).unwrap().ledger().pending();
        assert_eq!(pending.len(), 1, "{name} did not receive alice's burn");
        assert_eq!(pending[0].amount(), 15);
    }
}

#[test]
fn in_memory_network_syncs_mined_block_to_multiple_peers() {
    let wallets = wallets(&["alice", "bob", "carol", "dave"]);
    let allocations = allocations(&wallets, 1_000);
    let mut network = InMemoryNetwork::default();

    for (name, wallet) in ["alice", "bob", "carol", "dave"]
        .iter()
        .zip(wallets.clone())
    {
        network.insert(*name, node(name, wallet, allocations.clone()));
    }

    network.node_mut("alice").unwrap().burn(10).unwrap();
    network.deliver_until_idle().unwrap();
    network.node_mut("alice").unwrap().mine_one().unwrap();
    network.deliver_until_idle().unwrap();

    let tip = network.node("alice").unwrap().ledger().status().tip_hash;
    for name in ["alice", "bob", "carol", "dave"] {
        let ledger = network.node(name).unwrap().ledger();
        assert_eq!(ledger.status().height, 1, "{name} is at the wrong height");
        assert_eq!(ledger.status().tip_hash, tip, "{name} has a different tip");
        assert!(
            ledger.pending().is_empty(),
            "{name} kept mined transactions"
        );
    }
}

#[test]
fn in_memory_network_range_syncs_node_that_missed_multiple_blocks() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob];
    let allocations = allocations(&wallets, 1_000);
    let mut network = InMemoryNetwork::default();

    network.insert("alice", node("alice", alice.clone(), allocations.clone()));
    network.insert("bob", node("bob", wallets[1].clone(), allocations));

    for height in 1..=5 {
        network.node_mut("alice").unwrap().burn(1).unwrap();
        network
            .node_mut("alice")
            .unwrap()
            .mine_one_at(height * VDF_TARGET_BLOCK_MS)
            .unwrap();
    }
    assert_eq!(network.node("alice").unwrap().ledger().status().height, 5);
    assert_eq!(network.node("bob").unwrap().ledger().status().height, 0);

    assert!(network.sync_node_from_peer("alice", "bob", 2).unwrap());
    assert_eq!(network.node("bob").unwrap().ledger().status().height, 2);

    assert!(network.sync_node_from_peer("alice", "bob", 10).unwrap());
    let alice_tip = network.node("alice").unwrap().ledger().status().tip_hash;
    let bob_status = network.node("bob").unwrap().ledger().status();
    assert_eq!(bob_status.height, 5);
    assert_eq!(bob_status.tip_hash, alice_tip);
}

#[test]
fn joined_nodes_import_transfer_block_and_every_wallet_mines() {
    let alice = Wallet::from_seed("flow-alice");
    let bob = Wallet::from_seed("flow-bob");
    let carol = Wallet::from_seed("flow-carol");
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), luun(100));
    let alice_ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![GenesisBurn::new(alice.address(), MICRO_LUUN)],
        5,
    )
    .unwrap();

    let mut network = InMemoryNetwork::default();
    network.insert(
        "a",
        NodeCore::from_ledger(alice.clone(), alice_ledger, DEFAULT_BURN_PER_BLOCK),
    );
    let mut mined_by = Vec::new();

    for height in 1..=2 {
        network.node_mut("a").unwrap().burn(1).unwrap();
        network.deliver_until_idle().unwrap();
        let block = network.node_mut("a").unwrap().mine_one_at(height).unwrap();
        mined_by.push(block.miner.clone());
        network.deliver_until_idle().unwrap();
    }

    let bob_ledger = Ledger::from_snapshot(network.node("a").unwrap().chain_snapshot()).unwrap();
    network.insert(
        "b",
        NodeCore::from_ledger(bob.clone(), bob_ledger, DEFAULT_BURN_PER_BLOCK),
    );

    for height in 3..=4 {
        network.node_mut("a").unwrap().burn(1).unwrap();
        network.deliver_until_idle().unwrap();
        let block = network.node_mut("a").unwrap().mine_one_at(height).unwrap();
        mined_by.push(block.miner.clone());
        network.deliver_until_idle().unwrap();
    }

    let carol_ledger = Ledger::from_snapshot(network.node("a").unwrap().chain_snapshot()).unwrap();
    network.insert(
        "c",
        NodeCore::from_ledger(carol.clone(), carol_ledger, DEFAULT_BURN_PER_BLOCK),
    );

    network
        .node_mut("a")
        .unwrap()
        .transfer_with_fee(bob.address(), luun(30), 0)
        .unwrap();
    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block5 = network.node_mut("a").unwrap().mine_one_at(5).unwrap();
    assert!(
        block5
            .transactions
            .iter()
            .any(|tx| tx.to() == Some(bob.address()) && tx.amount() == luun(30))
    );
    mined_by.push(block5.miner.clone());
    let block5_outbox = network.node_mut("a").unwrap().drain_outbox();
    for envelope in &block5_outbox {
        network
            .node_mut("b")
            .unwrap()
            .receive(envelope.clone())
            .unwrap();
    }
    assert_eq!(network.node("b").unwrap().ledger().status().height, 5);
    assert_eq!(network.node("c").unwrap().ledger().status().height, 4);
    let catchup_snapshot = network.node("a").unwrap().chain_snapshot();
    network
        .node_mut("c")
        .unwrap()
        .import_chain_snapshot(catchup_snapshot)
        .unwrap();

    for id in ["a", "b", "c"] {
        assert_eq!(
            network.node(id).unwrap().ledger().status().height,
            5,
            "{id} did not import block 5"
        );
        assert_eq!(
            network.node(id).unwrap().ledger().balance_of(bob.address()),
            luun(30),
            "{id} did not apply A -> B transfer"
        );
    }

    network.node_mut("b").unwrap().burn(10).unwrap();
    network.deliver_until_idle().unwrap();
    let block6 = network.node_mut("a").unwrap().mine_one_at(6).unwrap();
    mined_by.push(block6.miner.clone());
    network.deliver_until_idle().unwrap();

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block7 = network.node_mut("a").unwrap().mine_one_at(7).unwrap();
    mined_by.push(block7.miner.clone());
    network.deliver_until_idle().unwrap();

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block8 = network.node_mut("a").unwrap().mine_one_at(8).unwrap();
    mined_by.push(block8.miner.clone());
    network.deliver_until_idle().unwrap();

    network
        .node_mut("b")
        .unwrap()
        .transfer_with_fee(carol.address(), luun(10), 0)
        .unwrap();
    network.node_mut("b").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    assert_eq!(
        network
            .node("a")
            .unwrap()
            .ledger()
            .expected_leader_for_next_block(),
        Some(bob.address().to_string())
    );
    let block9 = network.node_mut("b").unwrap().mine_one_at(9).unwrap();
    mined_by.push(block9.miner.clone());
    network.deliver_until_idle().unwrap();

    network.node_mut("c").unwrap().burn(5).unwrap();
    network.deliver_until_idle().unwrap();
    let block10 = network.node_mut("a").unwrap().mine_one_at(10).unwrap();
    mined_by.push(block10.miner.clone());
    network.deliver_until_idle().unwrap();

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block11 = network.node_mut("a").unwrap().mine_one_at(11).unwrap();
    mined_by.push(block11.miner.clone());
    network.deliver_until_idle().unwrap();

    network.node_mut("b").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block12 = network.node_mut("b").unwrap().mine_one_at(12).unwrap();
    mined_by.push(block12.miner.clone());
    network.deliver_until_idle().unwrap();

    assert_eq!(
        network
            .node("a")
            .unwrap()
            .ledger()
            .expected_leader_for_next_block(),
        Some(carol.address().to_string())
    );
    network.node_mut("c").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let block13 = network.node_mut("c").unwrap().mine_one_at(13).unwrap();
    mined_by.push(block13.miner.clone());
    network.deliver_until_idle().unwrap();

    let final_tip = network.node("a").unwrap().ledger().status().tip_hash;
    for id in ["a", "b", "c"] {
        assert_eq!(network.node(id).unwrap().ledger().status().height, 13);
        assert_eq!(
            network.node(id).unwrap().ledger().status().tip_hash,
            final_tip
        );
    }
    for wallet in [&alice, &bob, &carol] {
        assert!(
            mined_by.iter().any(|miner| miner == wallet.address()),
            "{} never mined",
            wallet.address()
        );
    }
}

#[test]
fn persisted_joined_nodes_restart_and_keep_syncing_without_tcp() {
    let temp = tempdir().unwrap();
    let alice = Wallet::from_seed("persistent-flow-alice");
    let bob = Wallet::from_seed("persistent-flow-bob");
    let carol = Wallet::from_seed("persistent-flow-carol");
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 50);
    let alice_ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(alice.address(), 1)], 5)
            .unwrap();

    let mut network = InMemoryNetwork::default();
    network.insert(
        "a",
        NodeCore::from_ledger(alice.clone(), alice_ledger, DEFAULT_BURN_PER_BLOCK),
    );

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.node_mut("a").unwrap().mine_one_at(1).unwrap();
    network.deliver_until_idle().unwrap();

    let bob_store = SqliteChainStore::open(temp.path().join("bob.sqlite3")).unwrap();
    bob_store
        .save(&network.node("a").unwrap().chain_snapshot())
        .unwrap();
    let bob_joined_ledger = Ledger::from_snapshot(bob_store.load().unwrap().unwrap()).unwrap();
    network.insert(
        "b",
        NodeCore::from_ledger(bob.clone(), bob_joined_ledger, DEFAULT_BURN_PER_BLOCK),
    );
    assert_eq!(
        network.node("b").unwrap().ledger().status().tip_hash,
        network.node("a").unwrap().ledger().status().tip_hash
    );

    network
        .node_mut("a")
        .unwrap()
        .transfer(bob.address(), 10)
        .unwrap();
    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    network.node_mut("a").unwrap().mine_one_at(2).unwrap();
    network.deliver_until_idle().unwrap();
    assert_eq!(
        network
            .node("b")
            .unwrap()
            .ledger()
            .balance_of(bob.address()),
        10
    );

    bob_store
        .save(&network.node("b").unwrap().chain_snapshot())
        .unwrap();
    let bob_restarted_ledger = Ledger::from_snapshot(bob_store.load().unwrap().unwrap()).unwrap();
    network.insert(
        "b",
        NodeCore::from_ledger(bob.clone(), bob_restarted_ledger, DEFAULT_BURN_PER_BLOCK),
    );
    assert_eq!(
        network.node("b").unwrap().ledger().status().tip_hash,
        network.node("a").unwrap().ledger().status().tip_hash,
        "restarted Bob should resume the persisted chain tip"
    );

    let carol_store = SqliteChainStore::open(temp.path().join("carol.sqlite3")).unwrap();
    carol_store
        .save(&network.node("a").unwrap().chain_snapshot())
        .unwrap();
    let carol_joined_ledger = Ledger::from_snapshot(carol_store.load().unwrap().unwrap()).unwrap();
    network.insert(
        "c",
        NodeCore::from_ledger(carol, carol_joined_ledger, DEFAULT_BURN_PER_BLOCK),
    );

    network.node_mut("b").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    network.node_mut("a").unwrap().mine_one_at(3).unwrap();
    network.deliver_until_idle().unwrap();

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    network.node_mut("a").unwrap().mine_one_at(4).unwrap();
    network.deliver_until_idle().unwrap();

    network.node_mut("a").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    network.node_mut("a").unwrap().mine_one_at(5).unwrap();
    network.deliver_until_idle().unwrap();

    assert_eq!(
        network
            .node("a")
            .unwrap()
            .ledger()
            .expected_leader_for_next_block()
            .as_deref(),
        Some(bob.address())
    );

    network.node_mut("b").unwrap().burn(1).unwrap();
    network.deliver_until_idle().unwrap();
    let bob_block = network.node_mut("b").unwrap().mine_one_at(6).unwrap();
    assert_eq!(bob_block.miner, bob.address());
    network.deliver_until_idle().unwrap();

    let final_status = network.node("a").unwrap().ledger().status();
    for id in ["b", "c"] {
        assert_eq!(
            network.node(id).unwrap().ledger().status().height,
            final_status.height,
            "{id} did not catch up after Bob restarted"
        );
        assert_eq!(
            network.node(id).unwrap().ledger().status().tip_hash,
            final_status.tip_hash,
            "{id} ended on a different tip after Bob restarted"
        );
    }

    bob_store
        .save(&network.node("b").unwrap().chain_snapshot())
        .unwrap();
    assert_eq!(
        bob_store
            .load()
            .unwrap()
            .unwrap()
            .blocks
            .last()
            .unwrap()
            .height,
        final_status.height
    );
}

#[test]
fn mined_block_gossip_does_not_include_full_chain_snapshot() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob.clone()];
    let allocations = allocations(&wallets, 1_000);

    let mut alice_node = NodeCore::new(NodeConfig {
        wallet: alice,
        genesis_allocations: allocations.clone(),
        vdf_rounds: 10,
        burn_per_block: 1,
        burn_fee: 1,
    });

    let plan = alice_node.prepare_automatic_mining(1);
    let burn_outbox = alice_node.drain_outbox();
    assert_eq!(burn_outbox.len(), 1);
    assert!(matches!(
        burn_outbox[0],
        luun::app::GossipEnvelope::Transaction(_)
    ));

    let work = plan.work.unwrap();
    let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
    alice_node
        .complete_prepared_block(work, vdf_output)
        .unwrap();
    let block_outbox = alice_node.drain_outbox();

    assert_eq!(block_outbox.len(), 1);
    assert!(matches!(
        block_outbox[0],
        luun::app::GossipEnvelope::Block(_)
    ));
}

#[test]
fn received_transaction_is_rebroadcast_to_other_peers_without_networking() {
    let names = ["alice", "bob", "carol"];
    let wallets = wallets(&names);
    let allocations = allocations(&wallets, 1_000);
    let alice = wallets[0].clone();
    let bob = wallets[1].clone();
    let carol = wallets[2].clone();

    let mut carol_node = node("carol", carol, allocations.clone());
    let mut hub = node("alice", alice, allocations.clone());
    let mut bob_node = node("bob", bob, allocations);

    let tx = carol_node.burn(25).unwrap();
    carol_node.drain_outbox();

    hub.receive(luun::app::GossipEnvelope::Transaction(tx.clone()))
        .unwrap();
    let forwarded = hub.drain_outbox();
    assert_eq!(forwarded.len(), 1);
    assert!(matches!(
        forwarded[0],
        luun::app::GossipEnvelope::Transaction(_)
    ));

    for envelope in forwarded {
        bob_node.receive(envelope).unwrap();
    }
    assert!(
        bob_node
            .ledger()
            .pending()
            .iter()
            .any(|pending| pending.signature() == tx.signature())
    );

    hub.receive(luun::app::GossipEnvelope::Transaction(tx))
        .unwrap();
    assert!(hub.drain_outbox().is_empty());
}

#[test]
fn mempool_gossip_repairs_future_nonce_gap_without_networking() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob.clone()];
    let allocations = allocations(&wallets, 1_000);
    let mut alice_node = node("alice", alice, allocations.clone());
    let mut bob_node = node("bob", bob, allocations);

    let first = alice_node.burn(1).unwrap();
    let second = alice_node.burn(1).unwrap();
    alice_node.drain_outbox();

    bob_node
        .receive(luun::app::GossipEnvelope::Transaction(second.clone()))
        .unwrap();
    assert_eq!(bob_node.ledger().pending().len(), 1);

    let mut requests = Vec::new();
    for envelope in alice_node.mempool_gossip() {
        match envelope {
            luun::app::GossipEnvelope::Inventory { txs, blocks } => {
                requests.extend(bob_node.missing_inventory_requests(&txs, &blocks));
            }
            other => bob_node.receive(other).unwrap(),
        }
    }
    for request in requests {
        match request {
            luun::app::GossipEnvelope::TransactionRequest { signatures } => {
                bob_node
                    .receive(luun::app::GossipEnvelope::Transactions {
                        transactions: alice_node.transactions_by_signature(&signatures),
                    })
                    .unwrap();
            }
            other => bob_node.receive(other).unwrap(),
        }
    }
    let block = alice_node.mine_one_at(1).unwrap();
    let signatures = block
        .transactions
        .iter()
        .map(|tx| tx.signature())
        .collect::<Vec<_>>();

    assert!(signatures.contains(&first.signature()));
    assert!(signatures.contains(&second.signature()));
}

#[test]
fn received_block_is_rebroadcast_to_other_peers_without_networking() {
    let names = ["alice", "bob", "carol"];
    let wallets = wallets(&names);
    let allocations = allocations(&wallets, 1_000);
    let alice = wallets[0].clone();
    let bob = wallets[1].clone();
    let carol = wallets[2].clone();

    let mut miner = node("alice", alice, allocations.clone());
    let mut hub = node("bob", bob, allocations.clone());
    let mut carol_node = node("carol", carol, allocations);

    miner.burn(10).unwrap();
    miner.drain_outbox();
    let block = miner.mine_one_at(1).unwrap();
    miner.drain_outbox();

    hub.receive(luun::app::GossipEnvelope::Block(block.clone()))
        .unwrap();
    let forwarded = hub.drain_outbox();
    assert_eq!(forwarded.len(), 1);
    assert!(matches!(forwarded[0], luun::app::GossipEnvelope::Block(_)));

    for envelope in forwarded {
        carol_node.receive(envelope).unwrap();
    }
    assert_eq!(carol_node.ledger().height(), 1);
    assert_eq!(
        carol_node.ledger().status().tip_hash,
        miner.ledger().status().tip_hash
    );

    hub.receive(luun::app::GossipEnvelope::Block(block))
        .unwrap();
    assert!(hub.drain_outbox().is_empty());
}

#[test]
fn imported_snapshot_blocks_are_rebroadcast_without_networking() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob.clone()];
    let allocations = allocations(&wallets, 1_000);
    let mut miner = node("alice", alice, allocations.clone());
    let mut hub = node("bob", bob, allocations);

    miner.burn(1).unwrap();
    miner.drain_outbox();
    miner.mine_one_at(1).unwrap();
    miner.drain_outbox();

    miner.burn(1).unwrap();
    miner.drain_outbox();
    miner.mine_one_at(2).unwrap();
    miner.drain_outbox();

    hub.import_chain_snapshot(miner.chain_snapshot()).unwrap();
    let outbox = hub.drain_outbox();
    assert_eq!(outbox.len(), 1);
    match &outbox[0] {
        luun::app::GossipEnvelope::Blocks { blocks } => {
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].height, 1);
            assert_eq!(blocks[1].height, 2);
        }
        other => panic!("expected imported blocks gossip, got {other:?}"),
    }
}

#[test]
fn multiple_peers_can_contribute_burns_to_the_same_lottery_block() {
    let names = ["alice", "bob", "carol", "dave"];
    let wallets = wallets(&names);
    let allocations = allocations(&wallets, 1_000);
    let mut network = InMemoryNetwork::default();

    for (name, wallet) in names.iter().zip(wallets.clone()) {
        network.insert(*name, node(name, wallet, allocations.clone()));
    }

    for (name, amount) in names.iter().zip([10, 20, 30, 40]) {
        network.node_mut(name).unwrap().burn(amount).unwrap();
    }
    network.deliver_until_idle().unwrap();
    network.node_mut("alice").unwrap().mine_one().unwrap();
    network.deliver_until_idle().unwrap();

    for name in names {
        let ledger = network.node(name).unwrap().ledger();
        let block = &ledger.chain()[1];
        let burned = block
            .transactions
            .iter()
            .filter(|tx| tx.is_burn())
            .map(|tx| tx.amount())
            .sum::<Amount>();

        assert_eq!(block.transactions.len(), 4);
        assert_eq!(burned, 100);
        assert!(ledger.expected_leader_for_next_block().is_some());
    }
}

#[test]
fn peer_book_tracks_multiple_peers_without_networking() {
    let mut peers = PeerBook::from_addresses(vec![
        "127.0.0.1:9444".to_string(),
        "127.0.0.1:9445".to_string(),
        "127.0.0.1:9444".to_string(),
    ]);

    peers.record_sent("127.0.0.1:9444", 2);
    peers.record_status("127.0.0.1:9444", 12, "tip-hash".to_string());
    peers.record_error("127.0.0.1:9445", "connection refused");
    peers.record_received("127.0.0.1:9555", 1);
    peers.record_inbound_error("127.0.0.1:56666", "invalid nonce");

    let mut list = peers.list();
    list.sort_by(|left, right| left.address.cmp(&right.address));
    assert_eq!(list.len(), 4);

    let outbound_addresses = peers.addresses();
    assert_eq!(outbound_addresses.len(), 2);
    assert!(outbound_addresses.contains(&"127.0.0.1:9444".to_string()));
    assert!(outbound_addresses.contains(&"127.0.0.1:9445".to_string()));
    assert!(!outbound_addresses.contains(&"127.0.0.1:56666".to_string()));

    let sent_peer = list
        .iter()
        .find(|peer| peer.address == "127.0.0.1:9444")
        .unwrap();
    assert_eq!(sent_peer.messages_sent, 2);
    assert_eq!(sent_peer.last_known_height, Some(12));
    assert_eq!(sent_peer.last_known_tip_hash.as_deref(), Some("tip-hash"));
    assert_eq!(sent_peer.last_error, None);

    let failed_peer = list
        .iter()
        .find(|peer| peer.address == "127.0.0.1:9445")
        .unwrap();
    assert_eq!(
        failed_peer.last_error.as_deref(),
        Some("connection refused")
    );

    let inbound_peer = list
        .iter()
        .find(|peer| peer.address == "127.0.0.1:9555")
        .unwrap();
    assert_eq!(inbound_peer.direction, PeerDirection::Inbound);
    assert_eq!(inbound_peer.messages_received, 1);

    let inbound_error = list
        .iter()
        .find(|peer| peer.address == "127.0.0.1:56666")
        .unwrap();
    assert_eq!(inbound_error.direction, PeerDirection::Inbound);
    assert_eq!(inbound_error.last_error.as_deref(), Some("invalid nonce"));
}

#[test]
fn chain_snapshot_round_trips_ledger_state() {
    let alice = Wallet::from_seed("alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);

    let mut ledger = Ledger::new(allocations, 10);
    submit_burn(&mut ledger, &alice, 10);
    let block = ledger.mine_next_block(&alice, 1).unwrap();
    ledger.apply_block(block).unwrap();

    let restored = Ledger::from_snapshot(ledger.snapshot()).unwrap();
    assert_eq!(restored.status().height, ledger.status().height);
    assert_eq!(restored.status().tip_hash, ledger.status().tip_hash);
    assert_eq!(
        restored.balance_of(alice.address()),
        ledger.balance_of(alice.address())
    );
}

#[test]
fn friend_node_can_join_snapshot_from_started_chain() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");

    let mut alice_genesis = BTreeMap::new();
    alice_genesis.insert(alice.address().to_string(), 1_000);
    let mut alice_node = node("alice", alice.clone(), alice_genesis);
    alice_node
        .set_automatic_burn_settings(true, DEFAULT_BURN_PER_BLOCK, MICRO_LUUN)
        .unwrap();
    alice_node.burn(1).unwrap();
    alice_node.automatic_mine_once(1);

    let joined_ledger = Ledger::from_snapshot(alice_node.chain_snapshot()).unwrap();
    let mut bob_node = NodeCore::from_ledger(bob.clone(), joined_ledger, DEFAULT_BURN_PER_BLOCK);

    assert_eq!(
        bob_node.ledger().status().tip_hash,
        alice_node.ledger().status().tip_hash
    );
    assert_eq!(bob_node.ledger().status().height, 1);
    assert_eq!(bob_node.ledger().balance_of(bob.address()), 0);

    let outcome = bob_node.automatic_mine_once(2);
    assert!(outcome.burned.is_none());
}

#[test]
fn running_node_rejects_snapshot_from_different_genesis() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");

    let mut alice_genesis = BTreeMap::new();
    alice_genesis.insert(alice.address().to_string(), 1_000);
    let alice_node = node("alice", alice, alice_genesis);

    let mut bob_genesis = BTreeMap::new();
    bob_genesis.insert(bob.address().to_string(), 1_000);
    let mut bob_node = node("bob", bob, bob_genesis);

    let error = bob_node
        .import_chain_snapshot(alice_node.chain_snapshot())
        .unwrap_err();

    assert!(error.to_string().contains("genesis"));
}

#[test]
fn same_height_fork_snapshot_does_not_reorg() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob.clone()];
    let shared_genesis = allocations(&wallets, 1_000);
    let base = Ledger::new_with_genesis_burns(
        shared_genesis,
        vec![GenesisBurn::new(alice.address(), 1)],
        1,
    )
    .unwrap();
    let mut local = base.clone();

    let local_first_fork_hash = mine_wallet_burn_block(&mut local, &alice, 1);
    let remote = fork_with_worse_vrf_block(&base, &alice, &local_first_fork_hash, 1).unwrap();

    let local_tip = local.status().tip_hash;
    assert!(!local.extend_from_snapshot(remote.snapshot()).unwrap());
    assert_eq!(local.status().tip_hash, local_tip);
}

#[test]
fn fork_choice_preflight_rejects_non_matching_genesis_before_scoring() {
    let alice = Wallet::from_seed("preflight-genesis-alice");
    let bob = Wallet::from_seed("preflight-genesis-bob");
    let mut local_genesis = BTreeMap::new();
    local_genesis.insert(alice.address().to_string(), 1_000);
    let mut remote_genesis = BTreeMap::new();
    remote_genesis.insert(bob.address().to_string(), 1_000);
    let mut local = Ledger::new(local_genesis, 1);
    let mut remote = Ledger::new(remote_genesis, 1);

    mine_wallet_burn_block(&mut local, &alice, 1);
    for timestamp in 1..=3 {
        mine_wallet_burn_block(&mut remote, &bob, timestamp);
    }

    let local_tip = local.status().tip_hash;
    let error = local.extend_from_snapshot(remote.snapshot()).unwrap_err();

    assert!(error.to_string().contains("genesis"));
    assert_eq!(local.status().tip_hash, local_tip);
}

#[test]
fn fork_choice_preflight_rejects_invalid_fork_before_vrf_scoring() {
    let alice = Wallet::from_seed("preflight-invalid-alice");
    let shared_genesis = allocations(std::slice::from_ref(&alice), 10_000);
    let mut common = Ledger::new(shared_genesis, 1);
    for timestamp in 1..=5 {
        mine_wallet_burn_block(&mut common, &alice, timestamp);
    }

    let mut local = common.clone();
    let local_first_fork_hash = mine_wallet_burn_block(&mut local, &alice, 6);
    mine_wallet_burn_block(&mut local, &alice, 7);
    mine_wallet_burn_block(&mut local, &alice, 8);

    let remote = fork_with_better_vrf_block(&common, &alice, &local_first_fork_hash, 100).unwrap();
    assert!(remote.chain()[6].hash < local.chain()[6].hash);
    let mut invalid_snapshot = remote.snapshot();
    if let Some(transaction) = invalid_snapshot.blocks[6].transactions.first_mut() {
        match transaction {
            luun::domain::Transaction::Burn { signature, .. }
            | luun::domain::Transaction::Transfer { signature, .. }
            | luun::domain::Transaction::Mine { signature, .. } => signature.push_str("00"),
        }
    }

    let local_tip = local.status().tip_hash;
    let error = local.extend_from_snapshot(invalid_snapshot).unwrap_err();

    assert!(error.to_string().contains("invalid"));
    assert_eq!(local.status().height, 8);
    assert_eq!(local.status().tip_hash, local_tip);
}

#[test]
fn fork_conflict_before_last_six_blocks_is_finalized_even_if_remote_is_longer() {
    let alice = Wallet::from_seed("finality-alice");
    let shared_genesis = allocations(std::slice::from_ref(&alice), 10_000);
    let mut common = Ledger::new(shared_genesis, 1);
    mine_wallet_burn_block(&mut common, &alice, 1);

    let mut local = common.clone();
    for timestamp in 2..=8 {
        mine_wallet_burn_block(&mut local, &alice, timestamp);
    }

    let mut remote = common;
    for timestamp in 20..=29 {
        mine_wallet_burn_block(&mut remote, &alice, timestamp);
    }

    assert_eq!(local.status().height, 8);
    assert_eq!(remote.status().height, 11);
    let finalized_local_tip = local.status().tip_hash;

    assert!(
        !local.extend_from_snapshot(remote.snapshot()).unwrap(),
        "forks that rewrite blocks before the last six should not be accepted"
    );
    assert_eq!(local.status().height, 8);
    assert_eq!(local.status().tip_hash, finalized_local_tip);
}

#[test]
fn shorter_better_rank_fork_inside_last_six_does_not_beat_positive_quality() {
    let alice = Wallet::from_seed("better-vrf-alice");
    let shared_genesis = allocations(std::slice::from_ref(&alice), 10_000);
    let mut common = Ledger::new(shared_genesis, 1);
    for timestamp in 1..=5 {
        mine_wallet_burn_block(&mut common, &alice, timestamp);
    }

    let mut local = common.clone();
    let local_first_fork_hash = mine_wallet_burn_block(&mut local, &alice, 6);
    mine_wallet_burn_block(&mut local, &alice, 7);
    mine_wallet_burn_block(&mut local, &alice, 8);

    let remote = fork_with_better_vrf_block(&common, &alice, &local_first_fork_hash, 100).unwrap();

    assert_eq!(local.status().height, 8);
    assert_eq!(remote.status().height, 6);
    assert!(remote.status().height + 2 >= local.status().height);
    assert!(
        remote.chain()[6].hash < local.chain()[6].hash,
        "test setup should give the remote fork the better VRF leader score"
    );
    let remote_tip = remote.status().tip_hash;

    assert!(
        !local.extend_from_snapshot(remote.snapshot()).unwrap(),
        "a shorter fork should not beat greater positive chain quality"
    );
    assert_eq!(local.status().height, 8);
    assert_ne!(local.status().tip_hash, remote_tip);
}

#[test]
fn better_vrf_fork_inside_last_six_loses_when_more_than_two_blocks_shorter() {
    let alice = Wallet::from_seed("too-short-vrf-alice");
    let shared_genesis = allocations(std::slice::from_ref(&alice), 10_000);
    let mut common = Ledger::new(shared_genesis, 1);
    for timestamp in 1..=5 {
        mine_wallet_burn_block(&mut common, &alice, timestamp);
    }

    let mut local = common.clone();
    let local_first_fork_hash = mine_wallet_burn_block(&mut local, &alice, 6);
    mine_wallet_burn_block(&mut local, &alice, 7);
    mine_wallet_burn_block(&mut local, &alice, 8);
    mine_wallet_burn_block(&mut local, &alice, 9);

    let remote = fork_with_better_vrf_block(&common, &alice, &local_first_fork_hash, 100).unwrap();

    assert_eq!(local.status().height, 9);
    assert_eq!(remote.status().height, 6);
    assert!(remote.chain()[6].hash < local.chain()[6].hash);
    let local_tip = local.status().tip_hash;

    assert!(
        !local.extend_from_snapshot(remote.snapshot()).unwrap(),
        "even a better VRF fork should not win when more than two blocks shorter"
    );
    assert_eq!(local.status().height, 9);
    assert_eq!(local.status().tip_hash, local_tip);
}

#[test]
fn transactions_from_abandoned_fork_blocks_return_to_mempool_after_switch() {
    let alice = Wallet::from_seed("reorg-alice");
    let bob = Wallet::from_seed("reorg-bob");
    let carol = Wallet::from_seed("reorg-carol");
    let wallets = vec![alice.clone(), bob.clone(), carol.clone()];
    let shared_genesis = allocations(&wallets, 10_000);
    let mut common = Ledger::new_with_genesis_burns(
        shared_genesis,
        vec![GenesisBurn::new(alice.address(), 1)],
        1,
    )
    .unwrap();
    mine_wallet_burn_block(&mut common, &alice, 1);

    let mut local = common.clone();
    let abandoned_transfer = transfer_tx(&local, &bob, carol.address(), 7);
    local
        .submit_transaction(abandoned_transfer.clone())
        .unwrap();
    mine_wallet_burn_block(&mut local, &alice, 2);

    let mut remote = common;
    for timestamp in 20..=23 {
        mine_wallet_burn_block(&mut remote, &alice, timestamp);
    }

    assert!(local.extend_from_snapshot(remote.snapshot()).unwrap());
    assert!(
        local
            .pending()
            .iter()
            .any(|tx| tx.signature() == abandoned_transfer.signature()),
        "transactions mined only on the abandoned fork should return to the mempool"
    );
}

#[test]
fn longer_valid_fork_snapshot_reorgs_and_preserves_local_transactions() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");
    let wallets = vec![alice.clone(), bob.clone()];
    let shared_genesis = allocations(&wallets, 1_000);
    let mut local = Ledger::new_with_genesis_burns(
        shared_genesis.clone(),
        vec![GenesisBurn::new(alice.address(), 1)],
        1,
    )
    .unwrap();
    let mut remote = Ledger::new_with_genesis_burns(
        shared_genesis,
        vec![GenesisBurn::new(alice.address(), 1)],
        1,
    )
    .unwrap();

    let local_burn = burn_tx(&local, &alice, 1);
    local.submit_transaction(local_burn.clone()).unwrap();
    let local_block = local.mine_next_block(&alice, 1).unwrap();
    local.apply_block(local_block).unwrap();
    let local_transfer = transfer_tx(&local, &bob, alice.address(), 5);
    local.submit_transaction(local_transfer.clone()).unwrap();

    submit_burn(&mut remote, &alice, 1);
    let remote_block_1 = remote.mine_next_block(&alice, 1).unwrap();
    remote.apply_block(remote_block_1).unwrap();
    submit_burn(&mut remote, &alice, 1);
    let remote_block_2 = remote.mine_next_block(&alice, 2).unwrap();
    remote.apply_block(remote_block_2).unwrap();

    let remote_tip = remote.status().tip_hash;
    assert!(local.extend_from_snapshot(remote.snapshot()).unwrap());
    assert_eq!(local.status().height, 2);
    assert_eq!(local.status().tip_hash, remote_tip);
    assert!(
        local
            .pending()
            .iter()
            .any(|tx| tx.signature() == local_transfer.signature())
    );
}

#[test]
fn node_receives_chain_snapshot_envelope_when_joining_without_tcp() {
    let alice = Wallet::from_seed("alice");
    let bob = Wallet::from_seed("bob");

    let wallets = vec![alice.clone(), bob.clone()];
    let shared_genesis = allocations(&wallets, 1_000);
    let mut alice_node = node("alice", alice, shared_genesis.clone());
    alice_node.burn(1).unwrap();
    alice_node.mine_one().unwrap();

    let mut bob_node = node("bob", bob, shared_genesis);

    bob_node
        .receive(luun::app::GossipEnvelope::ChainSnapshot(
            alice_node.chain_snapshot(),
        ))
        .unwrap();

    assert_eq!(
        bob_node.ledger().status().tip_hash,
        alice_node.ledger().status().tip_hash
    );
}
