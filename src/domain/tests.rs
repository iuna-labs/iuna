use super::*;

#[test]
fn target_block_time_is_five_minutes() {
    assert_eq!(VDF_TARGET_BLOCK_MS, 5 * 60 * 1_000);
}

fn test_utxo_outpoint(index: usize) -> OutPoint {
    OutPoint {
        txid: format!("{index:064x}"),
        index: 0,
    }
}

fn named_test_outpoint(name: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("test-utxo:{name}")),
        index: 0,
    }
}

fn ledger_with_wallet_utxos(wallet: &Wallet, amounts: &[Amount]) -> Ledger {
    let mut ledger = Ledger::new(BTreeMap::new(), 1);
    ledger.utxos = amounts
        .iter()
        .enumerate()
        .map(|(index, amount)| {
            (
                test_utxo_outpoint(index),
                TxOutput {
                    address: wallet.address().to_string(),
                    amount: *amount,
                },
            )
        })
        .collect();
    ledger
}

fn pending_balances(ledger: &Ledger) -> BTreeMap<String, Amount> {
    balances_from_utxos(&ledger.utxos_after_valid_pending().unwrap())
}

fn ledger_with_allocation(wallet: &Wallet, amount: Amount) -> Ledger {
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), amount);
    Ledger::new(genesis, 1)
}

fn mine_burn_block_with_mines(ledger: &mut Ledger, wallet: &Wallet, mine_actions: usize) {
    let burn = ledger.build_burn(wallet, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    for _ in 0..mine_actions {
        let mine = ledger.build_mine(wallet.address()).unwrap();
        ledger.submit_transaction(mine).unwrap();
    }
    let block = ledger.mine_next_block(wallet, ledger.height() + 1).unwrap();
    ledger.apply_block(block).unwrap();
}

fn test_mine_with_salt(ledger: &Ledger, recipient: &str, salt: u64) -> Transaction {
    let anchor = ledger.tip().hash.clone();
    let difficulty_bits = ledger.current_mine_difficulty_bits();
    for nonce in 0..u64::MAX {
        let signature = mine_signature(recipient, &anchor, salt, nonce, difficulty_bits);
        if hash_meets_difficulty(&signature, difficulty_bits) {
            return Transaction::Mine {
                recipient: recipient.to_string(),
                anchor,
                salt,
                nonce,
                difficulty_bits,
                proof_header: None,
                signature,
            };
        }
    }
    panic!("test should find a valid mine action");
}

fn apply_preverified_burn_block_at(
    ledger: &mut Ledger,
    wallet: &Wallet,
    timestamp_ms: u64,
) -> Block {
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let work = ledger
        .prepare_next_block(wallet.address(), timestamp_ms)
        .unwrap();
    let block = work.finish(wallet, "preverified-vdf".to_string());
    ledger
        .apply_preverified_block_at(block.clone(), u64::MAX)
        .unwrap();
    block
}

fn apply_preverified_burn_block_with_mines(
    ledger: &mut Ledger,
    wallet: &Wallet,
    mine_actions: usize,
) {
    let burn = ledger.build_burn(wallet, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let timestamp_ms = ledger
        .tip()
        .timestamp_ms
        .saturating_add(VDF_TARGET_BLOCK_MS);
    let mut block = ledger
        .prepare_next_block(wallet.address(), timestamp_ms)
        .unwrap()
        .finish(wallet, "preverified-vdf".to_string());
    for salt in 0..mine_actions {
        block.transactions.push(test_mine_with_salt(
            ledger,
            wallet.address(),
            salt as u64 + 1,
        ));
    }
    block.reward = fee_reward(&block.transactions).unwrap();
    block.hash = block.compute_hash();
    ledger.apply_preverified_block_at(block, u64::MAX).unwrap();
}

fn vdf_retarget_sample_block(
    timestamp_ms: u64,
    finalizer_mode: FinalizerMode,
    finalizer_rank: u32,
) -> Block {
    Block {
        height: 1,
        prev_hash: String::new(),
        timestamp_ms,
        miner: String::new(),
        finalizer_mode,
        finalizer_rank,
        reward: 0,
        vdf_rounds: 1,
        vdf_output: String::new(),
        leader_proof: None,
        blinded_transactions: Vec::new(),
        reveal_bundle_section: RevealBundleSection::default(),
        transactions: Vec::new(),
        hash: String::new(),
    }
}

const TEST_BURN_AMOUNT: Amount = MICRO_IUNA / 10;

fn unsigned_mine(ledger: &Ledger, recipient: &str) -> Transaction {
    let anchor = ledger.tip().hash.clone();
    let difficulty_bits = ledger.current_mine_difficulty_bits();
    for nonce in 0..u64::MAX {
        let salt = 1;
        let signature = mine_signature(recipient, &anchor, salt, nonce, difficulty_bits);
        if hash_meets_difficulty(&signature, difficulty_bits) {
            return Transaction::Mine {
                recipient: recipient.to_string(),
                anchor,
                salt,
                nonce,
                difficulty_bits,
                proof_header: None,
                signature,
            };
        }
    }
    panic!("expected to find mine proof");
}

fn wallet_for_address<'a>(wallets: &'a [Wallet], address: &str) -> &'a Wallet {
    wallets
        .iter()
        .find(|wallet| wallet.address() == address)
        .unwrap_or_else(|| panic!("missing wallet for address {address}"))
}

fn ledger_with_finalizers(
    finalizers: &[Wallet],
    extra_allocations: &[(&Wallet, Amount)],
) -> Ledger {
    let mut allocations = BTreeMap::new();
    for wallet in finalizers {
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
    }
    for (wallet, amount) in extra_allocations {
        allocations.insert(wallet.address().to_string(), *amount);
    }
    Ledger::new_with_genesis_burns(
        allocations,
        finalizers
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect(),
        1,
    )
    .unwrap()
}

fn mine_preverified_as_next_leader(
    ledger: &mut Ledger,
    wallets: &[Wallet],
    timestamp_ms: u64,
) -> Block {
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(wallets, &leader);
    let prepared = ledger
        .prepare_next_block(wallet.address(), timestamp_ms)
        .unwrap();
    let block = prepared.finish(wallet, "preverified-vdf".to_string());
    ledger
        .apply_preverified_block_at(block.clone(), u64::MAX)
        .unwrap();
    block
}

fn mine_preverified_as_next_leader_with_reveal_bundles(
    ledger: &mut Ledger,
    wallets: &[Wallet],
    timestamp_ms: u64,
) -> Block {
    let block =
        prepare_preverified_as_next_leader_with_reveal_bundles(ledger, wallets, timestamp_ms);
    ledger
        .apply_preverified_block_at(block.clone(), u64::MAX)
        .unwrap();
    block
}

fn prepare_preverified_as_next_leader_with_reveal_bundles(
    ledger: &Ledger,
    wallets: &[Wallet],
    timestamp_ms: u64,
) -> Block {
    let bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallet_for_address(wallets, &member.owner);
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(wallets, &leader);
    let prepared = ledger
        .prepare_next_block_with_reveal_bundles(wallet.address(), timestamp_ms, bundles)
        .unwrap();
    prepared.finish(wallet, "preverified-vdf".to_string())
}

fn queue_next_leader_burn(ledger: &mut Ledger, wallets: &[Wallet]) {
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(wallets, &leader);
    let burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
}

fn transfer_with_extra_zero_outputs(
    ledger: &Ledger,
    wallet: &Wallet,
    to: &str,
    amount: Amount,
    fee: Amount,
    extra_outputs: usize,
) -> Transaction {
    let required = amount.checked_add(fee).unwrap();
    let (inputs, input_total) = ledger.select_inputs(wallet.address(), required).unwrap();
    let mut outputs = vec![TxOutput {
        address: to.to_string(),
        amount,
    }];
    outputs.extend((0..extra_outputs).map(|_| TxOutput {
        address: to.to_string(),
        amount: 0,
    }));
    let change = input_total - required;
    if change > 0 {
        outputs.push(TxOutput {
            address: wallet.address().to_string(),
            amount: change,
        });
    }
    UnsignedUtxoTransaction::Transfer {
        inputs,
        outputs,
        fee,
    }
    .sign(wallet)
}

#[test]
fn wallet_utxos_only_include_outputs_owned_by_address() {
    let alice = Wallet::from_seed("wallet-utxos-alice");
    let bob = Wallet::from_seed("wallet-utxos-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3]);
    ledger.utxos.insert(
        named_test_outpoint("bob"),
        TxOutput {
            address: bob.address().to_string(),
            amount: 5,
        },
    );

    let alice_utxos = ledger.utxos_for_address(alice.address());
    let total = alice_utxos
        .iter()
        .map(|(_, output)| output.amount)
        .sum::<Amount>();

    assert_eq!(alice_utxos.len(), 2);
    assert_eq!(total, ledger.balance_of(alice.address()));
    assert!(
        alice_utxos
            .iter()
            .all(|(_, output)| output.address == alice.address())
    );
}

#[test]
fn transfer_combines_multiple_small_utxos_to_cover_amount_and_fee() {
    let alice = Wallet::from_seed("combine-small-utxos-alice");
    let bob = Wallet::from_seed("combine-small-utxos-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 1]);

    let tx = ledger.build_transfer(&alice, bob.address(), 2, 1).unwrap();

    let Transaction::Transfer {
        inputs,
        outputs,
        fee,
        ..
    } = &tx
    else {
        panic!("expected transfer");
    };
    assert_eq!(inputs.len(), 3);
    assert_eq!(*fee, 1);
    assert_eq!(
        outputs,
        &[TxOutput {
            address: bob.address().to_string(),
            amount: 2
        }]
    );

    ledger.submit_transaction(tx).unwrap();
    let balances = pending_balances(&ledger);
    assert_eq!(
        balances.get(alice.address()).copied().unwrap_or_default(),
        0
    );
    assert_eq!(balances.get(bob.address()).copied().unwrap_or_default(), 2);
}

#[test]
fn transfer_returns_change_when_combined_utxos_exceed_payment() {
    let alice = Wallet::from_seed("combine-change-alice");
    let bob = Wallet::from_seed("combine-change-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);

    let tx = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();

    let Transaction::Transfer {
        inputs, outputs, ..
    } = &tx
    else {
        panic!("expected transfer");
    };
    assert_eq!(inputs.len(), 3);
    assert_eq!(
        outputs,
        &[
            TxOutput {
                address: bob.address().to_string(),
                amount: 3
            },
            TxOutput {
                address: alice.address().to_string(),
                amount: 1
            }
        ]
    );

    ledger.submit_transaction(tx).unwrap();
    let balances = pending_balances(&ledger);
    assert_eq!(balances.get(alice.address()).copied(), Some(1));
    assert_eq!(balances.get(bob.address()).copied(), Some(3));
}

#[test]
fn transaction_economic_size_uses_compact_canonical_fields() {
    let alice = Wallet::from_seed("economic-size-alice");
    let bob = Wallet::from_seed("economic-size-bob");
    let ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);
    let selected = vec![
        test_utxo_outpoint(0),
        test_utxo_outpoint(1),
        test_utxo_outpoint(2),
    ];

    let tx = ledger
        .build_transfer_with_inputs(&alice, bob.address(), 3, 0, &selected)
        .unwrap();

    assert!(tx.economic_size_bytes() < tx.serialized_size_bytes().unwrap());
    assert_eq!(
        tx.economic_size_bytes(),
        1 + 1 + (3 * (32 + 1 + 32)) + 1 + (2 * (32 + 1)) + 1 + 64
    );
}

#[test]
fn blinded_fee_rate_size_uses_visible_envelope_bytes() {
    let alice = Wallet::from_seed("blinded-fee-size-alice");
    let ledger = ledger_with_wallet_utxos(&alice, &[10]);
    let built = ledger
        .build_blinded_burn(&alice, 1, 2, ledger.height() + 4)
        .unwrap();

    assert_eq!(
        built.transaction.fee_rate_size_bytes(),
        built.transaction.serialized_size_bytes().unwrap()
    );
    assert!(built.transaction.fee_rate_size_bytes() > built.transaction.encrypted_size as usize);
}

#[test]
fn blinded_fee_split_burns_rounding_dust() {
    let committer = Wallet::from_seed("blinded-split-committer");
    let executor = Wallet::from_seed("blinded-split-executor");
    let commitment = "01".repeat(32);
    let active = ActiveBlindedTransaction {
        transaction: BlindedTransaction {
            commitment: commitment.clone(),
            inputs: Vec::new(),
            fee: 1,
            encrypted_size: 1,
            expires_at_height: 2,
            nonce: "02".repeat(BLINDED_NONCE_BYTES),
            ciphertext: "03".to_string(),
            payload_hash: "04".repeat(32),
        },
        locked_outputs: Vec::new(),
        included_height: 1,
        included_by: committer.address().to_string(),
    };
    let transaction = Transaction::Transfer {
        inputs: Vec::new(),
        outputs: Vec::new(),
        fee: 1,
        signature: String::new(),
    };
    let mut utxos = BTreeMap::new();

    credit_blinded_fee_outputs(
        &mut utxos,
        &active,
        executor.address(),
        &transaction,
        &[],
        3,
        false,
    )
    .unwrap();

    assert!(!utxos.contains_key(&blinded_committer_fee_outpoint(&commitment)));
    assert!(!utxos.contains_key(&blinded_executor_fee_outpoint(&commitment)));
}

#[test]
fn blinded_fee_split_pays_no_reveal_finalizer_without_signed_reveal_lists() {
    let committer = Wallet::from_seed("blinded-no-list-committer");
    let executor = Wallet::from_seed("blinded-no-list-executor");
    let commitment = "06".repeat(32);
    let active = ActiveBlindedTransaction {
        transaction: BlindedTransaction {
            commitment: commitment.clone(),
            inputs: Vec::new(),
            fee: 100,
            encrypted_size: 1,
            expires_at_height: 2,
            nonce: "02".repeat(BLINDED_NONCE_BYTES),
            ciphertext: "03".to_string(),
            payload_hash: "04".repeat(32),
        },
        locked_outputs: Vec::new(),
        included_height: 1,
        included_by: committer.address().to_string(),
    };
    let transaction = Transaction::Transfer {
        inputs: Vec::new(),
        outputs: Vec::new(),
        fee: 100,
        signature: String::new(),
    };
    let mut utxos = BTreeMap::new();

    credit_blinded_fee_outputs(
        &mut utxos,
        &active,
        executor.address(),
        &transaction,
        &[],
        3,
        false,
    )
    .unwrap();

    assert_eq!(
        utxos.get(&blinded_committer_fee_outpoint(&commitment)),
        Some(&TxOutput {
            address: committer.address().to_string(),
            amount: 35,
        })
    );
    assert!(!utxos.contains_key(&blinded_executor_fee_outpoint(&commitment)));
}

#[test]
fn blinded_fee_split_pays_committer_executor_and_reveal_bundle_signers() {
    let committer = Wallet::from_seed("blinded-scale-committer");
    let executor = Wallet::from_seed("blinded-scale-executor");
    let signer_a = Wallet::from_seed("blinded-scale-signer-a");
    let signer_b = Wallet::from_seed("blinded-scale-signer-b");
    let commitment = "05".repeat(32);
    let active = ActiveBlindedTransaction {
        transaction: BlindedTransaction {
            commitment: commitment.clone(),
            inputs: Vec::new(),
            fee: 7,
            encrypted_size: 1,
            expires_at_height: 2,
            nonce: "02".repeat(BLINDED_NONCE_BYTES),
            ciphertext: "03".to_string(),
            payload_hash: "04".repeat(32),
        },
        locked_outputs: Vec::new(),
        included_height: 1,
        included_by: committer.address().to_string(),
    };
    let transaction = Transaction::Transfer {
        inputs: Vec::new(),
        outputs: Vec::new(),
        fee: 100,
        signature: String::new(),
    };
    let mut utxos = BTreeMap::new();
    let signatures = vec![
        RevealBundleSignature {
            slot: 0,
            member: signer_a.address().to_string(),
            signature: "11".repeat(SIGNATURE_BYTES),
        },
        RevealBundleSignature {
            slot: 2,
            member: signer_b.address().to_string(),
            signature: "22".repeat(SIGNATURE_BYTES),
        },
    ];

    credit_blinded_fee_outputs(
        &mut utxos,
        &active,
        executor.address(),
        &transaction,
        &signatures,
        3,
        false,
    )
    .unwrap();

    assert_eq!(
        utxos.get(&blinded_committer_fee_outpoint(&commitment)),
        Some(&TxOutput {
            address: committer.address().to_string(),
            amount: 35,
        })
    );
    assert_eq!(
        utxos.get(&blinded_executor_fee_outpoint(&commitment)),
        Some(&TxOutput {
            address: executor.address().to_string(),
            amount: 23,
        })
    );
    assert_eq!(
        utxos.get(&blinded_reveal_bundle_signer_fee_outpoint(&commitment, 0)),
        Some(&TxOutput {
            address: signer_a.address().to_string(),
            amount: 10,
        })
    );
    assert_eq!(
        utxos.get(&blinded_reveal_bundle_signer_fee_outpoint(&commitment, 2)),
        Some(&TxOutput {
            address: signer_b.address().to_string(),
            amount: 10,
        })
    );
}

#[test]
fn blinded_reveal_finalizer_fee_scales_by_available_reveal_bundle_slots() {
    let fee = 300_000;
    let full_share = blinded_fee_share(fee, BLINDED_REVEAL_FINALIZER_FEE_BPS);

    assert_eq!(blinded_reveal_finalizer_fee(fee, 0, 3), 0);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 1, 3), full_share / 3);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 1, 2), full_share / 2);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 1, 1), full_share);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 2, 3), full_share * 2 / 3);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 3, 3), full_share);
    assert_eq!(blinded_reveal_finalizer_fee(fee, 4, 3), full_share);
}

#[test]
fn transfer_rejects_invalid_recipient_address() {
    let alice = Wallet::from_seed("invalid-transfer-recipient-alice");
    let ledger = ledger_with_wallet_utxos(&alice, &[10]);

    let error = ledger.build_transfer(&alice, "aa", 1, 0).unwrap_err();

    assert!(format!("{error:#}").contains("invalid transfer recipient address"));
}

#[test]
fn mine_rejects_invalid_recipient_address_before_pow() {
    let ledger = Ledger::new(BTreeMap::new(), 1);

    let error = ledger.build_mine("aa").unwrap_err();

    assert!(format!("{error:#}").contains("invalid mine recipient address"));
}

#[test]
fn mine_search_respects_nonce_attempt_limit() {
    let alice = Wallet::from_seed("bounded-mine-search-alice");
    let ledger = Ledger::new(BTreeMap::new(), 1);

    let outcome = ledger.search_mine(alice.address(), 1, 0, 0).unwrap();

    assert!(outcome.transaction.is_none());
    assert_eq!(outcome.next_nonce, 0);
    assert_eq!(outcome.attempts, 0);
}

#[test]
fn mempool_rejects_invalid_input_outpoint_id() {
    let alice = Wallet::from_seed("invalid-outpoint-alice");
    let bob = Wallet::from_seed("invalid-outpoint-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[10]);
    let unsigned = UnsignedUtxoTransaction::Transfer {
        inputs: vec![UnsignedTxInput {
            outpoint: OutPoint {
                txid: "aa".to_string(),
                index: 0,
            },
            owner: alice.address().to_string(),
        }],
        outputs: vec![TxOutput {
            address: bob.address().to_string(),
            amount: 1,
        }],
        fee: 0,
    };
    let transaction = unsigned.sign(&alice);

    let error = ledger.submit_transaction(transaction).unwrap_err();

    assert!(format!("{error:#}").contains("invalid input outpoint txid"));
    assert!(ledger.pending().is_empty());
}

#[test]
fn missing_input_transaction_goes_to_orphan_pool_not_pending_mempool() {
    let alice = Wallet::from_seed("missing-input-orphan-alice");
    let bob = Wallet::from_seed("missing-input-orphan-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[10]);
    let transaction = UnsignedUtxoTransaction::Transfer {
        inputs: vec![UnsignedTxInput {
            outpoint: OutPoint {
                txid: hex_hash("missing-input-orphan"),
                index: 0,
            },
            owner: alice.address().to_string(),
        }],
        outputs: vec![TxOutput {
            address: bob.address().to_string(),
            amount: 1,
        }],
        fee: 0,
    }
    .sign(&alice);

    let outcome = ledger.submit_transaction_with_outcome(transaction).unwrap();

    assert_eq!(outcome, TransactionSubmitOutcome::Added);
    assert!(ledger.pending().is_empty());
    assert_eq!(ledger.orphan_transactions().len(), 1);
}

#[test]
fn vdf_retarget_observed_block_time_is_clamped() {
    assert_eq!(
        clamped_vdf_retarget_observed_block_ms(1),
        MIN_VDF_RETARGET_OBSERVED_BLOCK_MS
    );
    assert_eq!(
        clamped_vdf_retarget_observed_block_ms(VDF_TARGET_BLOCK_MS),
        VDF_TARGET_BLOCK_MS
    );
    assert_eq!(
        clamped_vdf_retarget_observed_block_ms(u64::MAX),
        MAX_VDF_RETARGET_OBSERVED_BLOCK_MS
    );
}

#[test]
fn vdf_retarget_observed_block_time_ignores_ticket_fallback_ranks() {
    let parent = vdf_retarget_sample_block(0, FinalizerMode::Ticket, 0);
    let fallback_child =
        vdf_retarget_sample_block(VDF_TARGET_BLOCK_MS * 2, FinalizerMode::Ticket, 1);

    assert_eq!(
        vdf_retarget_observed_block_ms(&parent, &fallback_child),
        None
    );
}

#[test]
fn vdf_retarget_observed_block_time_ignores_recovery_blocks() {
    let parent = vdf_retarget_sample_block(0, FinalizerMode::Ticket, 0);
    let recovery_child =
        vdf_retarget_sample_block(RECOVERY_BLOCK_DELAY_MS, FinalizerMode::Recovery, 0);

    assert_eq!(
        vdf_retarget_observed_block_ms(&parent, &recovery_child),
        None
    );
}

#[test]
fn vdf_retarget_keeps_rounds_inside_deadband() {
    let current = 1_000;
    let low_deadband_edge =
        VDF_TARGET_BLOCK_MS - VDF_TARGET_BLOCK_MS * VDF_RETARGET_DEADBAND_PERCENT as u64 / 100;
    let high_deadband_edge =
        VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS * VDF_RETARGET_DEADBAND_PERCENT as u64 / 100;

    assert_eq!(retarget_vdf_rounds(current, low_deadband_edge), current);
    assert_eq!(retarget_vdf_rounds(current, VDF_TARGET_BLOCK_MS), current);
    assert_eq!(retarget_vdf_rounds(current, high_deadband_edge), current);
}

#[test]
fn vdf_retarget_limits_each_step_to_two_percent() {
    let current = 1_000;

    assert_eq!(
        retarget_vdf_rounds(current, MIN_VDF_RETARGET_OBSERVED_BLOCK_MS),
        1_020
    );
    assert_eq!(
        retarget_vdf_rounds(current, MAX_VDF_RETARGET_OBSERVED_BLOCK_MS),
        980
    );
}

#[test]
fn vdf_rounds_retarget_below_legacy_u32_limit_after_slow_blocks() {
    let wallet = Wallet::from_seed("vdf-rounds-slow-above-u32");
    let initial_rounds = u64::from(u32::MAX);
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 1_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), 1)],
        initial_rounds,
    )
    .unwrap();

    let block1 = apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
    assert_eq!(block1.vdf_rounds, initial_rounds);
    assert_eq!(ledger.vdf_rounds(), initial_rounds);

    let block2 = apply_preverified_burn_block_at(
        &mut ledger,
        &wallet,
        VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS * 2,
    );
    assert_eq!(block2.vdf_rounds, initial_rounds);

    assert!(
        ledger.vdf_rounds() < initial_rounds,
        "slow blocks should retarget below the legacy u32 VDF rounds ceiling"
    );
}

#[test]
fn fallback_block_is_excluded_from_vdf_retarget_observations() {
    let alice = Wallet::from_seed("fallback-retarget-alice");
    let bob = Wallet::from_seed("fallback-retarget-bob");
    let wallets = [&alice, &bob];
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 1_000);
    genesis.insert(bob.address().to_string(), 1_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![
            GenesisBurn::new(alice.address(), 1),
            GenesisBurn::new(bob.address(), 1),
        ],
        100,
    )
    .unwrap();

    let primary = ledger.expected_leader_for_next_block().unwrap();
    let primary_wallet = wallets
        .into_iter()
        .find(|wallet| wallet.address() == primary)
        .unwrap();
    apply_preverified_burn_block_at(&mut ledger, primary_wallet, VDF_TARGET_BLOCK_MS);
    assert_eq!(ledger.vdf_rounds(), 100);

    let primary = ledger.expected_leader_for_next_block().unwrap();
    let fallback = wallets
        .into_iter()
        .find(|wallet| wallet.address() != primary)
        .unwrap();
    let timestamp_ms = ledger.tip().timestamp_ms + 1;
    let block = apply_preverified_burn_block_at(&mut ledger, fallback, timestamp_ms);

    assert_eq!(block.finalizer_rank, 1);
    assert_eq!(block.vdf_rounds, 200);
    assert_eq!(ledger.vdf_rounds(), 100);
}

#[test]
fn recovery_block_is_excluded_from_vdf_retarget_observations() {
    let alice = Wallet::from_seed("recovery-retarget-alice");
    let bob = Wallet::from_seed("recovery-retarget-bob");
    let mut genesis = BTreeMap::new();
    genesis.insert(alice.address().to_string(), 1_000);
    genesis.insert(bob.address().to_string(), 1_000);
    let mut ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(alice.address(), 1)], 100)
            .unwrap();

    apply_preverified_burn_block_at(&mut ledger, &alice, VDF_TARGET_BLOCK_MS);
    assert_eq!(ledger.vdf_rounds(), 100);

    let burn = ledger.build_burn(&bob, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger
        .mine_recovery_block(&bob, VDF_TARGET_BLOCK_MS + RECOVERY_BLOCK_DELAY_MS)
        .unwrap();
    assert_eq!(block.finalizer_mode, FinalizerMode::Recovery);
    ledger.apply_block(block).unwrap();

    assert_eq!(ledger.vdf_rounds(), 100);
}

#[test]
fn generated_vdf_retarget_decreases_after_slow_blocks_above_legacy_limit() {
    let legacy_limit = u64::from(u32::MAX);
    let slow_observed_ms = [
        VDF_TARGET_BLOCK_MS * 6 / 5,
        VDF_TARGET_BLOCK_MS * 2,
        VDF_TARGET_BLOCK_MS * 3,
        MAX_VDF_RETARGET_OBSERVED_BLOCK_MS,
    ];

    for seed in 0..16_u64 {
        let wallet = Wallet::from_seed(&format!("generated-vdf-retarget-{seed}"));
        let initial_rounds = legacy_limit + 1 + seed * 1_000_003;
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            initial_rounds,
        )
        .unwrap();
        let observed_ms = slow_observed_ms[seed as usize % slow_observed_ms.len()];

        apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
        let second = apply_preverified_burn_block_at(
            &mut ledger,
            &wallet,
            VDF_TARGET_BLOCK_MS + observed_ms,
        );

        assert_eq!(second.vdf_rounds, initial_rounds);
        assert!(
            ledger.vdf_rounds() < initial_rounds,
            "seed {seed} with observed {observed_ms}ms should lower VDF rounds from {initial_rounds}, got {}",
            ledger.vdf_rounds()
        );

        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let next_work = ledger
            .prepare_next_block(wallet.address(), second.timestamp_ms + VDF_TARGET_BLOCK_MS)
            .unwrap();
        assert_eq!(next_work.vdf_rounds(), ledger.vdf_rounds());
    }
}

#[test]
fn block_timestamp_future_check_uses_supplied_network_time() {
    let wallet = Wallet::from_seed("adjusted-time-domain");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 1_000);
    let mut ledger = Ledger::new(allocations, 1);

    let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
    assert!(ledger.submit_transaction(burn).unwrap());
    let block = ledger.mine_next_block(&wallet, 10 * 60 * 1_000).unwrap();

    let error = ledger.apply_block_at(block, 1_000).unwrap_err();

    assert!(format!("{error:#}").contains("too far in the future"));
}

#[test]
fn ticket_block_timestamp_uses_finalizer_rank_time_slot() {
    let alice = Wallet::from_seed("rank-slot-alice");
    let bob = Wallet::from_seed("rank-slot-bob");
    let wallets = [alice.clone(), bob.clone()];
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![
            GenesisBurn::new(alice.address(), 1),
            GenesisBurn::new(bob.address(), 1),
        ],
        100,
    )
    .unwrap();

    let primary = wallet_for_address(&wallets, &ledger.expected_leader_for_next_block().unwrap());
    let burn = ledger.build_burn(primary, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let work = ledger.prepare_next_block(primary.address(), 1).unwrap();
    assert_eq!(work.timestamp_ms(), 1);
    let block = work.finish(primary, "preverified-vdf".to_string());
    ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

    let fallback = wallets
        .iter()
        .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
        .expect("expected rank 1 fallback");
    let burn = ledger.build_burn(fallback, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let parent_timestamp = ledger.tip().timestamp_ms;
    let work = ledger
        .prepare_next_block(fallback.address(), parent_timestamp + 1)
        .unwrap();

    assert_eq!(
        work.timestamp_ms(),
        parent_timestamp + VDF_TARGET_BLOCK_MS * 2
    );
    assert_eq!(work.vdf_rounds(), ledger.vdf_rounds() * 2);
}

#[test]
fn required_anchor_burn_is_selected_before_higher_fee_burns_when_block_is_full() {
    let wallet = Wallet::from_seed("required-anchor-priority-wallet");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
        1,
    )
    .unwrap();
    let high_fee_outpoint = named_test_outpoint("required-anchor-priority-high-fee");
    let anchor_outpoint = named_test_outpoint("required-anchor-priority-anchor");
    ledger.utxos.insert(
        high_fee_outpoint.clone(),
        TxOutput {
            address: wallet.address().to_string(),
            amount: 3,
        },
    );
    ledger.utxos.insert(
        anchor_outpoint.clone(),
        TxOutput {
            address: wallet.address().to_string(),
            amount: 2,
        },
    );
    let high_fee_burn = ledger
        .build_burn_with_inputs(&wallet, 1, 1, &[high_fee_outpoint])
        .unwrap();
    let anchor_burn = ledger
        .build_burn_with_inputs(&wallet, 1, 0, &[anchor_outpoint])
        .unwrap();
    ledger.submit_transaction(high_fee_burn).unwrap();
    ledger.submit_transaction(anchor_burn.clone()).unwrap();
    ledger.launch_profile.max_block_transactions = 1;

    let work = ledger
        .prepare_next_block_with_required_burn_and_reveal_bundles(
            wallet.address(),
            1,
            Vec::new(),
            Some(anchor_burn.signature()),
        )
        .unwrap();
    let block = work.finish(&wallet, "preverified-vdf".to_string());

    assert_eq!(block.transactions.len(), 1);
    assert_eq!(block.transactions[0].signature(), anchor_burn.signature());
}

#[test]
fn late_ticket_vdf_completion_is_visible_to_retarget() {
    let wallet = Wallet::from_seed("late-ticket-vdf-wallet");
    let mut allocations = BTreeMap::new();
    allocations.insert(wallet.address().to_string(), 1_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(wallet.address(), 1)],
        100,
    )
    .unwrap();

    apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
    assert_eq!(ledger.vdf_rounds(), 100);

    let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let work = ledger
        .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
        .unwrap();
    let scheduled_timestamp = work.timestamp_ms();
    let late_timestamp = ledger.tip().timestamp_ms + VDF_TARGET_BLOCK_MS * 3;
    assert!(late_timestamp > scheduled_timestamp);

    let block = work.finish_at(&wallet, "preverified-vdf".to_string(), late_timestamp);
    assert_eq!(block.timestamp_ms, late_timestamp);
    ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

    assert!(
        ledger.vdf_rounds() < 100,
        "late VDF completion should lower future VDF rounds"
    );
}

#[test]
fn block_before_finalizer_rank_time_slot_is_rejected() {
    let alice = Wallet::from_seed("rank-slot-reject-alice");
    let bob = Wallet::from_seed("rank-slot-reject-bob");
    let wallets = [alice.clone(), bob.clone()];
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1_000);
    allocations.insert(bob.address().to_string(), 1_000);
    let mut ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![
            GenesisBurn::new(alice.address(), 1),
            GenesisBurn::new(bob.address(), 1),
        ],
        100,
    )
    .unwrap();

    let primary = wallet_for_address(&wallets, &ledger.expected_leader_for_next_block().unwrap());
    let burn = ledger.build_burn(primary, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let work = ledger.prepare_next_block(primary.address(), 1).unwrap();
    let block = work.finish(primary, "preverified-vdf".to_string());
    ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

    let fallback = wallets
        .iter()
        .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
        .expect("expected rank 1 fallback");
    let burn = ledger.build_burn(fallback, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let parent_timestamp = ledger.tip().timestamp_ms;
    let work = ledger
        .prepare_next_block(fallback.address(), parent_timestamp + 1)
        .unwrap();
    let mut block = work.finish(fallback, "preverified-vdf".to_string());
    block.timestamp_ms = parent_timestamp + VDF_TARGET_BLOCK_MS * 2 - 1;
    block.hash = block.compute_hash();

    let error = ledger
        .apply_preverified_block_at(block, u64::MAX)
        .unwrap_err();

    assert!(format!("{error:#}").contains("before finalizer rank 1 time slot"));
}

#[test]
fn miner_skips_oversized_pending_transaction_and_keeps_fitting_fee_transaction() {
    let alice = Wallet::from_seed("oversized-select-alice");
    let bob = Wallet::from_seed("oversized-select-bob");
    let carol = Wallet::from_seed("oversized-select-carol");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 1);
    allocations.insert(bob.address().to_string(), 300_000);
    allocations.insert(carol.address().to_string(), 300_000);
    let mut ledger =
        Ledger::new_with_genesis_burns(allocations, vec![GenesisBurn::new(alice.address(), 1)], 10)
            .unwrap();
    let burn = ledger.build_burn(&alice, 1, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let oversized =
        transfer_with_extra_zero_outputs(&ledger, &bob, alice.address(), 1, 100_000, 4_000);
    let fitting = ledger
        .build_transfer(&carol, alice.address(), 1, 5)
        .unwrap();
    assert!(oversized.serialized_size_bytes().unwrap() > MAX_BLOCK_BYTES);
    ledger.submit_transaction(oversized.clone()).unwrap();
    ledger.submit_transaction(fitting.clone()).unwrap();

    let block = ledger.mine_next_block(&alice, 1).unwrap();
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
fn transfer_can_spend_selected_utxos_when_they_cover_amount_and_fee() {
    let alice = Wallet::from_seed("selected-utxos-alice");
    let bob = Wallet::from_seed("selected-utxos-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
    let selected = vec![test_utxo_outpoint(2)];

    let tx = ledger
        .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
        .unwrap();

    let Transaction::Transfer {
        inputs, outputs, ..
    } = &tx
    else {
        panic!("expected transfer");
    };
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].outpoint, selected[0]);
    assert_eq!(
        outputs,
        &[
            TxOutput {
                address: bob.address().to_string(),
                amount: 2
            },
            TxOutput {
                address: alice.address().to_string(),
                amount: 2
            }
        ]
    );

    ledger.submit_transaction(tx).unwrap();
    let balances = pending_balances(&ledger);
    assert_eq!(balances.get(bob.address()).copied(), Some(2));
    assert_eq!(balances.get(alice.address()).copied(), Some(7));
}

#[test]
fn burn_can_spend_selected_utxos_when_they_cover_amount_and_fee() {
    let alice = Wallet::from_seed("selected-burn-utxos-alice");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
    let selected = vec![test_utxo_outpoint(1)];

    let tx = ledger
        .build_burn_with_inputs(&alice, 1, 1, &selected)
        .unwrap();

    let Transaction::Burn {
        inputs,
        change,
        amount,
        fee,
        ..
    } = &tx
    else {
        panic!("expected burn");
    };
    assert_eq!(*amount, 1);
    assert_eq!(*fee, 1);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].outpoint, selected[0]);
    assert_eq!(
        change,
        &[TxOutput {
            address: alice.address().to_string(),
            amount: 1
        }]
    );

    ledger.submit_transaction(tx).unwrap();
    let balances = pending_balances(&ledger);
    assert_eq!(balances.get(alice.address()).copied(), Some(8));
}

#[test]
fn transfer_rejects_selected_utxos_that_do_not_cover_amount_plus_fee() {
    let alice = Wallet::from_seed("selected-utxos-insufficient-alice");
    let bob = Wallet::from_seed("selected-utxos-insufficient-bob");
    let ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
    let selected = vec![test_utxo_outpoint(0)];

    let error = ledger
        .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
        .unwrap_err();

    assert!(format!("{error:#}").contains("selected UTXOs do not cover"));
}

#[test]
fn transfer_rejects_selected_utxos_owned_by_someone_else() {
    let alice = Wallet::from_seed("selected-utxos-owner-alice");
    let bob = Wallet::from_seed("selected-utxos-owner-bob");
    let carol = Wallet::from_seed("selected-utxos-owner-carol");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[5]);
    ledger.utxos.insert(
        named_test_outpoint("carol"),
        TxOutput {
            address: carol.address().to_string(),
            amount: 5,
        },
    );
    let selected = vec![named_test_outpoint("carol")];

    let error = ledger
        .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
        .unwrap_err();

    assert!(format!("{error:#}").contains("is not owned"));
}

#[test]
fn transfer_rejects_when_combined_utxos_do_not_cover_amount_plus_fee() {
    let alice = Wallet::from_seed("combine-insufficient-alice");
    let bob = Wallet::from_seed("combine-insufficient-bob");
    let ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 1]);

    let error = ledger
        .build_transfer(&alice, bob.address(), 3, 1)
        .unwrap_err();

    assert!(format!("{error:#}").contains("insufficient funds"));
}

#[test]
fn pending_change_from_combined_utxos_can_fund_next_transaction() {
    let alice = Wallet::from_seed("combine-pending-change-alice");
    let bob = Wallet::from_seed("combine-pending-change-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);

    let first = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
    let first_signature = first.signature().to_string();
    ledger.submit_transaction(first).unwrap();

    let second = ledger.build_transfer(&alice, bob.address(), 1, 0).unwrap();
    let Transaction::Transfer { inputs, .. } = &second else {
        panic!("expected transfer");
    };
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].outpoint.txid, first_signature);
    assert_eq!(inputs[0].outpoint.index, 1);

    ledger.submit_transaction(second).unwrap();
    let balances = pending_balances(&ledger);
    assert_eq!(
        balances.get(alice.address()).copied().unwrap_or_default(),
        0
    );
    assert_eq!(balances.get(bob.address()).copied(), Some(4));
}

#[test]
fn winning_burn_ticket_is_consumed_even_when_window_remains() {
    let mut tickets = vec![
        BurnTicket {
            id: "high-burn".to_string(),
            owner: "alice".to_string(),
            amount: 10_000,
            eligible_from_height: 4,
            eligible_until_height: 6,
        },
        BurnTicket {
            id: "small-burn".to_string(),
            owner: "bob".to_string(),
            amount: 1,
            eligible_from_height: 5,
            eligible_until_height: 7,
        },
    ];
    let mut block = Block {
        height: 4,
        prev_hash: "0".repeat(64),
        timestamp_ms: 1,
        miner: "alice".to_string(),
        finalizer_mode: FinalizerMode::Ticket,
        finalizer_rank: 0,
        reward: BLOCK_REWARD,
        vdf_rounds: 1,
        vdf_output: "vdf".to_string(),
        leader_proof: Some(LeaderProof {
            ticket_id: "high-burn".to_string(),
            public_key: "alice".to_string(),
            signature: "signature".to_string(),
        }),
        blinded_transactions: Vec::new(),
        reveal_bundle_section: RevealBundleSection::default(),
        transactions: Vec::new(),
        hash: String::new(),
    };
    block.hash = block.compute_hash();

    consume_leader_ticket(&block, &mut tickets).unwrap();

    assert!(
        tickets.iter().all(|ticket| ticket.id != "high-burn"),
        "a winning burn must not remain eligible for the rest of its window"
    );
    assert!(
        tickets.iter().any(|ticket| ticket.id == "small-burn"),
        "unselected future tickets should remain pending"
    );
}

#[test]
fn burn_leader_ranks_for_block_reconstructs_historical_ticket_order() {
    let alice = Wallet::from_seed("burn-rank-alice");
    let bob = Wallet::from_seed("burn-rank-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![
            GenesisBurn::new(alice.address(), MICRO_IUNA),
            GenesisBurn::new(bob.address(), MICRO_IUNA),
        ],
        1,
    )
    .unwrap();

    let ranks = ledger.burn_leader_ranks_for_block(1).unwrap();
    let leader = ledger.expected_leader_for_next_block().unwrap();

    assert_eq!(ranks.len(), 2);
    assert_eq!(ranks[0].rank, 0);
    assert_eq!(ranks[0].owner, leader);
    assert!(ranks.iter().all(|rank| rank.amount == MICRO_IUNA));
    assert_eq!(ledger.burn_leader_ranks_for_block(0).unwrap(), Vec::new());

    let batch = ledger.burn_leader_ranks_for_blocks([0, 1]).unwrap();
    assert!(batch.get(&0).unwrap().is_empty());
    assert_eq!(batch.get(&1), Some(&ranks));
}

#[test]
fn mine_recipient_is_bound_to_proof_hash() {
    let alice = Wallet::from_seed("mine-proof-alice");
    let bob = Wallet::from_seed("mine-proof-bob");
    let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);
    let mut forged = unsigned_mine(&ledger, alice.address());
    if let Transaction::Mine { recipient, .. } = &mut forged {
        *recipient = bob.address().to_string();
    }

    let error = ledger.submit_transaction(forged).unwrap_err();

    assert!(format!("{error:#}").contains("proof hash is invalid"));
}

#[test]
fn mine_action_uses_fixed_reward_and_fixed_finalizer_fee() {
    let alice = Wallet::from_seed("mine-fixed-reward-alice");
    let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);

    let mine = ledger.build_mine(alice.address()).unwrap();

    assert_eq!(mine.amount(), MINE_REWARD);
    assert_eq!(mine.fee(), MINE_FINALIZER_FEE);
    assert!(ledger.submit_transaction(mine).unwrap());
}

#[test]
fn burn_fee_goes_to_block_finalizer() {
    let alice = Wallet::from_seed("burn-fee-finalizer-alice");
    let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);

    let burn_fee = 12;
    let burn = ledger
        .build_burn(&alice, TEST_BURN_AMOUNT, burn_fee)
        .unwrap();
    ledger.submit_transaction(burn).unwrap();
    let prepared = ledger.prepare_next_block(alice.address(), 1).unwrap();

    assert_eq!(prepared.reward, burn_fee);
}

#[test]
fn blinded_burn_commits_ciphertext_and_reveal_executes_later() {
    let alice = Wallet::from_seed("blinded-burn-finalizer-alice");
    let bob = Wallet::from_seed("blinded-burn-finalizer-bob");
    let carol = Wallet::from_seed("blinded-burn-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let fee = 100;
    let burn_amount = 3;
    let before_carol = ledger.balance_of(carol.address());

    let blinded = ledger
        .build_blinded_burn(&carol, burn_amount, fee, ledger.height() + 4)
        .unwrap();
    assert!(!blinded.transaction.ciphertext.contains("burn"));
    assert!(!blinded.transaction.ciphertext.contains(carol.address()));
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);

    let commit_block = mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    let inclusion_finalizer = commit_block.miner.clone();
    assert_eq!(
        commit_block
            .transactions
            .iter()
            .filter(|transaction| transaction.is_burn())
            .count(),
        1
    );
    assert_eq!(commit_block.blinded_transactions, vec![blinded.transaction]);
    assert_eq!(commit_block.reward, 0);
    let before_inclusion_finalizer = ledger.balance_of(&inclusion_finalizer);

    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    let reveal_block =
        mine_preverified_as_next_leader_with_reveal_bundles(&mut ledger, &finalizers, 2);
    let reveal_executor = reveal_block.miner.clone();

    assert_eq!(reveal_block.all_blinded_reveals().len(), 1);
    assert_eq!(
        ledger.balance_of(carol.address()),
        before_carol - burn_amount - fee
    );
    assert!(ledger.tickets.iter().any(|ticket| {
        ticket.owner == carol.address()
            && ticket.amount == burn_amount
            && ticket.eligible_from_height
                == reveal_block.height + ledger.launch_profile.ticket_maturity_delay_heights
    }));
    let reveal_plaintext_burn_spent_by_inclusion_finalizer = reveal_block
        .transactions
        .iter()
        .filter(|transaction| {
            transaction.is_burn() && transaction.sender() == inclusion_finalizer.as_str()
        })
        .fold(0_u64, |total, transaction| {
            total + transaction.amount() + transaction.fee()
        });
    let committer_fee = blinded_fee_share(fee, BLINDED_COMMITTER_FEE_BPS);
    let reveal_finalizer_fee = blinded_reveal_finalizer_fee(
        fee,
        reveal_block.included_reveal_bundle_count(),
        ledger
            .burn_leader_ranks_for_block(reveal_block.height)
            .unwrap()
            .len(),
    );
    let reveal_bundle_signer_fee = blinded_fee_share(fee, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS);
    let commitment = &commit_block.blinded_transactions[0].commitment;
    assert_eq!(
        ledger
            .utxos
            .get(&blinded_committer_fee_outpoint(commitment))
            .unwrap(),
        &TxOutput {
            address: inclusion_finalizer.clone(),
            amount: committer_fee,
        }
    );
    assert!(
        !ledger
            .utxos
            .contains_key(&blinded_executor_fee_outpoint(commitment))
    );
    assert_eq!(reveal_block.reward, reveal_finalizer_fee);
    assert_eq!(
        ledger
            .utxos
            .get(&reward_outpoint(&reveal_block.hash))
            .unwrap(),
        &TxOutput {
            address: reveal_executor.clone(),
            amount: reveal_finalizer_fee,
        }
    );
    for signature in &reveal_block.reveal_bundle_section.signatures {
        assert_eq!(
            ledger
                .utxos
                .get(&blinded_reveal_bundle_signer_fee_outpoint(
                    commitment,
                    signature.slot
                ))
                .unwrap(),
            &TxOutput {
                address: signature.member.clone(),
                amount: reveal_bundle_signer_fee,
            }
        );
    }
    let mut inclusion_finalizer_fee = committer_fee;
    if inclusion_finalizer == reveal_executor {
        inclusion_finalizer_fee += reveal_finalizer_fee;
    }
    inclusion_finalizer_fee += reveal_block
        .reveal_bundle_section
        .signatures
        .iter()
        .filter(|signature| signature.member == inclusion_finalizer)
        .count() as u64
        * reveal_bundle_signer_fee;
    assert_eq!(
        ledger.balance_of(&inclusion_finalizer),
        before_inclusion_finalizer + inclusion_finalizer_fee
            - reveal_plaintext_burn_spent_by_inclusion_finalizer
    );
}

#[test]
fn blinded_reveal_finalizer_fees_are_aggregated_into_block_reward() {
    let alice = Wallet::from_seed("aggregated-finalizer-fee-alice");
    let bob = Wallet::from_seed("aggregated-finalizer-fee-bob");
    let carol = Wallet::from_seed("aggregated-finalizer-fee-carol");
    let dave = Wallet::from_seed("aggregated-finalizer-fee-dave");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(
        &finalizers,
        &[(&carol, 10 * MICRO_IUNA), (&dave, 10 * MICRO_IUNA)],
    );
    let first_fee = 100;
    let second_fee = 200;
    let first_blinded = ledger
        .build_blinded_burn(&carol, 3, first_fee, ledger.height() + 4)
        .unwrap();
    let second_blinded = ledger
        .build_blinded_burn(&dave, 4, second_fee, ledger.height() + 4)
        .unwrap();
    let first_commitment = first_blinded.transaction.commitment.clone();
    let second_commitment = second_blinded.transaction.commitment.clone();
    ledger
        .submit_blinded_transaction(first_blinded.transaction)
        .unwrap();
    ledger
        .submit_blinded_transaction(second_blinded.transaction)
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    let commit_timestamp_ms = ledger
        .tip()
        .timestamp_ms
        .saturating_add(VDF_TARGET_BLOCK_MS);
    let commit_block =
        mine_preverified_as_next_leader(&mut ledger, &finalizers, commit_timestamp_ms);
    assert_eq!(commit_block.height, 1);

    ledger.submit_blinded_reveal(first_blinded.reveal).unwrap();
    ledger.submit_blinded_reveal(second_blinded.reveal).unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    let reveal_timestamp_ms = ledger
        .tip()
        .timestamp_ms
        .saturating_add(VDF_TARGET_BLOCK_MS);
    let reveal_block = prepare_preverified_as_next_leader_with_reveal_bundles(
        &ledger,
        &finalizers,
        reveal_timestamp_ms,
    );
    let first_reveal_finalizer_fee = blinded_reveal_finalizer_fee(
        first_fee,
        reveal_block.included_reveal_bundle_count(),
        ledger
            .burn_leader_ranks_for_block(reveal_block.height)
            .unwrap()
            .len(),
    );
    let second_reveal_finalizer_fee = blinded_reveal_finalizer_fee(
        second_fee,
        reveal_block.included_reveal_bundle_count(),
        ledger
            .burn_leader_ranks_for_block(reveal_block.height)
            .unwrap()
            .len(),
    );
    let aggregate_reveal_finalizer_fee = first_reveal_finalizer_fee
        .checked_add(second_reveal_finalizer_fee)
        .unwrap();

    assert_eq!(reveal_block.height, 2);
    assert_eq!(reveal_block.reward, aggregate_reveal_finalizer_fee);
    let mut plain_fee_reward_block = reveal_block.clone();
    plain_fee_reward_block.reward = fee_reward(&plain_fee_reward_block.transactions).unwrap();
    plain_fee_reward_block.hash = plain_fee_reward_block.compute_hash();
    let error = ledger
        .clone()
        .apply_preverified_block_at(plain_fee_reward_block, u64::MAX)
        .unwrap_err();
    assert!(format!("{error:#}").contains("block reward is invalid"));

    ledger
        .apply_preverified_block_at(reveal_block.clone(), u64::MAX)
        .unwrap();
    assert!(
        !ledger
            .utxos
            .contains_key(&blinded_executor_fee_outpoint(&first_commitment))
    );
    assert!(
        !ledger
            .utxos
            .contains_key(&blinded_executor_fee_outpoint(&second_commitment))
    );
    assert_eq!(
        ledger.utxos.get(&reward_outpoint(&reveal_block.hash)),
        Some(&TxOutput {
            address: reveal_block.miner.clone(),
            amount: aggregate_reveal_finalizer_fee,
        })
    );
}

#[test]
fn blinded_utxo_commit_exposes_and_locks_inputs_until_reveal_or_expiry() {
    let alice = Wallet::from_seed("blinded-lock-alice");
    let bob = Wallet::from_seed("blinded-lock-bob");
    let mut ledger = ledger_with_wallet_utxos(&alice, &[10]);
    let transfer = ledger.build_transfer(&alice, bob.address(), 3, 2).unwrap();
    let visible_inputs = transfer.inputs().to_vec();

    let blinded = ledger
        .build_blinded_transaction(&alice, transfer, ledger.height() + 4)
        .unwrap();

    assert_eq!(blinded.transaction.inputs.len(), visible_inputs.len());
    assert_eq!(
        unsigned_inputs(&blinded.transaction.inputs),
        unsigned_inputs(&visible_inputs)
    );
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();
    let error = ledger
        .build_transfer(&alice, bob.address(), 1, 0)
        .unwrap_err();
    assert!(format!("{error:#}").contains("insufficient funds"));
}

#[test]
fn blinded_payload_omits_visible_inputs_and_reconstructs_transaction_on_reveal() {
    let alice = Wallet::from_seed("blinded-compact-payload-alice");
    let bob = Wallet::from_seed("blinded-compact-payload-bob");
    let ledger = ledger_with_wallet_utxos(&alice, &[10]);
    let transfer = ledger.build_transfer(&alice, bob.address(), 3, 2).unwrap();
    let full_transaction_bytes = serde_json::to_vec(&transfer).unwrap().len();
    let blinded = ledger
        .build_blinded_transaction(&alice, transfer.clone(), ledger.height() + 4)
        .unwrap();
    let key = decode_hex_array::<BLINDED_KEY_BYTES>(&blinded.reveal.key).unwrap();
    let nonce = decode_hex_array::<BLINDED_NONCE_BYTES>(&blinded.transaction.nonce).unwrap();
    let ciphertext = decode_hex(&blinded.transaction.ciphertext).unwrap();

    let plaintext = decrypt_blinded_payload(
        &key,
        &nonce,
        &signed_blinded_inputs(&unsigned_inputs(&blinded.transaction.inputs), ""),
        blinded.transaction.fee,
        blinded.transaction.expires_at_height,
        &ciphertext,
    )
    .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
    let revealed = decrypt_blinded_transaction(&blinded.transaction, &blinded.reveal).unwrap();

    assert_eq!(
        payload.get("kind").and_then(|kind| kind.as_str()),
        Some("transfer")
    );
    assert!(payload.get("inputs").is_none());
    assert!(plaintext.len() < full_transaction_bytes);
    assert_eq!(revealed, transfer);
}

#[test]
fn unrevealed_blinded_utxo_commit_burns_fee_and_returns_change() {
    let alice = Wallet::from_seed("blinded-expiry-alice");
    let bob = Wallet::from_seed("blinded-expiry-bob");
    let carol = Wallet::from_seed("blinded-expiry-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let carol_balance = 10 * MICRO_IUNA;
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, carol_balance)]);
    let fee = 100;
    let blinded = ledger
        .build_blinded_burn(&carol, 3, fee, ledger.height() + 2)
        .unwrap();
    let commitment = blinded.transaction.commitment.clone();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

    assert_eq!(ledger.balance_of(carol.address()), 0);
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 2);

    assert_eq!(
        ledger
            .utxos
            .get(&blinded_committer_fee_outpoint(&commitment)),
        None
    );
    assert_eq!(
        ledger
            .utxos
            .get(&blinded_expiry_change_outpoint(&commitment)),
        Some(&TxOutput {
            address: carol.address().to_string(),
            amount: carol_balance - fee,
        })
    );
    assert_eq!(ledger.balance_of(carol.address()), carol_balance - fee);
}

#[test]
fn fee_bearing_blinded_commit_without_inputs_is_rejected() {
    let alice = Wallet::from_seed("blinded-no-input-fee-alice");
    let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);
    let mut blinded = ledger
        .build_blinded_burn(&alice, 1, 1, ledger.height() + 4)
        .unwrap()
        .transaction;
    blinded.inputs.clear();
    blinded.commitment = blinded_transaction_commitment(&blinded).unwrap();

    let error = ledger.submit_blinded_transaction(blinded).unwrap_err();

    assert!(format!("{error:#}").contains("must lock visible inputs"));
}

#[test]
fn mine_actions_cannot_be_blinded() {
    let alice = Wallet::from_seed("blinded-mine-collateral-alice");
    let ledger = ledger_with_finalizers(&[alice.clone()], &[]);
    let mine = ledger.build_mine(alice.address()).unwrap();
    let error = ledger
        .build_blinded_transaction(&alice, mine, ledger.height() + 4)
        .unwrap_err();

    assert!(format!("{error:#}").contains("mine actions are public"));
}

#[test]
fn reveal_bundle_hashes_are_bound_to_next_block_vdf_seed() {
    let alice = Wallet::from_seed("bundle-seed-alice");
    let bob = Wallet::from_seed("bundle-seed-bob");
    let carol = Wallet::from_seed("bundle-seed-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    let leader = ledger.expected_leader_for_next_block().unwrap();
    let leader_wallet = wallet_for_address(&finalizers, &leader);
    let bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallet_for_address(&finalizers, &member.owner);
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    if bundles.len() > 1 {
        let mut reversed = bundles.clone();
        reversed.reverse();
        let error = ledger
            .validate_next_block_reveal_bundles(reversed)
            .unwrap_err();
        assert!(format!("{error:#}").contains("reveal bundles are not in slot order"));
    }

    let without_bundles = ledger
        .prepare_next_block(leader_wallet.address(), ledger.tip().timestamp_ms + 1)
        .unwrap();
    let with_bundles = ledger
        .prepare_next_block_with_reveal_bundles(
            leader_wallet.address(),
            ledger.tip().timestamp_ms + 1,
            bundles,
        )
        .unwrap();

    assert_ne!(without_bundles.vdf_seed(), with_bundles.vdf_seed());
}

#[test]
fn reveal_committee_includes_next_block_finalizer_as_slot_zero() {
    let alice = Wallet::from_seed("bundle-finalizer-slot-alice");
    let bob = Wallet::from_seed("bundle-finalizer-slot-bob");
    let carol = Wallet::from_seed("bundle-finalizer-slot-carol");
    let dave = Wallet::from_seed("bundle-finalizer-slot-dave");
    let erin = Wallet::from_seed("bundle-finalizer-slot-erin");
    let finalizers = [alice.clone(), bob.clone(), carol.clone(), dave.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&erin, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&erin, 3, 7, ledger.height() + 4)
        .unwrap();
    let commitment = blinded.transaction.commitment.clone();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let committee = ledger.reveal_committee_for_next_block();
    let leader_wallet = wallet_for_address(&finalizers, &leader);
    let bundle = ledger.build_reveal_bundle(leader_wallet).unwrap().unwrap();

    assert_eq!(committee.first().map(|member| member.slot), Some(0));
    assert_eq!(committee.first().map(|member| member.rank), Some(0));
    assert_eq!(
        committee.first().map(|member| member.owner.as_str()),
        Some(leader.as_str())
    );
    assert_eq!(bundle.slot, 0);
    assert_eq!(bundle.member, leader);
    assert!(
        bundle
            .reveals
            .iter()
            .any(|reveal| reveal.commitment == commitment)
    );
}

#[test]
fn reveal_bundle_section_deduplicates_reveals_with_slot_mask() {
    let alice = Wallet::from_seed("bundle-compact-alice");
    let bob = Wallet::from_seed("bundle-compact-bob");
    let carol = Wallet::from_seed("bundle-compact-carol");
    let dave = Wallet::from_seed("bundle-compact-dave");
    let finalizers = [alice.clone(), bob.clone(), carol.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&dave, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&dave, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger
        .submit_blinded_reveal(blinded.reveal.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);

    let mut bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallet_for_address(&finalizers, &member.owner);
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(bundles.len() >= 2);
    bundles.truncate(2);
    let expected_hashes = reveal_bundle_hashes(&bundles);
    let expected_mask = bundles
        .iter()
        .fold(0_u8, |mask, bundle| mask | (1_u8 << bundle.slot));

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let leader_wallet = wallet_for_address(&finalizers, &leader);
    let prepared = ledger
        .prepare_next_block_with_reveal_bundles(
            leader_wallet.address(),
            ledger.tip().timestamp_ms + 1,
            bundles.clone(),
        )
        .unwrap();
    let block = prepared.finish(leader_wallet, "preverified-vdf".to_string());

    assert_eq!(block.reveal_bundle_section.signatures.len(), 2);
    assert_eq!(block.reveal_bundle_section.reveals.len(), 1);
    assert_eq!(
        block.reveal_bundle_section.reveals[0].bundle_mask,
        expected_mask
    );
    assert_eq!(block.all_blinded_reveals(), vec![&blinded.reveal]);
    assert_eq!(block.reveal_bundle_hashes(), expected_hashes);
    assert_eq!(
        block
            .reveal_bundle_section
            .expand(block.height, &block.prev_hash),
        bundles
    );
}

#[test]
fn reveal_bundle_validation_rejects_wrong_signature_and_slot() {
    let alice = Wallet::from_seed("bundle-invalid-alice");
    let bob = Wallet::from_seed("bundle-invalid-bob");
    let carol = Wallet::from_seed("bundle-invalid-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    let member = ledger.reveal_committee_for_next_block()[0].clone();
    let wallet = wallet_for_address(&finalizers, &member.owner);
    let bundle = ledger.build_reveal_bundle(wallet).unwrap().unwrap();

    let mut wrong_signature = bundle.clone();
    wrong_signature.signature = "00".repeat(SIGNATURE_BYTES);
    let error = ledger
        .validate_next_block_reveal_bundles(vec![wrong_signature])
        .unwrap_err();
    assert!(format!("{error:#}").contains("reveal bundle signature is invalid"));

    let mut wrong_slot = bundle;
    wrong_slot.slot = REVEAL_COMMITTEE_SIZE as u8 - 1;
    let error = ledger
        .validate_next_block_reveal_bundles(vec![wrong_slot])
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("reveal bundle slot is not assigned")
            || format!("{error:#}").contains("reveal bundle member is not assigned to slot")
    );
}

#[test]
fn blinded_reveal_with_wrong_key_is_rejected_in_block() {
    let alice = Wallet::from_seed("blinded-wrong-key-finalizer-alice");
    let bob = Wallet::from_seed("blinded-wrong-key-finalizer-bob");
    let carol = Wallet::from_seed("blinded-wrong-key-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(&finalizers, &leader);
    let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(filler_burn).unwrap();
    let mut prepared = ledger
        .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
        .unwrap();
    let committee_member = ledger.reveal_committee_for_next_block()[0].clone();
    let committee_wallet = wallet_for_address(&finalizers, &committee_member.owner);
    let wrong_reveal = BlindedReveal {
        commitment: blinded.transaction.commitment,
        key: "00".repeat(BLINDED_KEY_BYTES),
    };
    let wrong_bundle = committee_wallet.reveal_bundle(RevealBundlePayload {
        height: prepared.height,
        prev_hash: prepared.prev_hash.clone(),
        slot: committee_member.slot,
        member: committee_wallet.address().to_string(),
        reveals: vec![wrong_reveal],
    });
    prepared.reveal_bundle_section = ledger.reveal_bundle_section_from_bundles(vec![wrong_bundle]);
    prepared.reward = blinded_reveal_finalizer_fee(
        blinded.transaction.fee,
        prepared.reveal_bundle_section.signatures.len(),
        ledger.reveal_committee_for_next_block().len(),
    );
    let block = prepared.finish(wallet, "preverified-vdf".to_string());

    let error = ledger
        .apply_preverified_block_at(block, u64::MAX)
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("decrypt blinded transaction"),
        "{error:#}"
    );
}

#[test]
fn expired_blinded_reveal_is_not_selected() {
    let alice = Wallet::from_seed("blinded-expire-finalizer-alice");
    let bob = Wallet::from_seed("blinded-expire-finalizer-bob");
    let carol = Wallet::from_seed("blinded-expire-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 2)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(&finalizers, &leader);
    let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(filler_burn).unwrap();
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 2);

    ledger.submit_blinded_reveal(blinded.reveal).unwrap();
    assert!(ledger.valid_pending_blinded_reveals().is_empty());
}

#[test]
fn recovery_block_includes_pending_blinded_transactions_when_space_allows() {
    let alice = Wallet::from_seed("recovery-blinded-commit-alice");
    let bob = Wallet::from_seed("recovery-blinded-commit-bob");
    let carol = Wallet::from_seed("recovery-blinded-commit-carol");
    let mut ledger = ledger_with_finalizers(
        &[alice],
        &[(&bob, 10 * MICRO_IUNA), (&carol, 10 * MICRO_IUNA)],
    );
    let blinded = ledger
        .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(recovery_burn).unwrap();

    let block = ledger
        .mine_recovery_block(&bob, RECOVERY_BLOCK_DELAY_MS)
        .unwrap();

    assert!(
        block
            .blinded_transactions
            .iter()
            .any(|transaction| transaction.commitment == blinded.transaction.commitment)
    );
}

#[test]
fn recovery_block_size_selection_uses_recovery_skeleton() {
    let alice = Wallet::from_seed("recovery-size-commit-alice");
    let bob = Wallet::from_seed("recovery-size-commit-bob");
    let carol = Wallet::from_seed("recovery-size-commit-carol");
    let mut ledger = ledger_with_finalizers(
        &[alice],
        &[(&bob, 10 * MICRO_IUNA), (&carol, 10 * MICRO_IUNA)],
    );
    let blinded = ledger
        .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(recovery_burn.clone()).unwrap();
    let recovery_selection = BlockSelection {
        transactions: vec![recovery_burn],
        blinded_transactions: vec![blinded.transaction.clone()],
    };
    let recovery_estimate =
        estimated_block_selection_size_bytes(&recovery_selection, true).unwrap();
    let ticket_estimate = estimated_block_selection_size_bytes(&recovery_selection, false).unwrap();
    assert!(recovery_estimate < ticket_estimate);

    ledger.launch_profile.max_block_bytes = recovery_estimate;
    let tight_block = ledger
        .mine_recovery_block(&bob, RECOVERY_BLOCK_DELAY_MS)
        .unwrap();

    assert!(
        tight_block
            .blinded_transactions
            .iter()
            .any(|transaction| transaction.commitment == blinded.transaction.commitment)
    );
}

#[test]
fn recovery_block_includes_pending_blinded_reveals_when_space_allows() {
    let alice = Wallet::from_seed("recovery-blinded-reveal-alice");
    let bob = Wallet::from_seed("recovery-blinded-reveal-bob");
    let carol = Wallet::from_seed("recovery-blinded-reveal-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger
        .submit_blinded_reveal(blinded.reveal.clone())
        .unwrap();
    let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(recovery_burn).unwrap();

    let bundles = ledger
        .reveal_committee_for_next_block()
        .into_iter()
        .filter_map(|member| {
            let wallet = wallet_for_address(&finalizers, &member.owner);
            ledger.build_reveal_bundle(wallet).unwrap()
        })
        .collect::<Vec<_>>();
    let prepared = ledger
        .prepare_recovery_block_with_reveal_bundles(
            bob.address(),
            ledger.recovery_block_min_timestamp(),
            bundles,
        )
        .unwrap();
    let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
    let block = prepared.finish(&bob, vdf_output);

    assert!(
        block
            .all_blinded_reveals()
            .iter()
            .any(|reveal| reveal.commitment == blinded.transaction.commitment)
    );
}

#[test]
fn blinded_transaction_expiring_at_next_height_is_not_selected() {
    let alice = Wallet::from_seed("blinded-next-expire-finalizer-alice");
    let bob = Wallet::from_seed("blinded-next-expire-finalizer-bob");
    let carol = Wallet::from_seed("blinded-next-expire-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 1)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(&finalizers, &leader);
    let error = ledger.prepare_next_block(wallet.address(), 1).unwrap_err();

    assert!(format!("{error:#}").contains("block must include at least one burn transaction"));
}

#[test]
fn blinded_transaction_expiry_cannot_exceed_protocol_window() {
    let alice = Wallet::from_seed("blinded-window-finalizer-alice");
    let bob = Wallet::from_seed("blinded-window-finalizer-bob");
    let carol = Wallet::from_seed("blinded-window-carol");
    let finalizers = [alice, bob];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let max_expiry = ledger
        .height()
        .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS);

    ledger.build_blinded_burn(&carol, 3, 7, max_expiry).unwrap();

    let error = ledger
        .build_blinded_burn(&carol, 3, 7, max_expiry + 1)
        .unwrap_err();
    assert!(format!("{error:#}").contains("expiry is too far in the future"));

    let mut forged = ledger
        .build_blinded_burn(&carol, 3, 7, max_expiry)
        .unwrap()
        .transaction;
    forged.expires_at_height = max_expiry + 1;
    forged.commitment = blinded_transaction_commitment(&forged).unwrap();
    let error = ledger.submit_blinded_transaction(forged).unwrap_err();
    assert!(format!("{error:#}").contains("expiry is too far in the future"));
}

#[test]
fn blinded_transaction_does_not_satisfy_plaintext_burn_requirement() {
    let alice = Wallet::from_seed("blinded-no-burn-finalizer-alice");
    let bob = Wallet::from_seed("blinded-no-burn-finalizer-bob");
    let carol = Wallet::from_seed("blinded-no-burn-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(&finalizers, &leader);
    let error = ledger.prepare_next_block(wallet.address(), 1).unwrap_err();

    assert!(format!("{error:#}").contains("block must include at least one burn transaction"));
}

#[test]
fn revealed_blinded_transaction_cannot_be_included_again() {
    let alice = Wallet::from_seed("blinded-duplicate-finalizer-alice");
    let bob = Wallet::from_seed("blinded-duplicate-finalizer-bob");
    let carol = Wallet::from_seed("blinded-duplicate-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 6)
        .unwrap();
    ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
    ledger
        .submit_blinded_reveal(blinded.reveal.clone())
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader_with_reveal_bundles(&mut ledger, &finalizers, 2);

    let leader = ledger.expected_leader_for_next_block().unwrap();
    let wallet = wallet_for_address(&finalizers, &leader);
    let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
    ledger.submit_transaction(filler_burn).unwrap();
    let mut prepared = ledger
        .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
        .unwrap();
    prepared
        .blinded_transactions
        .push(blinded.transaction.clone());
    let block = prepared.finish(wallet, "preverified-vdf".to_string());

    let error = ledger
        .apply_preverified_block_at(block, u64::MAX)
        .unwrap_err();

    assert!(format!("{error:#}").contains("blinded transaction is already on chain"));
}

#[test]
fn pending_blinded_reveals_expose_revealed_transaction_data() {
    let alice = Wallet::from_seed("pending-reveal-data-finalizer-alice");
    let bob = Wallet::from_seed("pending-reveal-data-finalizer-bob");
    let carol = Wallet::from_seed("pending-reveal-data-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let blinded = ledger
        .build_blinded_burn(&carol, 3, 7, ledger.height() + 6)
        .unwrap();
    let commitment = blinded.transaction.commitment.clone();
    ledger
        .submit_blinded_transaction(blinded.transaction)
        .unwrap();
    queue_next_leader_burn(&mut ledger, &finalizers);
    mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

    ledger.submit_blinded_reveal(blinded.reveal).unwrap();

    let revealed = ledger.pending_revealed_blinded_transactions();
    assert_eq!(revealed.len(), 1);
    assert_eq!(revealed[0].commitment, commitment);
    assert_eq!(revealed[0].height, ledger.height() + 1);
    assert_eq!(revealed[0].transaction.amount(), 3);
    assert_eq!(revealed[0].transaction.fee(), 7);
    assert!(revealed[0].transaction.is_burn());
}

#[test]
fn abandoned_fork_blinded_transactions_return_to_mempool() {
    let alice = Wallet::from_seed("blinded-reorg-finalizer-alice");
    let bob = Wallet::from_seed("blinded-reorg-finalizer-bob");
    let carol = Wallet::from_seed("blinded-reorg-carol");
    let finalizers = [alice.clone(), bob.clone()];
    let mut local = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
    let mut remote = local.clone();
    let blinded = local
        .build_blinded_burn(&carol, 3, 7, local.height() + 8)
        .unwrap();
    local
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();
    queue_next_leader_burn(&mut local, &finalizers);
    mine_preverified_as_next_leader(&mut local, &finalizers, 1);

    for timestamp_ms in [1, 2] {
        let leader = remote.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let burn = remote.build_burn(wallet, 1, 0).unwrap();
        remote.submit_transaction(burn).unwrap();
        mine_preverified_as_next_leader(&mut remote, &finalizers, timestamp_ms);
    }

    assert!(
        local
            .extend_from_preverified_snapshot_at(remote.snapshot(), u64::MAX)
            .unwrap()
    );
    assert!(local.has_blinded_transaction(&blinded.transaction.commitment));
    assert_eq!(
        local.pending_blinded_transactions(),
        std::slice::from_ref(&blinded.transaction)
    );
}

#[test]
fn block_selection_includes_mine_action_after_required_block_burn() {
    let alice = Wallet::from_seed("mine-fixed-reward-select-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let mine = ledger.build_mine(alice.address()).unwrap();
    ledger.submit_transaction(mine.clone()).unwrap();

    let block = ledger.mine_next_block(&alice, 1).unwrap();

    assert_eq!(
        block.transactions.iter().filter(|tx| tx.is_burn()).count(),
        1
    );
    assert!(
        block
            .transactions
            .iter()
            .any(|tx| tx.signature() == mine.signature())
    );
    assert_eq!(block.reward, MINE_FINALIZER_FEE);
}

#[test]
fn block_selection_can_skip_mine_action_when_space_is_limited() {
    let alice = Wallet::from_seed("mine-space-limit-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);
    ledger.launch_profile.max_block_transactions = 2;

    let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let first_mine = ledger.build_mine(alice.address()).unwrap();
    ledger.submit_transaction(first_mine).unwrap();
    let second_mine = ledger.build_mine(alice.address()).unwrap();
    ledger.submit_transaction(second_mine).unwrap();

    let block = ledger.mine_next_block(&alice, 1).unwrap();

    assert_eq!(block.transactions.len(), 2);
    assert!(block.transactions.iter().any(Transaction::is_burn));
    assert_eq!(block.reward, MINE_FINALIZER_FEE);
}

#[test]
fn pending_mine_outputs_are_not_spendable_until_confirmed() {
    let alice = Wallet::from_seed("pending-mine-spend-alice");
    let bob = Wallet::from_seed("pending-mine-spend-bob");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let mine = ledger.build_mine(alice.address()).unwrap();
    let mine_outpoint = OutPoint {
        txid: mine.signature().to_string(),
        index: 0,
    };
    ledger.submit_transaction(mine.clone()).unwrap();

    assert!(
        !ledger
            .available_utxos_for_address(alice.address())
            .unwrap()
            .iter()
            .any(|(outpoint, _)| outpoint == &mine_outpoint)
    );
    let pending_error = ledger
        .build_transfer_with_inputs(
            &alice,
            bob.address(),
            TEST_BURN_AMOUNT,
            0,
            std::slice::from_ref(&mine_outpoint),
        )
        .unwrap_err();
    assert!(format!("{pending_error:#}").contains("not spendable"));

    let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let block = ledger.mine_next_block(&alice, 1).unwrap();
    assert!(
        block
            .transactions
            .iter()
            .any(|tx| tx.signature() == mine.signature())
    );
    ledger.apply_locally_mined_block(block).unwrap();

    assert!(
        ledger
            .available_utxos_for_address(alice.address())
            .unwrap()
            .iter()
            .any(|(outpoint, _)| outpoint == &mine_outpoint)
    );
    ledger
        .build_transfer_with_inputs(
            &alice,
            bob.address(),
            TEST_BURN_AMOUNT,
            0,
            std::slice::from_ref(&mine_outpoint),
        )
        .unwrap();
}

#[test]
fn burns_built_after_pending_mine_do_not_spend_pending_mine_output() {
    let alice = Wallet::from_seed("pending-mine-burn-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let mine = ledger.build_mine(alice.address()).unwrap();
    let mine_outpoint = OutPoint {
        txid: mine.signature().to_string(),
        index: 0,
    };
    ledger.submit_transaction(mine).unwrap();

    let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();

    let Transaction::Burn { inputs, .. } = &burn else {
        panic!("expected burn transaction");
    };
    assert!(!inputs.iter().any(|input| input.outpoint == mine_outpoint));
}

#[test]
fn pending_blinded_transactions_with_spent_inputs_are_pruned_after_block_apply() {
    let alice = Wallet::from_seed("pending-blind-spent-prune-alice");
    let mut mempool_ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);
    let mut block_ledger = mempool_ledger.clone();
    let amount = mempool_ledger.balance_of(alice.address());
    let blinded = mempool_ledger
        .build_blinded_burn(&alice, amount, 0, mempool_ledger.height() + 4)
        .unwrap();
    mempool_ledger
        .submit_blinded_transaction(blinded.transaction.clone())
        .unwrap();

    let burn = block_ledger.build_burn(&alice, amount, 0).unwrap();
    let Transaction::Burn { inputs, .. } = &burn else {
        panic!("expected burn transaction");
    };
    assert!(blinded.transaction.inputs.iter().any(|input| {
        inputs
            .iter()
            .any(|burn_input| burn_input.outpoint == input.outpoint)
    }));
    block_ledger.submit_transaction(burn).unwrap();
    let block = block_ledger.mine_next_block(&alice, 1).unwrap();

    mempool_ledger.apply_block(block).unwrap();

    assert!(mempool_ledger.pending_blinded_transactions().is_empty());
}

#[test]
fn block_selection_limits_mine_actions_per_anchor() {
    let alice = Wallet::from_seed("mine-anchor-limit-selection-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let first_mine = ledger.build_mine(alice.address()).unwrap();
    ledger.submit_transaction(first_mine.clone()).unwrap();
    let second_mine = ledger.build_mine(alice.address()).unwrap();
    ledger.submit_transaction(second_mine.clone()).unwrap();
    assert_eq!(ledger.pending().len(), 3);
    let block = ledger.mine_next_block(&alice, 1).unwrap();

    assert_eq!(block.transactions.len(), 3);
    assert!(block.transactions.iter().any(Transaction::is_burn));
    let included_mines = block
        .transactions
        .iter()
        .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
        .count();
    assert_eq!(included_mines, MINE_ACTIONS_PER_ANCHOR_LIMIT);
    assert!(
        block
            .transactions
            .iter()
            .any(|tx| tx.signature() == first_mine.signature())
    );
    assert!(
        block
            .transactions
            .iter()
            .any(|tx| tx.signature() == second_mine.signature())
    );
    assert_ne!(first_mine.signature(), second_mine.signature());
    assert_eq!(block.reward, first_mine.fee() + second_mine.fee());
}

#[test]
fn blocks_reject_too_many_mine_actions_for_one_anchor() {
    let alice = Wallet::from_seed("mine-anchor-limit-block-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let first_mine = test_mine_with_salt(&ledger, alice.address(), 1);
    let second_mine = test_mine_with_salt(&ledger, alice.address(), 2);
    let third_mine = test_mine_with_salt(&ledger, alice.address(), 3);
    let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
    ledger.submit_transaction(burn).unwrap();
    let mut block = ledger
        .prepare_next_block(
            alice.address(),
            ledger
                .tip()
                .timestamp_ms
                .saturating_add(VDF_TARGET_BLOCK_MS),
        )
        .unwrap()
        .finish(&alice, "preverified-vdf".to_string());
    block.transactions.push(first_mine);
    block.transactions.push(second_mine);
    block.transactions.push(third_mine);
    block.reward = fee_reward(&block.transactions).unwrap();
    block.hash = block.compute_hash();

    let error = ledger
        .apply_preverified_block_at(block, u64::MAX)
        .unwrap_err();

    assert!(format!("{error:#}").contains("mine actions per anchor limit"));
}

#[test]
fn mempool_rejects_mine_actions_above_anchor_limit() {
    let alice = Wallet::from_seed("mine-anchor-limit-mempool-alice");
    let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

    let first_mine = test_mine_with_salt(&ledger, alice.address(), 1);
    let second_mine = test_mine_with_salt(&ledger, alice.address(), 2);
    let third_mine = test_mine_with_salt(&ledger, alice.address(), 3);
    ledger.submit_transaction(first_mine).unwrap();
    ledger.submit_transaction(second_mine).unwrap();
    let error = ledger.submit_transaction(third_mine).unwrap_err();

    assert!(format!("{error:#}").contains("mine transaction anchor limit reached"));
}

#[test]
fn mine_difficulty_increases_when_issuance_exceeds_target_window() {
    let alice = Wallet::from_seed("mine-difficulty-up-alice");
    let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);

    for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
        apply_preverified_burn_block_with_mines(&mut ledger, &alice, 2);
    }

    assert_eq!(
        ledger.current_mine_difficulty_bits(),
        MINE_DIFFICULTY_BITS + 1
    );
    let mine = ledger.build_mine(alice.address()).unwrap();
    let Transaction::Mine {
        difficulty_bits, ..
    } = mine
    else {
        panic!("expected mine action");
    };
    assert_eq!(difficulty_bits, MINE_DIFFICULTY_BITS + 1);
}

#[test]
fn mine_difficulty_decreases_when_issuance_is_below_target_window() {
    let alice = Wallet::from_seed("mine-difficulty-down-alice");
    let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);

    for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
        mine_burn_block_with_mines(&mut ledger, &alice, 0);
    }

    assert_eq!(
        ledger.current_mine_difficulty_bits(),
        MINE_DIFFICULTY_BITS - MINE_MAX_RETARGET_STEP_BITS
    );

    for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
        mine_burn_block_with_mines(&mut ledger, &alice, 0);
    }

    assert_eq!(
        ledger.current_mine_difficulty_bits(),
        MINE_MIN_DIFFICULTY_BITS
    );

    for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
        mine_burn_block_with_mines(&mut ledger, &alice, 0);
    }

    assert_eq!(
        ledger.current_mine_difficulty_bits(),
        MINE_MIN_DIFFICULTY_BITS
    );
}

#[test]
fn mine_actions_expire_when_anchor_is_too_old() {
    let alice = Wallet::from_seed("mine-anchor-expiry-alice");
    let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
    let stale_mine = ledger.build_mine(alice.address()).unwrap();

    for _ in 0..=MINE_MAX_ANCHOR_AGE_BLOCKS {
        mine_burn_block_with_mines(&mut ledger, &alice, 0);
    }

    let error = ledger.submit_transaction(stale_mine).unwrap_err();
    assert!(format!("{error:#}").contains("mine transaction anchor is too old"));
}

#[test]
fn pending_mine_actions_are_removed_when_anchor_expires() {
    let alice = Wallet::from_seed("pending-mine-anchor-expiry-alice");
    let bob = Wallet::from_seed("pending-mine-anchor-expiry-bob");
    let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
    ledger.launch_profile.max_block_transactions = 1;
    let stale_mine = ledger.build_mine(bob.address()).unwrap();
    ledger.submit_transaction(stale_mine.clone()).unwrap();

    for _ in 0..=MINE_MAX_ANCHOR_AGE_BLOCKS {
        mine_burn_block_with_mines(&mut ledger, &alice, 0);
    }

    assert!(
        ledger
            .pending()
            .iter()
            .all(|tx| tx.signature() != stale_mine.signature())
    );
}

#[test]
fn stratum_mine_header_proof_is_validated() {
    let alice = Wallet::from_seed("stratum-proof-alice");
    let ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
    let anchor = ledger.tip().hash.clone();
    let difficulty_bits = ledger.current_mine_difficulty_bits();
    let template = ledger
        .stratum_mine_template(alice.address(), anchor, 1, difficulty_bits)
        .unwrap();

    let mut accepted = None;
    for nonce in 0_u32..50_000 {
        let result = ledger.build_stratum_mine(
            template.clone(),
            StratumMineShare {
                extranonce2: [0, 0, 0, 0],
                header_nonce: nonce.to_le_bytes(),
            },
        );
        if let Ok(tx) = result {
            accepted = Some(tx);
            break;
        }
    }

    let tx = accepted.expect("expected Stratum proof within search range");
    let Transaction::Mine {
        proof_header,
        signature,
        ..
    } = tx
    else {
        panic!("expected mine action");
    };
    assert_eq!(proof_header.as_deref().unwrap_or_default().len(), 160);
    assert!(hash_meets_difficulty(&signature, difficulty_bits));
}

#[test]
fn stratum_mine_salt_allows_multiple_actions_for_same_anchor() {
    let alice = Wallet::from_seed("stratum-salt-alice");
    let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
    let anchor = ledger.tip().hash.clone();
    let difficulty_bits = ledger.current_mine_difficulty_bits();

    for salt in [1, 2] {
        let template = ledger
            .stratum_mine_template(alice.address(), anchor.clone(), salt, difficulty_bits)
            .unwrap();
        let mut accepted = None;
        for nonce in 0_u32..50_000 {
            let result = ledger.build_stratum_mine(
                template.clone(),
                StratumMineShare {
                    extranonce2: [0, 0, 0, 0],
                    header_nonce: nonce.to_le_bytes(),
                },
            );
            if let Ok(tx) = result {
                accepted = Some(tx);
                break;
            }
        }
        let tx = accepted.expect("expected Stratum proof within search range");
        assert!(ledger.submit_transaction(tx).unwrap());
    }

    assert_eq!(ledger.pending().len(), 2);
    let salts = ledger
        .pending()
        .iter()
        .map(|tx| match tx {
            Transaction::Mine { salt, .. } => *salt,
            _ => panic!("expected mine action"),
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(salts, BTreeSet::from([1, 2]));
}
