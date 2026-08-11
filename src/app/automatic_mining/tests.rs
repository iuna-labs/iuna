use std::collections::BTreeMap;

use crate::{
    adapters::config_store::{DEFAULT_POW_MINING_WORKERS, MAX_POW_MINING_WORKERS},
    app::{GossipEnvelope, NodeConfig, NodeCore},
    domain::{
        FinalizerMode, GenesisBurn, Ledger, MICRO_IUNA, MINE_ACTIONS_PER_ANCHOR_LIMIT,
        MINE_FINALIZER_FEE, RECOVERY_BLOCK_DELAY_MS, Transaction, VDF_TARGET_BLOCK_MS, Wallet,
        run_vdf,
    },
};

#[test]
fn same_height_verified_import_does_not_reset_auto_burn_guard() {
    let alice = Wallet::from_seed("same-height-import-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), MICRO_IUNA);
    let mut node = NodeCore::new(NodeConfig {
        wallet: alice,
        genesis_allocations: allocations,
        vdf_rounds: 10,
        burn_per_block: 1,
        burn_fee: 1,
        pow_mining_workers: 1,
        recovery_vdf_top_rank_percent: 100,
    });

    let first = node.prepare_automatic_mining(1);
    assert!(first.burned.is_some());
    assert_eq!(node.last_auto_burn_height, Some(0));

    let same_height_ledger = node.clone_ledger();
    assert!(!node.import_verified_ledger(same_height_ledger).unwrap());
    assert_eq!(node.last_auto_burn_height, Some(0));

    let second = node.prepare_automatic_mining(2);
    assert!(second.burned.is_none());
}

#[test]
fn automatic_pow_mining_searches_bounded_nonce_batches_per_tip() {
    let wallet = Wallet::from_seed("automatic-pow-mining-wallet");
    let ledger = Ledger::new_with_genesis_burns(
        BTreeMap::from([(wallet.address().to_string(), 1)]),
        vec![GenesisBurn::new(wallet.address(), 1)],
        10,
    )
    .unwrap();
    let mut node = NodeCore::from_ledger(wallet.clone(), ledger, 0);

    let disabled = node.prepare_automatic_mining(1);
    assert!(disabled.pow_mined.is_none());
    assert_eq!(
        disabled.skipped_reason.as_deref(),
        Some("automatic mining is off")
    );

    node.set_pow_mining_enabled(true);
    let first = node.prepare_automatic_mining(2);
    assert!(node.ledger().pending_blinded_transactions().len() <= 1);
    let first = std::iter::once(first)
        .chain((3..10_000).map(|timestamp| node.prepare_automatic_mining(timestamp)))
        .find(|plan| plan.pow_mined.is_some())
        .expect("bounded PoW search should eventually find a proof");
    let first_mine = first.pow_mined.as_ref().expect("PoW should be queued");
    let Transaction::Mine {
        anchor,
        recipient,
        difficulty_bits,
        ..
    } = first_mine
    else {
        panic!("expected mine transaction");
    };
    assert_eq!(anchor, &node.chain().last().unwrap().hash);
    assert_eq!(recipient, wallet.address());
    assert_eq!(
        *difficulty_bits,
        node.ledger().current_mine_difficulty_bits()
    );
    let first_pending = node.ledger().pending().len();
    assert!(first_pending >= 1);
    assert!(node.ledger().pending_blinded_transactions().is_empty());
    assert!(node.drain_outbox().iter().any(|envelope| {
            matches!(envelope, GossipEnvelope::MineAction(tx) if tx.signature() == first_mine.signature())
        }));
    assert!(
        node.status()
            .mining
            .last_auto_pow_mine_status
            .as_deref()
            .unwrap_or_default()
            .contains("queued")
    );

    let second = (10_000..20_000)
        .map(|timestamp| node.prepare_automatic_mining(timestamp))
        .find(|plan| plan.pow_mined.is_some())
        .expect("automatic PoW should allow a second proof for the same tip");
    let second_mine = second.pow_mined.as_ref().expect("PoW should be queued");
    assert_ne!(second_mine.signature(), first_mine.signature());
    assert_eq!(node.ledger().pending().len(), first_pending + 1);

    for timestamp in 20_000..20_010 {
        assert!(node.prepare_automatic_mining(timestamp).pow_mined.is_none());
    }
    assert_eq!(node.ledger().pending().len(), first_pending + 1);
    assert_eq!(
        node.status().mining.last_auto_pow_mine_status.as_deref(),
        Some("waiting for next chain tip after queued mine actions")
    );
    assert!(node.ledger().pending_blinded_transactions().is_empty());
}

#[test]
fn automatic_pow_mining_waits_after_queueing_anchor_limit_for_tip() {
    let wallet = Wallet::from_seed("automatic-pow-independent-wallet");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 1);
    let mut node = NodeCore::new(NodeConfig {
        wallet,
        genesis_allocations: allocations,
        vdf_rounds: 10,
        burn_per_block: 0,
        burn_fee: 0,
        pow_mining_workers: 1,
        recovery_vdf_top_rank_percent: 100,
    });

    node.set_pow_mining_enabled(true);
    let first_mined = (1..10_000)
        .find_map(|_| node.prepare_automatic_pow_mining().unwrap())
        .expect("PoW should eventually queue a mine action");
    let anchor = match first_mined {
        Transaction::Mine { ref anchor, .. } => anchor.clone(),
        _ => panic!("expected mine action"),
    };
    assert_eq!(node.ledger().pending_mine_count_for_anchor(&anchor), 1);

    (1..10_000)
        .find_map(|_| node.prepare_automatic_pow_mining().unwrap())
        .expect("PoW should allow a second mine action for the same tip");
    assert_eq!(
        node.ledger().pending_mine_count_for_anchor(&anchor),
        MINE_ACTIONS_PER_ANCHOR_LIMIT
    );
    assert!(node.prepare_automatic_pow_mining().unwrap().is_none());
    assert!(node.auto_pow_mine_cursor.is_none());
}

#[test]
fn disabling_automatic_pow_mining_clears_local_work() {
    let wallet = Wallet::from_seed("automatic-pow-disable-wallet");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 1);
    let mut node = NodeCore::new(NodeConfig {
        wallet,
        genesis_allocations: allocations,
        vdf_rounds: 10,
        burn_per_block: 0,
        burn_fee: 0,
        pow_mining_workers: 1,
        recovery_vdf_top_rank_percent: 100,
    });

    node.set_pow_mining_enabled(true);
    assert!(node.pow_mining_enabled());
    node.prepare_automatic_pow_mining().unwrap();
    assert!(node.auto_pow_mine_cursor.is_some());
    assert!(node.status().mining.last_auto_pow_mine_status.is_some());

    node.set_pow_mining_enabled(false);

    assert!(!node.pow_mining_enabled());
    assert!(node.auto_pow_mine_cursor.is_none());
    assert!(node.status().mining.last_auto_pow_mine_status.is_none());
}

#[test]
fn automatic_pow_mining_workers_are_clamped_and_reported() {
    let wallet = Wallet::from_seed("automatic-pow-workers-wallet");
    let mut node = NodeCore::new(NodeConfig {
        wallet,
        genesis_allocations: BTreeMap::new(),
        vdf_rounds: 10,
        burn_per_block: 0,
        burn_fee: 0,
        pow_mining_workers: 99,
        recovery_vdf_top_rank_percent: 100,
    });

    assert_eq!(node.pow_mining_workers(), MAX_POW_MINING_WORKERS);
    assert_eq!(
        node.status().mining.max_pow_mining_workers,
        MAX_POW_MINING_WORKERS
    );

    node.set_pow_mining_workers(0);

    assert_eq!(node.pow_mining_workers(), DEFAULT_POW_MINING_WORKERS);
    assert_eq!(
        node.status().mining.pow_mining_workers,
        DEFAULT_POW_MINING_WORKERS
    );
}

#[test]
fn automatic_pow_mining_skips_unspendable_owned_blinded_payloads() {
    let alice = Wallet::from_seed("automatic-pow-stale-owned-blind-alice");
    let bob = Wallet::from_seed("automatic-pow-stale-owned-blind-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(bob.address(), MICRO_IUNA)],
        10,
    )
    .unwrap();
    let mut node = NodeCore::from_ledger(alice.clone(), ledger, 0);

    let blinded = node
        .blinded_burn_with_fee(MICRO_IUNA / 10, 7, node.chain_height() + 4)
        .unwrap();
    let mut finalizer_ledger = node.ledger().clone();
    let leader_burn = finalizer_ledger.build_burn(&bob, 1, 0).unwrap();
    finalizer_ledger.submit_transaction(leader_burn).unwrap();
    let commit_block = finalizer_ledger.mine_next_block(&bob, 1).unwrap();
    assert!(
        commit_block
            .blinded_transactions
            .iter()
            .any(|tx| tx.commitment == blinded.commitment)
    );
    finalizer_ledger.apply_block(commit_block).unwrap();
    assert!(node.import_verified_ledger(finalizer_ledger).unwrap());
    assert!(
        node.ledger()
            .has_unrevealed_blinded_transaction(&blinded.commitment)
    );

    node.set_pow_mining_enabled(true);

    assert!(node.prepare_automatic_pow_mining().is_ok());
}

#[test]
fn automatic_pow_mining_skips_stale_local_anchor_reservation() {
    let wallet = Wallet::from_seed("automatic-pow-stale-anchor-wallet");
    let mut stale_allocations = BTreeMap::new();
    stale_allocations.insert(wallet.address().to_string(), MICRO_IUNA);
    let stale_ledger = Ledger::new(stale_allocations, 10);
    let stale_anchor = stale_ledger.build_burn(&wallet, 1, 0).unwrap();
    let live_ledger = Ledger::new(BTreeMap::new(), 10);
    let mut node = NodeCore::from_ledger(wallet.clone(), live_ledger, 0);
    node.local_block_anchor_burn = Some((node.chain_height(), stale_anchor));
    node.set_pow_mining_enabled(true);

    assert!(node.prepare_automatic_pow_mining().is_ok());
}

#[test]
fn automatic_finalization_does_not_tick_pow_mining() {
    let wallet = Wallet::from_seed("automatic-pow-separated-finalizer-wallet");
    let mut node = NodeCore::new(NodeConfig {
        wallet,
        genesis_allocations: BTreeMap::new(),
        vdf_rounds: 10,
        burn_per_block: 0,
        burn_fee: 0,
        pow_mining_workers: 1,
        recovery_vdf_top_rank_percent: 100,
    });

    node.set_pow_mining_enabled(true);
    let _ = node.prepare_automatic_finalization(1);

    assert!(node.auto_pow_mine_cursor.is_none());
}

#[test]
fn automatic_finalization_prepares_recovery_after_ticket_timeout() {
    let alice = Wallet::from_seed("automatic-recovery-alice");
    let bob = Wallet::from_seed("automatic-recovery-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    let mut node = NodeCore::from_ledger(bob, ledger, 1);

    let early = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS - 1);
    assert!(early.work.is_none());
    assert!(
        early
            .skipped_reason
            .as_deref()
            .unwrap_or_default()
            .contains("waiting for selected finalizer")
    );

    let recovery = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS);
    let work = recovery.work.expect("recovery work should be prepared");
    let block = work.finish(
        node.wallet.unlocked().unwrap(),
        "preverified-vdf".to_string(),
    );

    assert_eq!(block.finalizer_mode, FinalizerMode::Recovery);
    assert!(block.leader_proof.is_none());
}

#[test]
fn automatic_finalization_respects_zero_recovery_vdf_threshold() {
    let alice = Wallet::from_seed("automatic-recovery-zero-alice");
    let bob = Wallet::from_seed("automatic-recovery-zero-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    let mut node = NodeCore::from_ledger(bob, ledger, 1);
    node.set_recovery_vdf_top_rank_percent(0);

    let recovery = node.prepare_automatic_finalization(RECOVERY_BLOCK_DELAY_MS);

    assert!(recovery.work.is_none());
}

#[test]
fn automatic_non_leader_burn_is_queued_as_blinded() {
    let alice = Wallet::from_seed("auto-blinded-burn-alice");
    let bob = Wallet::from_seed("auto-blinded-burn-bob");
    let carol = Wallet::from_seed("auto-blinded-burn-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        finalizers
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect(),
        1,
    )
    .unwrap();
    assert_eq!(ledger.finalizer_rank_for_next_block(carol.address()), None);
    let mut node =
        NodeCore::from_ledger_with_burn_fee_and_enabled(carol, ledger, true, MICRO_IUNA / 10, 1);

    let plan = node.prepare_automatic_finalization(1);
    let outbox = node.drain_outbox();

    assert!(plan.burned.is_some());
    assert!(node.ledger().pending().is_empty());
    assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
    assert!(
        outbox
            .iter()
            .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
    );
}

#[test]
fn automatic_fallback_finalizer_prepares_anchor_and_blinded_burn() {
    let alice = Wallet::from_seed("auto-fallback-burn-alice");
    let bob = Wallet::from_seed("auto-fallback-burn-bob");
    let finalizers = [alice.clone(), bob.clone()];
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        finalizers
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect(),
        1,
    )
    .unwrap();
    let fallback = finalizers
        .iter()
        .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
        .unwrap()
        .clone();
    let mut node =
        NodeCore::from_ledger_with_burn_fee_and_enabled(fallback, ledger, true, MICRO_IUNA / 10, 1);

    let plan = node.prepare_automatic_finalization(1);
    let outbox = node.drain_outbox();

    assert!(plan.burned.is_some());
    assert!(
        plan.skipped_reason.is_none(),
        "fallback should not be skipped: {:?}",
        plan.skipped_reason
    );
    assert!(node.ledger().pending().is_empty());
    assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
    let (_, anchor_burn) = node
        .local_block_anchor_burn
        .as_ref()
        .expect("fallback anchor burn should be held locally");
    let anchor_signature = anchor_burn.signature().to_string();
    assert_eq!(anchor_burn.amount(), super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT);
    assert!(
        outbox
            .iter()
            .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
    );
    let work = plan.work.expect("fallback work should be prepared");
    let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
    let block = node
        .complete_prepared_block_at(work, vdf_output, VDF_TARGET_BLOCK_MS * 2)
        .unwrap();

    assert_eq!(block.finalizer_rank, 1);
    assert_eq!(block.finalizer_mode, FinalizerMode::Ticket);
    assert!(block.transactions.iter().any(|transaction| {
        transaction.is_burn() && transaction.amount() == super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT
    }));
    assert_eq!(
        block
            .transactions
            .first()
            .map(|transaction| transaction.signature()),
        Some(anchor_signature.as_str())
    );
    assert!(!block.blinded_transactions.is_empty());
    assert_eq!(node.ledger().height(), 1);
}

#[test]
fn automatic_leader_prepares_anchor_and_blinded_burn() {
    let alice = Wallet::from_seed("auto-plaintext-burn-alice");
    let bob = Wallet::from_seed("auto-plaintext-burn-bob");
    let finalizers = [alice.clone(), bob.clone()];
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        finalizers
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect(),
        1,
    )
    .unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let leader_wallet = finalizers
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap()
        .clone();
    for wallet in &finalizers {
        let split = ledger
            .build_transfer(wallet, wallet.address(), MICRO_IUNA, 0)
            .unwrap();
        ledger.submit_transaction(split).unwrap();
    }
    let anchor = ledger.build_burn(&leader_wallet, 1, 0).unwrap();
    ledger.submit_transaction(anchor).unwrap();
    let split_block = ledger.mine_next_block(&leader_wallet, 1).unwrap();
    ledger.apply_block(split_block).unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let leader_wallet = finalizers
        .iter()
        .find(|wallet| wallet.address() == leader)
        .unwrap()
        .clone();
    let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
        leader_wallet,
        ledger,
        true,
        MICRO_IUNA / 10,
        1,
    );

    let plan = node.prepare_automatic_finalization(1);
    let outbox = node.drain_outbox();

    assert!(plan.burned.is_some());
    assert!(node.ledger().pending().is_empty());
    assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
    let (_, anchor_burn) = node
        .local_block_anchor_burn
        .as_ref()
        .expect("leader anchor burn should be held locally");
    assert_eq!(anchor_burn.amount(), super::AUTO_BLOCK_ANCHOR_BURN_AMOUNT);
    assert!(
        outbox
            .iter()
            .any(|envelope| matches!(envelope, GossipEnvelope::BlindedTransaction(_)))
    );
    assert!(node.prepare_automatic_finalization(1).work.is_some());
}

#[test]
fn automatic_pow_mining_uses_protocol_finalizer_fee() {
    let wallet = Wallet::from_seed("automatic-pow-mining-fee-wallet");
    let ledger = Ledger::new_with_genesis_burns(
        BTreeMap::from([(wallet.address().to_string(), MICRO_IUNA)]),
        vec![GenesisBurn::new(wallet.address(), 1)],
        10,
    )
    .unwrap();
    let mut node = NodeCore::from_ledger(wallet, ledger, 0);

    node.set_pow_mining_enabled(true);
    let plan = (1..10_000)
        .map(|timestamp| node.prepare_automatic_mining(timestamp))
        .find(|plan| plan.pow_mined.is_some())
        .expect("bounded PoW search should eventually find a proof");
    let mine = plan.pow_mined.expect("PoW should be queued");

    assert_eq!(mine.fee(), MINE_FINALIZER_FEE);
    assert_eq!(mine.amount(), crate::domain::MINE_REWARD);
    assert_eq!(
        node.status().mining.automatic_pow_mine_fee,
        MINE_FINALIZER_FEE
    );
}
