use std::collections::{BTreeMap, BTreeSet};

use iuna::{
    app::{InMemoryNetwork, NodeCore},
    domain::{
        Amount, ChainSnapshot, GenesisBurn, Ledger, MICRO_IUNA, MINE_FINALIZER_FEE, OutPoint,
        Transaction, TxInput, TxOutput, VDF_TARGET_BLOCK_MS, Wallet, hex_hash, verify_vdf,
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
const VDF_STABILITY_SEEDS: std::ops::Range<u64> = 500..516;
const VDF_STABILITY_BLOCKS: usize = 128;
const VDF_STABILITY_INITIAL_ROUNDS: u64 = 1_000_000;

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
    assert_eq!(confirmed_supply, expected_confirmed_supply(&snapshot));
    assert_reference_model_matches(&snapshot, &replayed);
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
                Transaction::Mine {
                    required_burn_amount,
                    ..
                } => {
                    supply = supply
                        .checked_add(*required_burn_amount)
                        .expect("mine output does not overflow supply");
                }
                Transaction::BurnClaim { .. } => {}
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
    if let Transaction::Mine {
        recipient,
        required_burn_amount,
        ..
    } = transaction
    {
        let output = TxOutput {
            address: recipient.clone(),
            amount: *required_burn_amount,
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
        Transaction::Transfer { .. } | Transaction::Mine { .. } | Transaction::BurnClaim { .. } => {
            0
        }
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
        Transaction::Mine { .. } | Transaction::BurnClaim { .. } => &[],
    }
}

fn reference_outputs(transaction: &Transaction) -> Vec<TxOutput> {
    match transaction {
        Transaction::Transfer { outputs, .. } => outputs.clone(),
        Transaction::Burn { change, .. } => change.clone(),
        Transaction::Mine {
            recipient,
            required_burn_amount,
            ..
        } => vec![TxOutput {
            address: recipient.clone(),
            amount: *required_burn_amount,
        }],
        Transaction::BurnClaim { .. } => Vec::new(),
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

fn finalize_many(ledger: &mut Ledger, wallet: &Wallet, count: usize, start_timestamp_ms: u64) {
    for offset in 0..count {
        finalize_with_wallet(ledger, wallet, start_timestamp_ms + offset as u64);
    }
}

#[test]
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
                    let _ = network
                        .node_mut(&node_id)
                        .expect("node exists")
                        .transfer_with_fee(recipient, rng.amount(2 * MICRO_IUNA), 0);
                }
                1 => {
                    let _ = network
                        .node_mut(&node_id)
                        .expect("node exists")
                        .burn_with_fee(MICRO_IUNA, 0);
                }
                2 => {
                    let _ = network
                        .node_mut(&node_id)
                        .expect("node exists")
                        .mine_pow_reward();
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
                    let outcome = network
                        .node_mut(&format!("n{leader_index}"))
                        .expect("leader node exists")
                        .automatic_mine_once((round + 1) as u64);
                    if let Some(reason) = outcome.skipped_reason {
                        assert!(
                            reason.contains("at least one burn")
                                || reason.contains("selected finalizer")
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
                || message.contains("mine transaction anchor is not on this chain"),
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
                    let _ = network
                        .node_mut(&actor_id)
                        .expect("actor node exists")
                        .transfer_with_fee(recipient, MICRO_IUNA + rng.amount(17), 0);
                }
                1 => {
                    let _ = network
                        .node_mut(&actor_id)
                        .expect("actor node exists")
                        .mine_pow_reward();
                }
                _ => {
                    let _ = network
                        .node_mut("n0")
                        .expect("finalizer node exists")
                        .burn_with_fee(MICRO_IUNA, 0);
                }
            }

            let outcome = network
                .node_mut("n0")
                .expect("finalizer node exists")
                .automatic_mine_once((round + 1) as u64);
            if let Some(reason) = outcome.skipped_reason {
                assert!(
                    reason.contains("at least one burn") || reason.contains("could not"),
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
                | Transaction::Mine { signature, .. }
                | Transaction::BurnClaim { signature, .. } => signature.push_str("00"),
            }
        }
        assert!(Ledger::from_snapshot(mutated_transaction).is_err());
    }
}
