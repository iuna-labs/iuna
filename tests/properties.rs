use std::collections::{BTreeMap, BTreeSet};

use iuna::{
    app::{GossipEnvelope, InMemoryNetwork, NodeCore},
    domain::{
        Amount, ChainSnapshot, GenesisBurn, Ledger, MAX_BLOCK_BYTES, MICRO_IUNA,
        MINE_FINALIZER_FEE, MINE_REWARD, OutPoint, RECOVERY_BLOCK_DELAY_MS, Transaction, TxInput,
        TxOutput, VDF_TARGET_BLOCK_MS, Wallet, hex_hash, revealed_blinded_transactions, verify_vdf,
    },
};

const LEDGER_PROPERTY_SEEDS: std::ops::Range<u64> = 0..16;
const LEDGER_PROPERTY_ROUNDS: usize = 18;
const NETWORK_PROPERTY_SEEDS: std::ops::Range<u64> = 100..108;
const NETWORK_PROPERTY_ROUNDS: usize = 12;
const TAMPER_PROPERTY_SEEDS: std::ops::Range<u64> = 200..208;
const FORK_PROPERTY_SEEDS: std::ops::Range<u64> = 300..306;
const NETWORK_CHAOS_SEEDS: std::ops::Range<u64> = 400..405;
const NETWORK_CHAOS_ROUNDS: usize = 10;
const CLOCK_SKEW_NETWORK_SEEDS: std::ops::Range<u64> = 600..608;
const CLOCK_SKEW_NETWORK_ROUNDS: usize = 18;
const PARTITION_HEALING_SEEDS: std::ops::Range<u64> = 700..708;
const MULTI_BLOCK_PARTITION_SEEDS: std::ops::Range<u64> = 800..806;
const MULTI_RECOVERY_CANDIDATE_SEEDS: std::ops::Range<u64> = 900..908;
const LATE_JOIN_SYNC_SEEDS: std::ops::Range<u64> = 1_000..1_006;
const FUTURE_TIMESTAMP_SEEDS: std::ops::Range<u64> = 1_100..1_108;
const REORG_MEMPOOL_SEEDS: std::ops::Range<u64> = 1_200..1_208;
const FULL_BLOCK_SELECTION_SEEDS: std::ops::Range<u64> = 1_300..1_302;
const BLINDED_PARTITION_SEEDS: std::ops::Range<u64> = 1_400..1_404;
const BLINDED_EXPIRY_SEEDS: std::ops::Range<u64> = 1_500..1_506;
const SOAK_CHAOS_SEEDS: std::ops::Range<u64> = 1_600..1_603;
const SOAK_CHAOS_ROUNDS: usize = 32;
const VDF_STABILITY_SEEDS: std::ops::Range<u64> = 500..516;
const VDF_STABILITY_BLOCKS: usize = 128;
const VDF_STABILITY_INITIAL_ROUNDS: u64 = 1_000_000;
const TEST_REVEAL_BUNDLE_COLLECTION_MS: u64 = 30_000;

#[derive(Clone, Debug)]
struct TestRng {
    state: u64,
}

impl TestRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    fn index(&mut self, len: usize) -> usize {
        assert!(len > 0);
        (self.next_u64() as usize) % len
    }

    fn amount(&mut self, max_inclusive: Amount) -> Amount {
        1 + self.next_u64() % max_inclusive
    }
}

fn test_wallets(seed: u64, count: usize) -> Vec<Wallet> {
    (0..count)
        .map(|index| Wallet::from_seed(&format!("property-wallet-{seed}-{index}")))
        .collect()
}

fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
    wallets
        .iter()
        .map(|wallet| (wallet.address().to_string(), amount))
        .collect()
}

fn genesis_burns(wallets: &[Wallet], amount: Amount) -> Vec<GenesisBurn> {
    wallets
        .iter()
        .map(|wallet| GenesisBurn::new(wallet.address(), amount))
        .collect()
}

fn queue_plaintext_burn(node: &mut NodeCore, wallet: &Wallet, amount: Amount, fee: Amount) -> bool {
    node.ledger()
        .build_burn(wallet, amount, fee)
        .and_then(|tx| node.receive_transaction(tx).map(|_| ()))
        .is_ok()
}

fn queue_plaintext_transfer(
    node: &mut NodeCore,
    wallet: &Wallet,
    recipient: impl Into<String>,
    amount: Amount,
    fee: Amount,
) -> bool {
    node.ledger()
        .build_transfer(wallet, recipient, amount, fee)
        .and_then(|tx| node.receive_transaction(tx).map(|_| ()))
        .is_ok()
}

fn queue_plaintext_mine(node: &mut NodeCore, wallet: &Wallet) -> bool {
    node.ledger()
        .build_mine(wallet.address())
        .and_then(|tx| node.receive_transaction(tx).map(|_| ()))
        .is_ok()
}

fn property_ledger(seed: u64, wallet_count: usize) -> (Vec<Wallet>, Ledger) {
    let wallets = test_wallets(seed, wallet_count);
    let ledger = Ledger::new_with_genesis_burns(
        allocations(&wallets, 250 * MICRO_IUNA),
        genesis_burns(&wallets, MICRO_IUNA),
        1,
    )
    .expect("property genesis is valid");
    (wallets, ledger)
}

fn single_finalizer_ledger(seed: u64, wallet_count: usize) -> (Vec<Wallet>, Ledger) {
    let wallets = test_wallets(seed, wallet_count);
    let ledger = Ledger::new_with_genesis_burns(
        allocations(&wallets, 250 * MICRO_IUNA),
        vec![GenesisBurn::new(wallets[0].address(), MICRO_IUNA)],
        1,
    )
    .expect("single-finalizer property genesis is valid");
    (wallets, ledger)
}

fn vdf_stability_ledger(seed: u64) -> (Wallet, Ledger) {
    let wallet = Wallet::from_seed(&format!("vdf-stability-{seed}"));
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 250 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
        VDF_STABILITY_INITIAL_ROUNDS,
    )
    .expect("vdf stability genesis is valid");
    (wallet, ledger)
}

fn assert_chain_properties(snapshot: ChainSnapshot) {
    let replayed =
        Ledger::from_snapshot(snapshot.clone()).expect("snapshot replays as valid chain");
    assert_eq!(replayed.snapshot(), snapshot);

    let chain = replayed.chain();
    assert!(!chain.is_empty());
    assert_eq!(chain[0].height, 0);
    assert_eq!(chain[0].prev_hash, "0".repeat(64));
    assert_eq!(chain[0].hash, chain[0].compute_hash());

    for (index, block) in chain.iter().enumerate() {
        assert_eq!(block.height as usize, index);
        assert_eq!(block.hash, block.compute_hash());
        if index > 0 {
            assert_eq!(block.prev_hash, chain[index - 1].hash);
            assert!(
                block.timestamp_ms > chain[index - 1].timestamp_ms,
                "timestamps must increase at height {}",
                block.height
            );
            assert!(
                verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output),
                "VDF must verify at height {}",
                block.height
            );
            assert!(
                block.transactions.iter().any(Transaction::is_burn),
                "non-genesis block must include a burn at height {}",
                block.height
            );
        }
    }

    let confirmed_supply = replayed
        .status()
        .balances
        .values()
        .try_fold(0_u64, |total, amount| total.checked_add(*amount))
        .expect("confirmed supply does not overflow");
    let expected_supply = expected_confirmed_supply(&snapshot);
    assert!(confirmed_supply <= expected_supply);
    if snapshot.blocks.iter().all(|block| {
        block.blinded_transactions.is_empty() && block.all_blinded_reveals().is_empty()
    }) {
        assert_eq!(confirmed_supply, expected_supply);
        assert_reference_model_matches(&snapshot, &replayed);
    }
}

fn expected_confirmed_supply(snapshot: &ChainSnapshot) -> Amount {
    let mut supply = snapshot
        .genesis_allocations
        .values()
        .try_fold(0_u64, |total, amount| total.checked_add(*amount))
        .expect("genesis supply does not overflow");

    for block in &snapshot.blocks {
        for tx in &block.transactions {
            match tx {
                Transaction::Transfer { fee, .. } => {
                    supply = supply.checked_sub(*fee).expect("transfer fee is funded");
                }
                Transaction::Burn { amount, fee, .. } => {
                    supply = supply.checked_sub(*amount).expect("burn is funded");
                    supply = supply.checked_sub(*fee).expect("burn fee is funded");
                }
                Transaction::Mine { .. } => {
                    supply = supply
                        .checked_add(MINE_REWARD)
                        .expect("mine output does not overflow supply");
                }
            }
        }
        supply = supply
            .checked_add(block.reward)
            .expect("block reward does not overflow supply");
    }

    supply
}

fn assert_reference_model_matches(snapshot: &ChainSnapshot, replayed: &Ledger) {
    let reference_balances = reference_balances(snapshot);
    assert_eq!(reference_balances, replayed.status().balances);

    let reference_supply = reference_balances
        .values()
        .try_fold(0_u64, |total, amount| total.checked_add(*amount))
        .expect("reference supply does not overflow");
    assert_eq!(reference_supply, expected_confirmed_supply(snapshot));
}

fn reference_balances(snapshot: &ChainSnapshot) -> BTreeMap<String, Amount> {
    let mut utxos = BTreeMap::new();

    for block in &snapshot.blocks {
        if block.height == 0 {
            seed_reference_genesis_allocations(snapshot, &mut utxos);
        }

        let mut block_signatures = BTreeSet::new();
        let mut block_fees = 0_u64;
        for tx in &block.transactions {
            assert!(
                block_signatures.insert(tx.signature().to_string()),
                "duplicate transaction in block {}",
                block.height
            );
            apply_reference_transaction(tx, &mut utxos);
            block_fees = block_fees
                .checked_add(tx.fee())
                .expect("reference block fees do not overflow");
        }

        if block.height > 0 {
            assert_eq!(block.reward, block_fees);
        }
        if block.reward > 0 {
            let replaced = utxos.insert(
                OutPoint {
                    txid: block.hash.clone(),
                    index: u32::MAX,
                },
                TxOutput {
                    address: block.miner.clone(),
                    amount: block.reward,
                },
            );
            assert!(replaced.is_none(), "duplicate reference reward output");
        }
    }

    balances_from_reference_utxos(&utxos)
}

fn seed_reference_genesis_allocations(
    snapshot: &ChainSnapshot,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) {
    for (address, amount) in &snapshot.genesis_allocations {
        if *amount == 0 {
            continue;
        }
        utxos.insert(
            OutPoint {
                txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
                index: 0,
            },
            TxOutput {
                address: address.clone(),
                amount: *amount,
            },
        );
    }
}

fn apply_reference_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) {
    if let Transaction::Mine { recipient, .. } = transaction {
        let output = TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        };
        assert_eq!(transaction.fee(), MINE_FINALIZER_FEE);
        insert_reference_outputs(transaction, &[output], utxos);
        return;
    }

    let mut seen_inputs = BTreeSet::new();
    let mut input_total = 0_u64;
    for input in reference_inputs(transaction) {
        assert!(
            seen_inputs.insert(input.outpoint.clone()),
            "duplicate reference input"
        );
        let spent = utxos
            .remove(&input.outpoint)
            .expect("reference transaction spends an existing output");
        assert_eq!(spent.address, input.owner);
        input_total = input_total
            .checked_add(spent.amount)
            .expect("reference input total does not overflow");
    }

    let outputs = reference_outputs(transaction);
    let output_total = outputs.iter().fold(0_u64, |total, output| {
        total
            .checked_add(output.amount)
            .expect("reference output total does not overflow")
    });
    let burn_amount = match transaction {
        Transaction::Burn { amount, .. } => *amount,
        Transaction::Transfer { .. } | Transaction::Mine { .. } => 0,
    };
    let required = output_total
        .checked_add(transaction.fee())
        .expect("reference outputs plus fee do not overflow")
        .checked_add(burn_amount)
        .expect("reference outputs plus burn do not overflow");
    assert_eq!(input_total, required);

    insert_reference_outputs(transaction, &outputs, utxos);
}

fn insert_reference_outputs(
    transaction: &Transaction,
    outputs: &[TxOutput],
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) {
    for (index, output) in outputs.iter().enumerate() {
        let replaced = utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
        assert!(replaced.is_none(), "duplicate reference transaction output");
    }
}

fn reference_inputs(transaction: &Transaction) -> &[TxInput] {
    match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => &[],
    }
}

fn reference_outputs(transaction: &Transaction) -> Vec<TxOutput> {
    match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine { recipient, .. } => vec![TxOutput {
            address: recipient.clone(),
            amount: MINE_REWARD,
        }],
    }
}

fn balances_from_reference_utxos(utxos: &BTreeMap<OutPoint, TxOutput>) -> BTreeMap<String, Amount> {
    let mut balances = BTreeMap::new();
    for output in utxos.values() {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance
            .checked_add(output.amount)
            .expect("reference balance does not overflow");
    }
    balances
}

fn random_wallet_pair<'a>(rng: &mut TestRng, wallets: &'a [Wallet]) -> (&'a Wallet, &'a Wallet) {
    let from = rng.index(wallets.len());
    let mut to = rng.index(wallets.len() - 1);
    if to >= from {
        to += 1;
    }
    (&wallets[from], &wallets[to])
}

fn try_random_transaction(
    seed: u64,
    round: usize,
    rng: &mut TestRng,
    wallets: &[Wallet],
    ledger: &mut Ledger,
) {
    match rng.index(5) {
        0 => {
            let (from, to) = random_wallet_pair(rng, wallets);
            let amount = rng.amount(5 * MICRO_IUNA);
            let fee = rng.next_u64() % 4;
            if let Ok(tx) = ledger.build_transfer(from, to.address(), amount, fee) {
                let _ = ledger.submit_transaction(tx);
            }
        }
        1 => {
            let wallet = &wallets[rng.index(wallets.len())];
            let amount = rng.amount(3 * MICRO_IUNA);
            let fee = rng.next_u64() % 4;
            if let Ok(tx) = ledger.build_burn(wallet, amount, fee) {
                let _ = ledger.submit_transaction(tx);
            }
        }
        2 if round % 4 == 0 => {
            let wallet = &wallets[rng.index(wallets.len())];
            if let Ok(tx) = ledger.build_mine(wallet.address()) {
                let _ = ledger.submit_transaction(tx);
            }
        }
        3 => {
            let wallet = &wallets[rng.index(wallets.len())];
            let impossible = (seed + round as u64 + 1) * MICRO_IUNA * 10_000;
            assert!(ledger.build_burn(wallet, impossible, 0).is_err());
        }
        _ => {}
    }
}

fn try_finalize_next_block(round: usize, wallets: &[Wallet], ledger: &mut Ledger) {
    let Some(leader) = ledger.expected_leader_for_next_block() else {
        return;
    };
    let Some(wallet) = wallets.iter().find(|wallet| wallet.address() == leader) else {
        return;
    };

    if let Ok(tx) = ledger.build_burn(wallet, MICRO_IUNA, 0) {
        let _ = ledger.submit_transaction(tx);
    }

    if let Ok(block) = ledger.mine_next_block(wallet, (round + 1) as u64) {
        ledger
            .apply_block(block)
            .expect("locally mined block applies");
    }
}

fn finalize_with_wallet(ledger: &mut Ledger, wallet: &Wallet, timestamp_ms: u64) {
    let burn = ledger
        .build_burn(wallet, MICRO_IUNA, 0)
        .expect("finalizer can build burn");
    let _ = ledger
        .submit_transaction(burn)
        .expect("finalizer burn enters mempool");
    let block = ledger
        .mine_next_block(wallet, timestamp_ms)
        .expect("finalizer can mine next block");
    ledger.apply_block(block).expect("finalizer block applies");
}

fn finalize_preverified_with_wallet(ledger: &mut Ledger, wallet: &Wallet, timestamp_ms: u64) {
    let burn = ledger
        .build_burn(wallet, MICRO_IUNA, 0)
        .expect("finalizer can build burn");
    ledger
        .submit_transaction(burn)
        .expect("finalizer burn enters mempool");
    let work = ledger
        .prepare_next_block(wallet.address(), timestamp_ms)
        .expect("finalizer can prepare next block");
    let block = work.finish(wallet, "property-vdf".to_string());
    ledger
        .apply_locally_mined_block(block)
        .expect("locally mined block applies");
}

fn next_ticket_slot_timestamp(ledger: &Ledger, offset_ms: u64) -> u64 {
    ledger
        .chain()
        .last()
        .expect("ledger has genesis")
        .timestamp_ms
        .saturating_add(VDF_TARGET_BLOCK_MS)
        .saturating_add(offset_ms)
}

fn finalize_many(ledger: &mut Ledger, wallet: &Wallet, count: usize, start_timestamp_ms: u64) {
    for offset in 0..count {
        let timestamp_ms = next_ticket_slot_timestamp(ledger, start_timestamp_ms + offset as u64);
        finalize_with_wallet(ledger, wallet, timestamp_ms);
    }
}

#[test]
#[ignore = "long-running VDF retarget stability property"]
fn generated_vdf_retarget_stays_stable_under_noisy_block_times() {
    for seed in VDF_STABILITY_SEEDS {
        let (wallet, mut ledger) = vdf_stability_ledger(seed);
        let mut rng = TestRng::new(seed);
        let mut timestamp_ms = 0_u64;
        let mut min_rounds = ledger.vdf_rounds();
        let mut max_rounds = ledger.vdf_rounds();
        let mut previous_rounds = ledger.vdf_rounds();
        let mut pair_jitter_ms = 0_u64;

        for block_index in 0..VDF_STABILITY_BLOCKS {
            let interval_ms = if block_index == 0 {
                VDF_TARGET_BLOCK_MS
            } else if block_index % 2 == 1 {
                pair_jitter_ms = rng.next_u64() % (VDF_TARGET_BLOCK_MS / 4 + 1);
                VDF_TARGET_BLOCK_MS.saturating_sub(pair_jitter_ms)
            } else {
                VDF_TARGET_BLOCK_MS + pair_jitter_ms
            };
            timestamp_ms = timestamp_ms
                .checked_add(interval_ms)
                .expect("property timestamp does not overflow");

            finalize_preverified_with_wallet(&mut ledger, &wallet, timestamp_ms);

            let rounds = ledger.vdf_rounds();
            let max_step = (previous_rounds * 2 / 100).max(1);
            assert!(
                rounds.abs_diff(previous_rounds) <= max_step,
                "seed {seed} block {block_index}: VDF rounds changed from {previous_rounds} to {rounds}, above max step {max_step}"
            );
            min_rounds = min_rounds.min(rounds);
            max_rounds = max_rounds.max(rounds);
            previous_rounds = rounds;
        }

        let lower_bound = VDF_STABILITY_INITIAL_ROUNDS * 95 / 100;
        let upper_bound = VDF_STABILITY_INITIAL_ROUNDS * 105 / 100;
        assert!(
            min_rounds >= lower_bound && max_rounds <= upper_bound,
            "seed {seed}: VDF rounds drifted outside stability band: min {min_rounds}, max {max_rounds}"
        );
    }
}

#[test]
#[ignore = "long-running snapshot replay property"]
fn generated_chain_snapshots_preserve_core_invariants() {
    for seed in LEDGER_PROPERTY_SEEDS {
        let (wallets, mut ledger) = property_ledger(seed, 4);
        let mut rng = TestRng::new(seed);

        assert_chain_properties(ledger.snapshot());
        for round in 0..LEDGER_PROPERTY_ROUNDS {
            try_random_transaction(seed, round, &mut rng, &wallets, &mut ledger);
            if round % 2 == 0 {
                try_finalize_next_block(round, &wallets, &mut ledger);
            }
            assert_chain_properties(ledger.snapshot());
        }
    }
}

#[test]
fn generated_forks_reorg_only_inside_finality_and_preserve_local_transactions() {
    for seed in FORK_PROPERTY_SEEDS {
        let (wallets, mut common) = single_finalizer_ledger(seed, 3);
        let finalizer = &wallets[0];
        let sender = &wallets[1];
        let recipient = &wallets[2];
        let mut rng = TestRng::new(seed);

        finalize_many(&mut common, finalizer, 2 + rng.index(2), 1);
        assert_chain_properties(common.snapshot());

        let mut local = common.clone();
        let abandoned = local
            .build_transfer(sender, recipient.address(), MICRO_IUNA + rng.amount(5), 0)
            .expect("abandoned fork transfer builds");
        local
            .submit_transaction(abandoned.clone())
            .expect("abandoned fork transfer enters mempool");
        finalize_many(&mut local, finalizer, 1 + rng.index(2), 20);

        let mut remote = common.clone();
        finalize_many(&mut remote, finalizer, 4 + rng.index(3), 100);

        let remote_tip = remote.status().tip_hash;
        assert!(
            local
                .extend_from_snapshot(remote.snapshot())
                .expect("valid fresh fork import succeeds"),
            "longer fresh fork should be accepted"
        );
        assert_eq!(local.status().tip_hash, remote_tip);
        assert!(
            local
                .pending()
                .iter()
                .any(|tx| tx.signature() == abandoned.signature()),
            "transactions mined only on the abandoned fork should return to the mempool"
        );
        assert_chain_properties(local.snapshot());

        let (wallets, mut common) = single_finalizer_ledger(seed + 10_000, 2);
        let finalizer = &wallets[0];
        finalize_with_wallet(&mut common, finalizer, 1);

        let mut finalized_local = common.clone();
        finalize_many(&mut finalized_local, finalizer, 8 + rng.index(2), 10);
        let finalized_tip = finalized_local.status().tip_hash;

        let mut too_old_remote = common;
        finalize_many(&mut too_old_remote, finalizer, 12 + rng.index(2), 200);

        assert!(
            !finalized_local
                .extend_from_snapshot(too_old_remote.snapshot())
                .expect("valid old fork import is evaluated"),
            "forks that rewrite finalized history must be rejected"
        );
        assert_eq!(finalized_local.status().tip_hash, finalized_tip);
        assert_chain_properties(finalized_local.snapshot());
    }
}

#[test]
#[ignore = "long-running generated network convergence property"]
fn in_memory_network_converges_under_generated_node_actions() {
    for seed in NETWORK_PROPERTY_SEEDS {
        let (wallets, ledger) = property_ledger(seed, 3);
        let mut network = InMemoryNetwork::default();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            network.insert(
                format!("n{index}"),
                NodeCore::from_ledger_with_burn_fee_and_enabled(
                    wallet.clone(),
                    joined,
                    true,
                    MICRO_IUNA,
                    0,
                ),
            );
        }

        let mut rng = TestRng::new(seed);
        network
            .deliver_until_idle()
            .expect("initial network delivery succeeds");

        for round in 0..NETWORK_PROPERTY_ROUNDS {
            let node_index = rng.index(wallets.len());
            let node_id = format!("n{node_index}");
            let recipient = wallets[rng.index(wallets.len())].address().to_string();

            match rng.index(4) {
                0 => {
                    let _ = queue_plaintext_transfer(
                        network.node_mut(&node_id).expect("node exists"),
                        &wallets[node_index],
                        recipient,
                        rng.amount(2 * MICRO_IUNA),
                        0,
                    );
                }
                1 => {
                    let _ = queue_plaintext_burn(
                        network.node_mut(&node_id).expect("node exists"),
                        &wallets[node_index],
                        MICRO_IUNA,
                        0,
                    );
                }
                2 => {
                    let _ = queue_plaintext_mine(
                        network.node_mut(&node_id).expect("node exists"),
                        &wallets[node_index],
                    );
                }
                _ => {}
            }

            network
                .deliver_until_idle()
                .expect("transaction gossip converges");

            let leader = network
                .node("n0")
                .expect("anchor node exists")
                .ledger()
                .expected_leader_for_next_block();
            if let Some(leader) = leader {
                if let Some((leader_index, _)) = wallets
                    .iter()
                    .enumerate()
                    .find(|(_, wallet)| wallet.address() == leader)
                {
                    let timestamp_ms = (round + 1) as u64;
                    let mut outcome = network
                        .node_mut(&format!("n{leader_index}"))
                        .expect("leader node exists")
                        .automatic_mine_once(timestamp_ms);
                    if outcome
                        .skipped_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("collecting blinded reveals"))
                    {
                        outcome = network
                            .node_mut(&format!("n{leader_index}"))
                            .expect("leader node exists")
                            .automatic_mine_once(
                                timestamp_ms.saturating_add(TEST_REVEAL_BUNDLE_COLLECTION_MS + 1),
                            );
                    }
                    if let Some(reason) = outcome.skipped_reason {
                        assert!(
                            reason.contains("at least one burn")
                                || reason.contains("selected finalizer")
                                || reason.contains("required burn")
                                || reason.contains("could not")
                                || reason.contains("automatic"),
                            "unexpected mining skip reason: {reason}"
                        );
                    }
                }
            }

            network
                .deliver_until_idle()
                .expect("block gossip converges");
            assert_network_converged(&network, wallets.len());
        }
    }
}

fn assert_network_converged(network: &InMemoryNetwork, nodes: usize) {
    let first = network.node("n0").expect("first node exists");
    let height = first.chain_height();
    let tip = first.ledger().status().tip_hash;
    assert_chain_properties(first.chain_snapshot());

    for index in 1..nodes {
        let node = network.node(&format!("n{index}")).expect("node exists");
        assert_eq!(node.chain_height(), height, "node {index} height diverged");
        assert_eq!(
            node.ledger().status().tip_hash,
            tip,
            "node {index} tip diverged"
        );
        assert_chain_properties(node.chain_snapshot());
    }
}

#[test]
#[ignore = "long-running generated clock skew property"]
fn in_memory_network_survives_generated_clock_skew() {
    for seed in CLOCK_SKEW_NETWORK_SEEDS {
        let (wallets, ledger) = property_ledger(seed, 4);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = InMemoryNetwork::default();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                wallet.clone(),
                joined,
                true,
                MICRO_IUNA,
                0,
            );
            node.set_recovery_vdf_top_rank_percent(100);
            network.insert(&node_ids[index], node);
        }

        let mut rng = TestRng::new(seed);
        let skews = (0..wallets.len())
            .map(|index| {
                let magnitude = (rng.next_u64() % (VDF_TARGET_BLOCK_MS * 2 + 1)) as i64;
                if index % 2 == 0 {
                    magnitude
                } else {
                    -magnitude
                }
            })
            .collect::<Vec<_>>();

        network
            .deliver_until_idle()
            .expect("initial network delivery succeeds");

        for round in 0..CLOCK_SKEW_NETWORK_ROUNDS {
            let actor_index = rng.index(wallets.len());
            let actor_id = &node_ids[actor_index];
            match rng.index(4) {
                0 => {
                    let recipient = wallets[rng.index(wallets.len())].address().to_string();
                    let _ = queue_plaintext_transfer(
                        network.node_mut(actor_id).expect("actor node exists"),
                        &wallets[actor_index],
                        recipient,
                        rng.amount(MICRO_IUNA),
                        0,
                    );
                }
                1 => {
                    let _ = queue_plaintext_burn(
                        network.node_mut(actor_id).expect("actor node exists"),
                        &wallets[actor_index],
                        MICRO_IUNA,
                        0,
                    );
                }
                _ => {}
            }

            network
                .deliver_until_idle()
                .expect("transaction gossip survives skewed producers");

            let base_timestamp = network
                .node("n0")
                .expect("anchor node exists")
                .ledger()
                .chain()
                .last()
                .expect("anchor chain has a tip")
                .timestamp_ms
                .saturating_add(1 + (round as u64 % 3));
            let leader = network
                .node("n0")
                .expect("anchor node exists")
                .ledger()
                .expected_leader_for_next_block();

            if let Some(leader) = leader {
                if let Some((leader_index, _)) = wallets
                    .iter()
                    .enumerate()
                    .find(|(_, wallet)| wallet.address() == leader)
                {
                    let skewed_timestamp = skew_timestamp(base_timestamp, skews[leader_index]);
                    let mut outcome = network
                        .node_mut(&node_ids[leader_index])
                        .expect("leader node exists")
                        .automatic_mine_once(skewed_timestamp);
                    if outcome
                        .skipped_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("collecting blinded reveals"))
                    {
                        outcome = network
                            .node_mut(&node_ids[leader_index])
                            .expect("leader node exists")
                            .automatic_mine_once(
                                skewed_timestamp
                                    .saturating_add(TEST_REVEAL_BUNDLE_COLLECTION_MS + 1),
                            );
                    }
                    if let Some(reason) = outcome.skipped_reason {
                        assert!(
                            expected_clock_skew_skip_reason(&reason),
                            "unexpected skewed mining skip reason: {reason}"
                        );
                    }
                }
            }

            network
                .deliver_until_idle()
                .expect("block gossip survives skewed producers");

            if round % 4 == 3 {
                mine_expected_leader_with_network_time(&mut network, &node_ids, &wallets, round);
                network
                    .deliver_until_idle()
                    .expect("network-time recovery block gossip converges");
                assert_network_converged(&network, wallets.len());
            }
        }

        mine_expected_leader_with_network_time(
            &mut network,
            &node_ids,
            &wallets,
            CLOCK_SKEW_NETWORK_ROUNDS,
        );
        network
            .deliver_until_idle()
            .expect("final network-time block gossip converges");
        assert_network_converged(&network, wallets.len());
    }
}

#[test]
fn in_memory_network_heals_generated_ticket_recovery_partitions() {
    for seed in PARTITION_HEALING_SEEDS {
        let (wallets, ledger) = single_finalizer_ledger(seed, 3);
        let mut network = InMemoryNetwork::default();
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                wallet.clone(),
                joined,
                true,
                MICRO_IUNA,
                0,
            );
            node.set_recovery_vdf_top_rank_percent(100);
            network.insert(&node_ids[index], node);
        }

        network
            .deliver_until_idle()
            .expect("initial network delivery succeeds");

        let mut rng = TestRng::new(seed);
        let partition_a_timestamp = 1 + rng.next_u64() % VDF_TARGET_BLOCK_MS;
        let ticket = network
            .node_mut("n0")
            .expect("ticket finalizer exists")
            .automatic_mine_once(partition_a_timestamp);
        assert!(
            ticket.block.is_some(),
            "ticket side should finalize while partitioned"
        );

        let recovery_index = 1 + rng.index(wallets.len() - 1);
        let recovery_id = &node_ids[recovery_index];
        let recovery_timestamp = RECOVERY_BLOCK_DELAY_MS + rng.next_u64() % VDF_TARGET_BLOCK_MS;
        let recovery = network
            .node_mut(recovery_id)
            .expect("recovery finalizer exists")
            .automatic_mine_once(recovery_timestamp);
        assert!(
            recovery.block.is_some(),
            "recovery side should finalize while partitioned"
        );

        let ticket_tip = network
            .node("n0")
            .expect("ticket node exists")
            .ledger()
            .status()
            .tip_hash;
        let recovery_tip = network
            .node(recovery_id)
            .expect("recovery node exists")
            .ledger()
            .status()
            .tip_hash;
        assert_ne!(
            ticket_tip, recovery_tip,
            "partitioned groups should have diverged before reconnect"
        );

        let ticket_snapshot = network
            .node("n0")
            .expect("ticket node exists")
            .chain_snapshot();
        for id in node_ids.iter().skip(1) {
            network
                .node_mut(id)
                .expect("partition peer exists")
                .receive(GossipEnvelope::ChainSnapshot(ticket_snapshot.clone()))
                .expect("partition peer imports better ticket snapshot");
        }

        drain_all_outboxes(&mut network, &node_ids);
        assert_network_converged(&network, wallets.len());
        assert_eq!(
            network
                .node("n0")
                .expect("ticket node exists")
                .ledger()
                .status()
                .tip_hash,
            ticket_tip,
            "ticket fork should win over same-height recovery fork"
        );
    }
}

#[test]
fn in_memory_network_heals_generated_multi_block_partitions() {
    for seed in MULTI_BLOCK_PARTITION_SEEDS {
        let (wallets, ledger) = property_ledger(seed, 5);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = network_from_ledger(&wallets, &ledger);
        let mut rng = TestRng::new(seed);

        let partition_a = vec![0, 1, 2];
        let partition_b = vec![3, 4];
        for round in 0..3 {
            mine_one_partition_block(
                &mut network,
                &node_ids,
                &wallets,
                &partition_a,
                round,
                &mut rng,
            );
        }
        for round in 0..2 {
            mine_one_partition_block(
                &mut network,
                &node_ids,
                &wallets,
                &partition_b,
                round + 10,
                &mut rng,
            );
        }

        let partition_a_tip = network
            .node("n0")
            .expect("partition A anchor exists")
            .ledger()
            .status()
            .tip_hash;
        let partition_b_tip = network
            .node("n3")
            .expect("partition B anchor exists")
            .ledger()
            .status()
            .tip_hash;
        assert_ne!(
            partition_a_tip, partition_b_tip,
            "partitioned chains should diverge before reconnect"
        );
        assert!(
            network
                .node("n0")
                .expect("partition A anchor exists")
                .chain_height()
                > network
                    .node("n3")
                    .expect("partition B anchor exists")
                    .chain_height(),
            "partition A should be the taller reconnect candidate"
        );

        let taller_snapshot = network
            .node("n0")
            .expect("partition A anchor exists")
            .chain_snapshot();
        for id in &node_ids {
            network
                .node_mut(id)
                .expect("node exists")
                .receive(GossipEnvelope::ChainSnapshot(taller_snapshot.clone()))
                .expect("node imports taller partition snapshot");
        }

        drain_all_outboxes(&mut network, &node_ids);
        assert_network_converged(&network, wallets.len());
        assert_eq!(
            network
                .node("n3")
                .expect("partition B anchor exists")
                .ledger()
                .status()
                .tip_hash,
            partition_a_tip,
            "shorter partition should switch to taller chain"
        );
    }
}

#[test]
fn in_memory_network_converges_with_generated_multiple_recovery_candidates() {
    for seed in MULTI_RECOVERY_CANDIDATE_SEEDS {
        let (wallets, ledger) = single_finalizer_ledger(seed, 4);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = network_from_ledger(&wallets, &ledger);
        let mut rng = TestRng::new(seed);
        let mut recovery_tips = BTreeSet::new();

        for (index, node_id) in node_ids.iter().enumerate().skip(1) {
            let timestamp_ms = RECOVERY_BLOCK_DELAY_MS
                .saturating_add(1)
                .saturating_add(rng.next_u64() % VDF_TARGET_BLOCK_MS);
            let outcome = network
                .node_mut(node_id)
                .expect("recovery candidate exists")
                .automatic_mine_once(timestamp_ms);
            assert!(
                outcome.block.is_some(),
                "unranked node {index} should produce a recovery candidate"
            );
            recovery_tips.insert(
                network
                    .node(node_id)
                    .expect("recovery candidate exists")
                    .ledger()
                    .status()
                    .tip_hash,
            );
        }
        assert!(
            recovery_tips.len() > 1,
            "generated recovery candidates should create competing same-height forks"
        );

        let winning_id = node_ids[1].clone();
        let candidate_snapshots = node_ids
            .iter()
            .skip(1)
            .map(|id| {
                network
                    .node(id)
                    .expect("candidate node exists")
                    .chain_snapshot()
            })
            .collect::<Vec<_>>();
        for snapshot in candidate_snapshots {
            network
                .node_mut(&winning_id)
                .expect("winning candidate exists")
                .receive(GossipEnvelope::ChainSnapshot(snapshot))
                .expect("candidate fork choice accepts recovery snapshot");
        }
        let winning_snapshot = network
            .node(&winning_id)
            .expect("winning candidate exists")
            .chain_snapshot();
        let winning_tip = network
            .node(&winning_id)
            .expect("winning candidate exists")
            .ledger()
            .status()
            .tip_hash;

        for id in node_ids.iter().skip(1) {
            network
                .node_mut(id)
                .expect("candidate node exists")
                .receive(GossipEnvelope::ChainSnapshot(winning_snapshot.clone()))
                .expect("candidate imports best recovery snapshot");
        }

        for id in node_ids.iter().skip(1) {
            let node = network.node(id).expect("candidate node exists");
            assert_eq!(
                node.chain_height(),
                1,
                "{id} should stay at recovery height"
            );
            assert_eq!(
                node.ledger().status().tip_hash,
                winning_tip,
                "{id} should converge to the best recovery candidate"
            );
            assert_chain_properties(node.chain_snapshot());
        }
    }
}

#[test]
#[ignore = "long-running late join sync property"]
fn in_memory_network_syncs_generated_late_joiners_from_genesis() {
    for seed in LATE_JOIN_SYNC_SEEDS {
        let (wallets, ledger) = single_finalizer_ledger(seed, 3);
        let mut network = network_from_ledger(&wallets[0..1], &ledger);
        let mut rng = TestRng::new(seed);
        let produced_blocks = 24 + rng.index(12);

        for round in 0..produced_blocks {
            mine_one_partition_block(
                &mut network,
                &["n0".to_string()],
                &wallets,
                &[0],
                round,
                &mut rng,
            );
        }

        assert_eq!(
            network.node("n0").expect("producer exists").chain_height(),
            produced_blocks as u64
        );

        for (index, wallet) in wallets.iter().enumerate().skip(1) {
            let joined =
                Ledger::from_snapshot(ledger.snapshot()).expect("late node starts at genesis");
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                wallet.clone(),
                joined,
                true,
                MICRO_IUNA,
                0,
            );
            node.set_recovery_vdf_top_rank_percent(100);
            network.insert(format!("n{index}"), node);
        }

        for index in 1..wallets.len() {
            let id = format!("n{index}");
            sync_until_idle(&mut network, "n0", &id, 3);
        }
        network
            .deliver_until_idle()
            .expect("late joiner block gossip converges");
        assert_network_converged(&network, wallets.len());
    }
}

#[test]
fn in_memory_network_rejects_generated_future_timestamp_blocks_without_stalling() {
    for seed in FUTURE_TIMESTAMP_SEEDS {
        let (wallets, ledger) = single_finalizer_ledger(seed, 2);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = network_from_ledger(&wallets, &ledger);

        queue_plaintext_burn(
            network.node_mut("n0").expect("producer exists"),
            &wallets[0],
            MICRO_IUNA,
            0,
        );
        let future_block = network
            .node("n0")
            .expect("producer exists")
            .ledger()
            .mine_next_block(&wallets[0], u64::MAX)
            .expect("producer can build future-dated candidate");
        let rejected = network
            .node_mut("n1")
            .expect("receiver exists")
            .receive(GossipEnvelope::Block(future_block))
            .expect_err("future-dated block should be rejected");
        assert!(
            rejected.to_string().contains("too far in the future"),
            "unexpected future-block rejection: {rejected:#}"
        );
        assert_eq!(
            network.node("n1").expect("receiver exists").chain_height(),
            0,
            "receiver should keep its local chain after future-block rejection"
        );

        let valid_block = network
            .node("n0")
            .expect("producer exists")
            .ledger()
            .mine_next_block(&wallets[0], VDF_TARGET_BLOCK_MS)
            .expect("producer can build valid block after future rejection");
        network
            .node_mut("n0")
            .expect("producer exists")
            .receive(GossipEnvelope::Block(valid_block))
            .expect("producer applies valid block after future rejection");
        let valid_snapshot = network
            .node("n0")
            .expect("producer exists")
            .chain_snapshot();
        network
            .node_mut("n1")
            .expect("receiver exists")
            .receive(GossipEnvelope::ChainSnapshot(valid_snapshot))
            .expect("receiver should still import a later valid chain");
        drain_all_outboxes(&mut network, &node_ids);
        assert_network_converged(&network, wallets.len());
    }
}

#[test]
fn generated_reorgs_preserve_valid_mempool_transactions() {
    for seed in REORG_MEMPOOL_SEEDS {
        let wallets = test_wallets(seed, 3);
        let mut common = Ledger::new_with_genesis_burns(
            allocations(&wallets, 50 * MICRO_IUNA),
            vec![GenesisBurn::new(wallets[0].address(), MICRO_IUNA)],
            1,
        )
        .expect("reorg mempool genesis is valid");
        finalize_with_wallet(&mut common, &wallets[0], VDF_TARGET_BLOCK_MS);

        let mut local = common.clone();
        let abandoned_transfer = local
            .build_transfer(&wallets[1], wallets[2].address(), MICRO_IUNA, 0)
            .expect("abandoned fork transfer builds");
        local
            .submit_transaction(abandoned_transfer.clone())
            .expect("abandoned fork transfer enters mempool");
        finalize_with_wallet(&mut local, &wallets[0], VDF_TARGET_BLOCK_MS * 2);

        let surviving_transfer = local
            .build_transfer(&wallets[2], wallets[1].address(), MICRO_IUNA, 0)
            .expect("local pending transfer builds");
        local
            .submit_transaction(surviving_transfer.clone())
            .expect("local pending transfer enters mempool");

        let mut remote = common;
        for height in 2..=5 {
            finalize_with_wallet(
                &mut remote,
                &wallets[0],
                VDF_TARGET_BLOCK_MS.saturating_mul(height),
            );
        }

        assert!(
            local
                .extend_from_snapshot(remote.snapshot())
                .expect("longer remote snapshot is evaluated"),
            "longer fork should replace local fork"
        );
        let pending_signatures = local
            .pending()
            .iter()
            .map(Transaction::signature)
            .collect::<BTreeSet<_>>();
        assert!(
            pending_signatures.contains(abandoned_transfer.signature()),
            "transaction mined only on abandoned fork should return to mempool"
        );
        assert!(
            pending_signatures.contains(surviving_transfer.signature()),
            "valid local pending transaction should survive reorg"
        );
        assert_chain_properties(local.snapshot());
    }
}

#[test]
fn generated_full_block_selection_stays_valid_and_bounded() {
    for seed in FULL_BLOCK_SELECTION_SEEDS {
        let wallets = test_wallets(seed, 40);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations(&wallets, 20 * MICRO_IUNA),
            vec![GenesisBurn::new(wallets[0].address(), MICRO_IUNA)],
            1,
        )
        .expect("full block genesis is valid");
        let mut rng = TestRng::new(seed);

        for wallet in wallets.iter().skip(1) {
            let recipient = wallets[rng.index(wallets.len())].address().to_string();
            if let Ok(tx) = ledger.build_transfer(wallet, recipient, MICRO_IUNA, rng.amount(9)) {
                let _ = ledger.submit_transaction(tx);
            }
        }
        let anchor = ledger
            .build_burn(&wallets[0], MICRO_IUNA, 0)
            .expect("anchor burn builds");
        ledger
            .submit_transaction(anchor)
            .expect("anchor burn enters mempool");

        let block = ledger
            .mine_next_block(&wallets[0], VDF_TARGET_BLOCK_MS)
            .expect("full pending pool can produce a bounded block");
        assert!(
            block.serialized_size_bytes().expect("block serializes") <= MAX_BLOCK_BYTES,
            "selected block should fit max block bytes"
        );
        assert!(
            block.transactions.iter().any(Transaction::is_burn),
            "full block should retain required burn"
        );
        assert!(
            block.transactions.len() > 1,
            "selection should include more than the required anchor when space allows"
        );
        ledger
            .apply_block(block)
            .expect("bounded full block applies");
        assert_chain_properties(ledger.snapshot());
    }
}

#[test]
fn generated_blinded_commit_reveal_survives_partition_and_reconnect() {
    for seed in BLINDED_PARTITION_SEEDS {
        let finalizer = Wallet::from_seed(&format!("blinded-partition-finalizer-{seed}"));
        let sender = Wallet::from_seed(&format!("blinded-partition-sender-{seed}"));
        let observer = Wallet::from_seed(&format!("blinded-partition-observer-{seed}"));
        let wallets = vec![finalizer.clone(), sender.clone(), observer.clone()];
        let ledger = Ledger::new_with_genesis_burns(
            allocations(&wallets, 100 * MICRO_IUNA),
            vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
            1,
        )
        .expect("blinded partition genesis is valid");
        let mut network = network_from_ledger(&wallets, &ledger);

        network
            .node_mut("n1")
            .expect("sender exists")
            .burn(MICRO_IUNA)
            .expect("sender creates owned blinded burn");
        let sender_outbox = network
            .node_mut("n1")
            .expect("sender exists")
            .drain_outbox();
        for envelope in sender_outbox {
            network
                .node_mut("n0")
                .expect("finalizer exists")
                .receive(envelope)
                .expect("finalizer receives blinded commit before partition");
        }

        queue_plaintext_burn(
            network.node_mut("n0").expect("finalizer exists"),
            &finalizer,
            MICRO_IUNA,
            0,
        );
        let commit_block = network
            .node_mut("n0")
            .expect("finalizer exists")
            .mine_one_at(VDF_TARGET_BLOCK_MS)
            .expect("finalizer mines blinded commit block");
        assert_eq!(commit_block.blinded_transactions.len(), 1);

        network
            .node_mut("n1")
            .expect("sender exists")
            .receive(GossipEnvelope::Block(commit_block.clone()))
            .expect("sender imports commit block while reveal path is partitioned");
        assert_eq!(
            network
                .node("n1")
                .expect("sender exists")
                .ledger()
                .pending_blinded_reveals()
                .len(),
            1,
            "sender should publish reveal after seeing its commit"
        );

        network
            .deliver_until_idle()
            .expect("reconnect should gossip delayed reveal");
        queue_plaintext_burn(
            network.node_mut("n0").expect("finalizer exists"),
            &finalizer,
            MICRO_IUNA,
            0,
        );
        network
            .gossip_mempools_once()
            .expect("reveal gossip succeeds");
        let reveal_block = network
            .node_mut("n0")
            .expect("finalizer exists")
            .mine_one_at(VDF_TARGET_BLOCK_MS * 2)
            .expect("finalizer mines reveal block after reconnect");
        assert_eq!(reveal_block.all_blinded_reveals().len(), 1);
        network
            .deliver_until_idle()
            .expect("reveal block gossip converges");
        let revealed = revealed_blinded_transactions(
            &network
                .node("n0")
                .expect("finalizer exists")
                .chain_snapshot(),
        )
        .expect("revealed history is reconstructed");
        assert!(
            revealed
                .iter()
                .any(|revealed| revealed.height == reveal_block.height
                    && revealed.transaction.is_burn()
                    && revealed.transaction.sender() == sender.address()),
            "blinded burn should reveal after reconnect"
        );
        assert_network_converged(&network, wallets.len());
    }
}

#[test]
fn generated_expired_blinded_transactions_are_pruned_under_progress() {
    for seed in BLINDED_EXPIRY_SEEDS {
        let wallets = test_wallets(seed, 3);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations(&wallets, 40 * MICRO_IUNA),
            vec![GenesisBurn::new(wallets[0].address(), MICRO_IUNA)],
            1,
        )
        .expect("blinded expiry genesis is valid");

        let blinded = ledger
            .build_blinded_burn(&wallets[1], MICRO_IUNA, 0, 2)
            .expect("short-lived blinded burn builds");
        ledger
            .submit_blinded_transaction(blinded.transaction)
            .expect("short-lived blinded burn enters mempool");
        assert_eq!(ledger.pending_blinded_transactions().len(), 1);

        for height in 1..=3 {
            finalize_with_wallet(
                &mut ledger,
                &wallets[0],
                VDF_TARGET_BLOCK_MS.saturating_mul(height),
            );
        }
        assert!(
            ledger.pending_blinded_transactions().is_empty(),
            "expired pending blinded transaction should be pruned as blocks progress"
        );
        assert_chain_properties(ledger.snapshot());
    }
}

#[test]
#[ignore = "long-running testnet soak property"]
fn in_memory_network_soak_generated_chaos() {
    for seed in SOAK_CHAOS_SEEDS {
        let (wallets, ledger) = property_ledger(seed, 5);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = network_from_ledger(&wallets, &ledger);
        let mut rng = TestRng::new(seed);

        for round in 0..SOAK_CHAOS_ROUNDS {
            let mut offline = BTreeSet::new();
            if round % 5 == 1 {
                offline.insert(node_ids[rng.index(node_ids.len())].clone());
            }
            if round % 7 == 3 {
                offline.insert(node_ids[rng.index(node_ids.len())].clone());
            }

            let actor = rng.index(wallets.len());
            match rng.index(6) {
                0 => {
                    let recipient = wallets[rng.index(wallets.len())].address().to_string();
                    let _ = queue_plaintext_transfer(
                        network
                            .node_mut(&node_ids[actor])
                            .expect("actor node exists"),
                        &wallets[actor],
                        recipient,
                        rng.amount(MICRO_IUNA),
                        rng.next_u64() % 3,
                    );
                }
                1 => {
                    let _ = queue_plaintext_burn(
                        network
                            .node_mut(&node_ids[actor])
                            .expect("actor node exists"),
                        &wallets[actor],
                        MICRO_IUNA,
                        rng.next_u64() % 3,
                    );
                }
                2 => {
                    let expiry_height = network
                        .node(&node_ids[actor])
                        .expect("actor node exists")
                        .chain_height()
                        + 8;
                    let recipient = wallets[rng.index(wallets.len())].address().to_string();
                    let _ = network
                        .node_mut(&node_ids[actor])
                        .expect("actor node exists")
                        .blinded_transfer_with_fee(recipient, 1, 0, expiry_height);
                }
                _ => {}
            }

            deliver_chaos_until_idle(&mut network, &node_ids, &offline, &mut rng);
            let online = node_ids
                .iter()
                .enumerate()
                .filter_map(|(index, id)| (!offline.contains(id)).then_some(index))
                .collect::<Vec<_>>();
            if !online.is_empty() {
                let _ = try_mine_one_partition_block(
                    &mut network,
                    &node_ids,
                    &wallets,
                    &online,
                    round,
                    &mut rng,
                );
            }
            deliver_chaos_until_idle(&mut network, &node_ids, &offline, &mut rng);
        }

        let best_id = node_ids
            .iter()
            .max_by_key(|id| network.node(id).expect("node exists").chain_height())
            .expect("network has nodes")
            .clone();
        let best_snapshot = network
            .node(&best_id)
            .expect("best node exists")
            .chain_snapshot();
        for id in &node_ids {
            network
                .node_mut(id)
                .expect("node exists")
                .receive(GossipEnvelope::ChainSnapshot(best_snapshot.clone()))
                .expect("node imports best soak snapshot");
        }
        drain_all_outboxes(&mut network, &node_ids);
        assert_network_converged(&network, wallets.len());
    }
}

fn network_from_ledger(wallets: &[Wallet], ledger: &Ledger) -> InMemoryNetwork {
    let mut network = InMemoryNetwork::default();
    for (index, wallet) in wallets.iter().enumerate() {
        let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            wallet.clone(),
            joined,
            true,
            MICRO_IUNA,
            0,
        );
        node.set_recovery_vdf_top_rank_percent(100);
        network.insert(format!("n{index}"), node);
    }
    network
}

fn skew_timestamp(base_timestamp: u64, skew_ms: i64) -> u64 {
    if skew_ms >= 0 {
        base_timestamp.saturating_add(skew_ms as u64)
    } else {
        base_timestamp.saturating_sub(skew_ms.unsigned_abs())
    }
}

fn expected_clock_skew_skip_reason(reason: &str) -> bool {
    reason.contains("at least one burn")
        || reason.contains("selected finalizer")
        || reason.contains("required burn")
        || reason.contains("could not")
        || reason.contains("automatic")
        || reason.contains("block timestamp")
        || reason.contains("before finalizer rank")
        || reason.contains("collecting blinded reveals")
}

fn mine_expected_leader_with_network_time(
    network: &mut InMemoryNetwork,
    node_ids: &[String],
    wallets: &[Wallet],
    round: usize,
) {
    let Some(leader) = network
        .node("n0")
        .expect("anchor node exists")
        .ledger()
        .expected_leader_for_next_block()
    else {
        return;
    };
    let Some((leader_index, _)) = wallets
        .iter()
        .enumerate()
        .find(|(_, wallet)| wallet.address() == leader)
    else {
        return;
    };
    let timestamp_ms = network
        .node("n0")
        .expect("anchor node exists")
        .ledger()
        .chain()
        .last()
        .expect("anchor chain has a tip")
        .timestamp_ms
        .saturating_add(VDF_TARGET_BLOCK_MS + round as u64 + 1);
    let mut outcome = network
        .node_mut(&node_ids[leader_index])
        .expect("leader node exists")
        .automatic_mine_once(timestamp_ms);
    if outcome
        .skipped_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("collecting blinded reveals"))
    {
        outcome = network
            .node_mut(&node_ids[leader_index])
            .expect("leader node exists")
            .automatic_mine_once(timestamp_ms.saturating_add(TEST_REVEAL_BUNDLE_COLLECTION_MS + 1));
    }
    if let Some(reason) = outcome.skipped_reason {
        assert!(
            expected_clock_skew_skip_reason(&reason),
            "unexpected network-time mining skip reason: {reason}"
        );
    }
}

fn drain_all_outboxes(network: &mut InMemoryNetwork, node_ids: &[String]) {
    for id in node_ids {
        let _ = network.node_mut(id).expect("node exists").drain_outbox();
    }
}

fn mine_one_partition_block(
    network: &mut InMemoryNetwork,
    node_ids: &[String],
    wallets: &[Wallet],
    partition: &[usize],
    round: usize,
    rng: &mut TestRng,
) {
    assert!(
        try_mine_one_partition_block(network, node_ids, wallets, partition, round, rng),
        "partition could not produce a block"
    );
}

fn try_mine_one_partition_block(
    network: &mut InMemoryNetwork,
    node_ids: &[String],
    wallets: &[Wallet],
    partition: &[usize],
    round: usize,
    rng: &mut TestRng,
) -> bool {
    let anchor_id = &node_ids[partition[0]];
    let anchor_tip_timestamp = network
        .node(anchor_id)
        .expect("partition anchor exists")
        .ledger()
        .chain()
        .last()
        .expect("partition chain has a tip")
        .timestamp_ms;
    let mut candidates = partition.to_vec();
    candidates.sort_by_key(|index| {
        network
            .node(anchor_id)
            .expect("partition anchor exists")
            .ledger()
            .finalizer_rank_for_next_block(wallets[*index].address())
            .unwrap_or(u32::MAX)
    });

    for index in candidates {
        let rank_delay = network
            .node(anchor_id)
            .expect("partition anchor exists")
            .ledger()
            .finalizer_rank_for_next_block(wallets[index].address())
            .map(|rank| VDF_TARGET_BLOCK_MS.saturating_mul(u64::from(rank + 1) * 2))
            .unwrap_or(RECOVERY_BLOCK_DELAY_MS);
        let timestamp_ms = anchor_tip_timestamp
            .saturating_add(rank_delay)
            .saturating_add(1 + round as u64 + rng.next_u64() % 17);
        let mut outcome = network
            .node_mut(&node_ids[index])
            .expect("partition candidate exists")
            .automatic_mine_once(timestamp_ms);
        if outcome
            .skipped_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("collecting blinded reveals"))
        {
            outcome = network
                .node_mut(&node_ids[index])
                .expect("partition candidate exists")
                .automatic_mine_once(
                    timestamp_ms.saturating_add(TEST_REVEAL_BUNDLE_COLLECTION_MS + 1),
                );
        }
        if outcome.block.is_some() {
            let snapshot = network
                .node(&node_ids[index])
                .expect("partition producer exists")
                .chain_snapshot();
            for peer in partition {
                if *peer != index {
                    network
                        .node_mut(&node_ids[*peer])
                        .expect("partition peer exists")
                        .receive(GossipEnvelope::ChainSnapshot(snapshot.clone()))
                        .expect("partition peer imports produced block");
                }
            }
            return true;
        }
    }

    false
}

fn sync_until_idle(network: &mut InMemoryNetwork, from: &str, to: &str, limit: usize) {
    for _ in 0..256 {
        if !network
            .sync_node_from_peer(from, to, limit)
            .expect("range sync succeeds")
        {
            return;
        }
    }
    panic!("range sync did not become idle");
}

fn deliver_with_chaos(
    network: &mut InMemoryNetwork,
    node_ids: &[String],
    offline: &BTreeSet<String>,
    rng: &mut TestRng,
) -> bool {
    let mut outbound = Vec::new();
    for id in node_ids {
        if offline.contains(id) {
            continue;
        }
        let node = network.node_mut(id).expect("node exists");
        for envelope in node.drain_outbox() {
            outbound.push((id.clone(), envelope));
        }
    }

    if outbound.is_empty() {
        return false;
    }

    while !outbound.is_empty() {
        let index = rng.index(outbound.len());
        let (from, envelope) = outbound.swap_remove(index);
        for id in node_ids {
            if *id == from || offline.contains(id) {
                continue;
            }
            let duplicate = rng.index(5) == 0;
            receive_chaotic_envelope(network, id, envelope.clone());
            if duplicate {
                receive_chaotic_envelope(network, id, envelope.clone());
            }
        }
    }

    true
}

fn receive_chaotic_envelope(
    network: &mut InMemoryNetwork,
    id: &str,
    envelope: iuna::app::GossipEnvelope,
) {
    if let Err(error) = network.node_mut(id).expect("node exists").receive(envelope) {
        let message = error.to_string();
        assert!(
            message.contains("expected block height")
                || message.contains("conflicts with local chain")
                || message.contains("reveal bundle parent hash is invalid")
                || message.contains("mine transaction anchor is not on this chain")
                || message.contains("blinded transaction spends missing output")
                || message.contains("blinded transaction expiry is too far in the future"),
            "unexpected chaotic delivery error: {message}"
        );
    }
}

fn deliver_chaos_until_idle(
    network: &mut InMemoryNetwork,
    node_ids: &[String],
    offline: &BTreeSet<String>,
    rng: &mut TestRng,
) {
    for _ in 0..32 {
        if !deliver_with_chaos(network, node_ids, offline, rng) {
            return;
        }
    }
    panic!("chaotic network delivery did not become idle");
}

#[test]
fn in_memory_network_converges_after_generated_offline_and_reordered_delivery() {
    for seed in NETWORK_CHAOS_SEEDS {
        let (wallets, ledger) = single_finalizer_ledger(seed, 3);
        let node_ids = (0..wallets.len())
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let mut network = InMemoryNetwork::default();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            network.insert(
                &node_ids[index],
                NodeCore::from_ledger_with_burn_fee_and_enabled(
                    wallet.clone(),
                    joined,
                    true,
                    MICRO_IUNA,
                    0,
                ),
            );
        }

        let mut rng = TestRng::new(seed);
        deliver_chaos_until_idle(&mut network, &node_ids, &BTreeSet::new(), &mut rng);

        for round in 0..NETWORK_CHAOS_ROUNDS {
            let mut offline = BTreeSet::new();
            if round % 3 == 0 {
                offline.insert("n2".to_string());
            } else if round % 4 == 0 {
                offline.insert("n1".to_string());
            }

            let actor_index = 1 + rng.index(wallets.len() - 1);
            let actor_id = format!("n{actor_index}");
            let recipient = wallets[rng.index(wallets.len())].address().to_string();
            match rng.index(3) {
                0 => {
                    let _ = queue_plaintext_transfer(
                        network.node_mut(&actor_id).expect("actor node exists"),
                        &wallets[actor_index],
                        recipient,
                        MICRO_IUNA + rng.amount(17),
                        0,
                    );
                }
                1 => {
                    let _ = queue_plaintext_mine(
                        network.node_mut(&actor_id).expect("actor node exists"),
                        &wallets[actor_index],
                    );
                }
                _ => {
                    let _ = queue_plaintext_burn(
                        network.node_mut("n0").expect("finalizer node exists"),
                        &wallets[0],
                        MICRO_IUNA,
                        0,
                    );
                }
            }

            let timestamp_ms = (round + 1) as u64;
            let mut outcome = network
                .node_mut("n0")
                .expect("finalizer node exists")
                .automatic_mine_once(timestamp_ms);
            if outcome
                .skipped_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("collecting blinded reveals"))
            {
                outcome = network
                    .node_mut("n0")
                    .expect("finalizer node exists")
                    .automatic_mine_once(
                        timestamp_ms.saturating_add(TEST_REVEAL_BUNDLE_COLLECTION_MS + 1),
                    );
            }
            if let Some(reason) = outcome.skipped_reason {
                assert!(
                    reason.contains("at least one burn")
                        || reason.contains("required burn")
                        || reason.contains("could not")
                        || reason.contains("automatic burn failed"),
                    "unexpected chaotic mining skip reason: {reason}"
                );
            }

            deliver_chaos_until_idle(&mut network, &node_ids, &offline, &mut rng);
        }

        deliver_chaos_until_idle(&mut network, &node_ids, &BTreeSet::new(), &mut rng);
        for id in node_ids.iter().skip(1) {
            while network
                .sync_node_from_peer("n0", id, 128)
                .expect("lagging node range sync succeeds")
            {}
        }
        deliver_chaos_until_idle(&mut network, &node_ids, &BTreeSet::new(), &mut rng);
        assert_network_converged(&network, wallets.len());
    }
}

#[test]
fn generated_snapshot_tampering_is_rejected() {
    for seed in TAMPER_PROPERTY_SEEDS {
        let (wallets, mut ledger) = property_ledger(seed, 3);
        for round in 0..6 {
            try_finalize_next_block(round, &wallets, &mut ledger);
        }
        assert_chain_properties(ledger.snapshot());

        let mut mutated_hash = ledger.snapshot();
        if let Some(block) = mutated_hash.blocks.last_mut() {
            block.timestamp_ms = block.timestamp_ms.saturating_add(1);
        }
        assert!(Ledger::from_snapshot(mutated_hash).is_err());

        let mut mutated_transaction = ledger.snapshot();
        if let Some(transaction) = mutated_transaction
            .blocks
            .iter_mut()
            .flat_map(|block| block.transactions.iter_mut())
            .next()
        {
            match transaction {
                Transaction::Transfer { signature, .. }
                | Transaction::Burn { signature, .. }
                | Transaction::Mine { signature, .. } => signature.push_str("00"),
            }
        }
        assert!(Ledger::from_snapshot(mutated_transaction).is_err());
    }
}
