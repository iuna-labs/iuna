//! Ed25519-only wallet operations for IUNA.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        RwLock,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::{Result, bail};
use ed25519_dalek::{Signer, SigningKey};

use crate::consensus::{
    Address, Amount, ConsensusState, Hash, Input, OutPoint, Output, PROTOCOL_VERSION, PublicKey,
    Sidecar, TicketInput, TicketTransaction, Transaction, address_from_public_key, input_values,
    tagged_hash, validate_pending_ticket, validate_pending_transaction,
};

use super::consensus_node::{ChainRepository, Node};

/// Keeps automatic consolidation tickets comfortably below the 16 KiB Heartbeat limit while
/// still collapsing dust at each ten-Heartbeat purchase.
pub const AUTOMATIC_TICKET_MAX_INPUTS: usize = 32;
/// Recovery scans this deterministic window even when older addresses have already been emptied.
pub const WALLET_ADDRESS_SCAN_LIMIT: u32 = 1_000;

const RECEIVE_BRANCH: u8 = 0;
const CHANGE_BRANCH: u8 = 1;

pub struct Wallet {
    signing_key: SigningKey,
    key_cache: RwLock<BTreeMap<Address, SigningKey>>,
    change_cursor: AtomicU32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TicketParameters {
    pub budget: Amount,
    pub duration: u64,
    pub fee: Amount,
    pub mining_public_key: PublicKey,
    pub lottery_seed: Hash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmissionReceipt {
    pub id: Hash,
    pub fee: Amount,
}

impl Wallet {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&seed),
            key_cache: RwLock::new(BTreeMap::new()),
            change_cursor: AtomicU32::new(0),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn address(&self) -> Address {
        address_from_public_key(&self.public_key())
    }

    pub fn receive_address(&self, index: u32) -> Address {
        address_from_public_key(
            &self
                .child_key(RECEIVE_BRANCH, index)
                .verifying_key()
                .to_bytes(),
        )
    }

    pub fn change_address(&self, index: u32) -> Address {
        address_from_public_key(
            &self
                .child_key(CHANGE_BRANCH, index)
                .verifying_key()
                .to_bytes(),
        )
    }

    pub fn owned_addresses(&self) -> BTreeSet<Address> {
        self.keyring().keys().copied().collect()
    }

    /// Signs a non-empty Ledger candidate for a ticket whose mining key belongs to this wallet.
    /// Eligibility and transaction validity are checked by the node when the candidate is
    /// submitted; signing only proves that the winner authorized this exact content.
    pub fn sign_candidate(&self, mut candidate: Sidecar) -> Result<Sidecar> {
        if candidate.transactions.is_empty() {
            bail!("the unsigned deadline fallback must not be wallet-signed");
        }
        candidate.header.signature = self
            .signing_key
            .sign(&candidate.header.signing_digest())
            .to_bytes()
            .to_vec();
        Ok(candidate)
    }

    pub fn balance(&self, state: &ConsensusState) -> Amount {
        let keys = self.keyring();
        state
            .utxos
            .values()
            .filter(|output| keys.contains_key(&output.owner))
            .map(|output| output.value)
            .sum()
    }

    pub fn build_transaction(
        &self,
        state: &ConsensusState,
        reserved_inputs: &BTreeSet<OutPoint>,
        recipient: Address,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        if amount == 0 {
            bail!("Ledger amount must be positive");
        }
        let required = amount
            .checked_add(fee)
            .ok_or_else(|| anyhow::anyhow!("amount overflow"))?;
        let (selected, total) = self.select_inputs(state, reserved_inputs, required)?;
        let mut outputs = vec![Output {
            owner: recipient,
            value: amount,
        }];
        if total > required {
            let change_key = self.fresh_change_key(state)?;
            outputs.push(Output {
                owner: address_from_public_key(&change_key.verifying_key().to_bytes()),
                value: total - required,
            });
        }
        let mut transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: selected
                .into_iter()
                .map(|(previous_output, key)| Input {
                    previous_output,
                    public_key: key.verifying_key().to_bytes(),
                    signature: Vec::new(),
                })
                .collect(),
            outputs,
        };
        let values = input_values(
            &state.utxos,
            transaction
                .inputs
                .iter()
                .map(|input| &input.previous_output),
            "missing Ledger input",
        )?;
        for index in 0..transaction.inputs.len() {
            let key = self
                .key_for_public_key(&transaction.inputs[index].public_key)
                .ok_or_else(|| anyhow::anyhow!("selected Ledger input key is unavailable"))?;
            transaction.inputs[index].signature = self
                .sign_with_key(&key, &transaction.signing_digest(index, &values)?)
                .to_vec();
        }
        validate_pending_transaction(state, &transaction)?;
        Ok(transaction)
    }

    pub fn submit_transaction<R: ChainRepository>(
        &self,
        node: &mut Node<R>,
        recipient: Address,
        amount: Amount,
        fee: Amount,
    ) -> Result<SubmissionReceipt> {
        self.sync_change_cursor(node);
        self.release_change_on_error(|| {
            let transaction = self.build_transaction(
                node.chain().tip(),
                &node.reserved_inputs(),
                recipient,
                amount,
                fee,
            )?;
            let id = transaction.id();
            let fee = node.submit_transaction(transaction)?;
            Ok(SubmissionReceipt { id, fee })
        })
    }

    pub fn build_ticket(
        &self,
        state: &ConsensusState,
        reserved_inputs: &BTreeSet<OutPoint>,
        parameters: TicketParameters,
    ) -> Result<TicketTransaction> {
        if parameters.budget == 0 || parameters.fee == 0 {
            bail!("ticket expenditure and fee must both be positive");
        }
        let required = parameters
            .budget
            .checked_add(parameters.fee)
            .ok_or_else(|| anyhow::anyhow!("amount overflow"))?;
        let (points, total) = self.select_inputs(state, reserved_inputs, required)?;
        self.build_ticket_from_inputs(state, parameters, required, points, total)
    }

    pub fn build_consolidating_ticket(
        &self,
        state: &ConsensusState,
        reserved_inputs: &BTreeSet<OutPoint>,
        parameters: TicketParameters,
    ) -> Result<TicketTransaction> {
        if parameters.budget == 0 || parameters.fee == 0 {
            bail!("ticket expenditure and fee must both be positive");
        }
        let required = parameters
            .budget
            .checked_add(parameters.fee)
            .ok_or_else(|| anyhow::anyhow!("amount overflow"))?;
        let (points, total) = self.select_consolidating_inputs(
            state,
            reserved_inputs,
            required,
            AUTOMATIC_TICKET_MAX_INPUTS,
        )?;
        self.build_ticket_from_inputs(state, parameters, required, points, total)
    }

    fn build_ticket_from_inputs(
        &self,
        state: &ConsensusState,
        parameters: TicketParameters,
        required: Amount,
        points: Vec<(OutPoint, SigningKey)>,
        total: Amount,
    ) -> Result<TicketTransaction> {
        let mut ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: points
                .into_iter()
                .map(|(previous_output, key)| TicketInput {
                    previous_output,
                    public_key: key.verifying_key().to_bytes(),
                    signature: Vec::new(),
                })
                .collect(),
            budget: parameters.budget,
            duration: parameters.duration,
            mining_public_key: parameters.mining_public_key,
            lottery_seed: parameters.lottery_seed,
            change: if total > required {
                let key = self.fresh_change_key(state)?;
                Some(Output {
                    owner: address_from_public_key(&key.verifying_key().to_bytes()),
                    value: total - required,
                })
            } else {
                None
            },
        };
        let values = input_values(
            &state.utxos,
            ticket.inputs.iter().map(|input| &input.previous_output),
            "missing ticket input",
        )?;
        for index in 0..ticket.inputs.len() {
            let key = self
                .key_for_public_key(&ticket.inputs[index].public_key)
                .ok_or_else(|| anyhow::anyhow!("selected ticket input key is unavailable"))?;
            ticket.inputs[index].signature = self
                .sign_with_key(&key, &ticket.signing_digest(index, &values)?)
                .to_vec();
        }
        validate_pending_ticket(state, &ticket)?;
        Ok(ticket)
    }

    pub fn submit_ticket<R: ChainRepository>(
        &self,
        node: &mut Node<R>,
        parameters: TicketParameters,
    ) -> Result<SubmissionReceipt> {
        self.sync_change_cursor(node);
        self.release_change_on_error(|| {
            let ticket =
                self.build_ticket(node.chain().tip(), &node.reserved_inputs(), parameters)?;
            let id = ticket.id();
            let fee = node.submit_ticket(ticket)?;
            Ok(SubmissionReceipt { id, fee })
        })
    }

    pub fn submit_consolidating_ticket<R: ChainRepository>(
        &self,
        node: &mut Node<R>,
        parameters: TicketParameters,
    ) -> Result<SubmissionReceipt> {
        self.sync_change_cursor(node);
        self.release_change_on_error(|| {
            let ticket = self.build_consolidating_ticket(
                node.chain().tip(),
                &node.reserved_inputs(),
                parameters,
            )?;
            let id = ticket.id();
            let fee = node.submit_ticket(ticket)?;
            Ok(SubmissionReceipt { id, fee })
        })
    }

    fn select_inputs(
        &self,
        state: &ConsensusState,
        reserved_inputs: &BTreeSet<OutPoint>,
        required: Amount,
    ) -> Result<(Vec<(OutPoint, SigningKey)>, Amount)> {
        let keys = self.keyring();
        let mut points = Vec::new();
        let mut total = 0u64;
        for (point, output) in &state.utxos {
            let Some(key) = keys.get(&output.owner) else {
                continue;
            };
            if reserved_inputs.contains(point) {
                continue;
            }
            points.push((point.clone(), key.clone()));
            total = total
                .checked_add(output.value)
                .ok_or_else(|| anyhow::anyhow!("balance overflow"))?;
            if total >= required {
                return Ok((points, total));
            }
        }
        bail!("insufficient available balance")
    }

    fn select_consolidating_inputs(
        &self,
        state: &ConsensusState,
        reserved_inputs: &BTreeSet<OutPoint>,
        required: Amount,
        max_inputs: usize,
    ) -> Result<(Vec<(OutPoint, SigningKey)>, Amount)> {
        let keys = self.keyring();
        let mut available = state
            .utxos
            .iter()
            .filter_map(|(point, output)| {
                (!reserved_inputs.contains(point))
                    .then(|| {
                        keys.get(&output.owner)
                            .map(|key| (point.clone(), output.value, key.clone()))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        available.sort_by(
            |(left_point, left_value, _), (right_point, right_value, _)| {
                left_value
                    .cmp(right_value)
                    .then_with(|| left_point.cmp(right_point))
            },
        );
        let available_total = available.iter().try_fold(0_u64, |total, (_, value, _)| {
            total
                .checked_add(*value)
                .ok_or_else(|| anyhow::anyhow!("balance overflow"))
        })?;
        if available_total < required {
            bail!("insufficient available balance");
        }

        // Preserve the largest coin as immediately spendable liquidity whenever the remaining
        // coins can fund the ticket. Automatic mining can then consolidate dust without locking
        // the wallet's entire balance while the ticket is pending.
        let fundable_without_largest = available.len() > 1
            && available[..available.len() - 1]
                .iter()
                .rev()
                .take(max_inputs)
                .map(|(_, value, _)| *value)
                .sum::<u64>()
                >= required;
        let protect_largest = fundable_without_largest;
        let candidate_count = available.len() - usize::from(protect_largest);
        let candidates = &available[..candidate_count];
        let mut selected = BTreeSet::new();
        let mut total = 0_u64;
        for (point, value, _) in candidates.iter().rev() {
            if selected.len() >= max_inputs {
                bail!("automatic Ledger ticket needs more than {max_inputs} inputs");
            }
            selected.insert(point.clone());
            total = total
                .checked_add(*value)
                .ok_or_else(|| anyhow::anyhow!("balance overflow"))?;
            if total >= required {
                break;
            }
        }
        if total < required {
            bail!("insufficient available balance");
        }

        for (point, value, _) in candidates {
            if selected.len() >= max_inputs || selected.contains(point) {
                continue;
            }
            // If the largest coin could not be protected, still leave one unreserved output when
            // possible instead of turning every wallet operation into a child of this ticket.
            if !protect_largest && selected.len() + 1 >= candidates.len() {
                break;
            }
            selected.insert(point.clone());
            total = total
                .checked_add(*value)
                .ok_or_else(|| anyhow::anyhow!("balance overflow"))?;
        }
        Ok((
            selected
                .into_iter()
                .map(|point| {
                    let key = available
                        .iter()
                        .find(|(candidate, _, _)| candidate == &point)
                        .expect("selected point came from available inputs")
                        .2
                        .clone();
                    (point, key)
                })
                .collect(),
            total,
        ))
    }

    fn child_key(&self, branch: u8, index: u32) -> SigningKey {
        let mut material = Vec::with_capacity(37);
        material.extend_from_slice(&self.signing_key.to_bytes());
        material.push(branch);
        material.extend_from_slice(&index.to_be_bytes());
        SigningKey::from_bytes(&tagged_hash(b"IUNA-WALLET-CHILD-v1", &material))
    }

    fn keyring(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<Address, SigningKey>> {
        if self
            .key_cache
            .read()
            .expect("wallet key cache lock poisoned")
            .is_empty()
        {
            let mut keys = self
                .key_cache
                .write()
                .expect("wallet key cache lock poisoned");
            keys.insert(self.address(), self.signing_key.clone());
            for index in 0..WALLET_ADDRESS_SCAN_LIMIT {
                for branch in [RECEIVE_BRANCH, CHANGE_BRANCH] {
                    let key = self.child_key(branch, index);
                    keys.insert(
                        address_from_public_key(&key.verifying_key().to_bytes()),
                        key,
                    );
                }
            }
        }
        self.key_cache
            .read()
            .expect("wallet key cache lock poisoned")
    }

    fn key_for_public_key(&self, public_key: &PublicKey) -> Option<SigningKey> {
        self.keyring()
            .get(&address_from_public_key(public_key))
            .cloned()
    }

    fn fresh_change_key(&self, state: &ConsensusState) -> Result<SigningKey> {
        let mut index = self.change_cursor.load(Ordering::Relaxed);
        while index < WALLET_ADDRESS_SCAN_LIMIT {
            let key = self.child_key(CHANGE_BRANCH, index);
            let address = address_from_public_key(&key.verifying_key().to_bytes());
            if !state.utxos.values().any(|output| output.owner == address) {
                self.change_cursor
                    .store(index.saturating_add(1), Ordering::Relaxed);
                return Ok(key);
            }
            index = index.saturating_add(1);
        }
        bail!("wallet change-address scan limit reached")
    }

    /// A change address is only consumed once its object reaches the mempool. Rejected
    /// submissions hand the index back, so retries cannot open a gap wider than the recovery
    /// gap limit and hide later change from a restored wallet.
    fn release_change_on_error<T>(&self, submit: impl FnOnce() -> Result<T>) -> Result<T> {
        let before = self.change_cursor.load(Ordering::Relaxed);
        let result = submit();
        if result.is_err() {
            self.change_cursor.store(before, Ordering::Relaxed);
        }
        result
    }

    fn sync_change_cursor<R: ChainRepository>(&self, node: &mut Node<R>) {
        let mut index = self.change_cursor.load(Ordering::Relaxed);
        while index < WALLET_ADDRESS_SCAN_LIMIT && node.address_used(&self.change_address(index)) {
            index = index.saturating_add(1);
        }
        self.change_cursor.fetch_max(index, Ordering::Relaxed);
    }

    fn sign_with_key(&self, key: &SigningKey, message: &[u8]) -> [u8; 64] {
        key.sign(message).to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::consensus::{
        Chain, GENESIS_MINER, INITIAL_LEDGER_SUBSIDY, minimum_ticket_budget, pinned_genesis,
    };

    #[derive(Clone, Default)]
    struct MemoryRepository(Arc<Mutex<Option<Chain>>>);

    impl ChainRepository for MemoryRepository {
        fn load(&self) -> Result<Option<Chain>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn save(&self, chain: &Chain) -> Result<()> {
            *self.0.lock().unwrap() = Some(chain.clone());
            Ok(())
        }
    }

    const GENESIS_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];

    fn minimum_test_ticket_budget() -> u64 {
        minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap()
    }

    fn automatic_test_ticket_budget() -> u64 {
        minimum_ticket_budget(10, INITIAL_LEDGER_SUBSIDY).unwrap()
    }

    #[test]
    fn browser_and_node_wallets_derive_the_same_child_addresses() {
        let vector: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/vectors/browser_wallet_transaction.json"
        ))
        .unwrap();
        let wallet = Wallet::from_seed([0; 32]);
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(vector["address_hex"], hex(&wallet.address()));
        assert_eq!(
            vector["receive_0_address_hex"],
            hex(&wallet.receive_address(0))
        );
        assert_eq!(
            vector["change_0_address_hex"],
            hex(&wallet.change_address(0))
        );
    }

    #[test]
    fn builds_valid_ledger_transactions_and_mining_tickets() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let (_, genesis) = pinned_genesis().unwrap();
        assert_eq!(wallet.public_key(), GENESIS_MINER);
        assert_eq!(wallet.address(), address_from_public_key(&GENESIS_MINER));
        assert_ne!(wallet.receive_address(0), wallet.change_address(0));

        let transaction = wallet
            .build_transaction(&genesis, &BTreeSet::new(), [91; 32], 10_000_000, 0)
            .unwrap();
        assert_eq!(
            validate_pending_transaction(&genesis, &transaction).unwrap(),
            0
        );
        assert_eq!(transaction.outputs[1].owner, wallet.change_address(0));

        let ticket = wallet
            .build_ticket(
                &genesis,
                &BTreeSet::new(),
                TicketParameters {
                    budget: minimum_test_ticket_budget(),
                    duration: 1,
                    fee: 1,
                    mining_public_key: [92; 32],
                    lottery_seed: [93; 32],
                },
            )
            .unwrap();
        assert_eq!(validate_pending_ticket(&genesis, &ticket).unwrap(), 1);
        assert_eq!(
            ticket.change.as_ref().unwrap().owner,
            wallet.change_address(1)
        );
    }

    #[test]
    fn spends_outputs_from_a_derived_receive_key() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let (_, mut state) = pinned_genesis().unwrap();
        state.utxos.clear();
        state.utxos.insert(
            OutPoint {
                transaction_id: [44; 32],
                output_index: 0,
            },
            Output {
                owner: wallet.receive_address(0),
                value: 1_000,
            },
        );
        let transaction = wallet
            .build_transaction(&state, &BTreeSet::new(), [45; 32], 900, 1)
            .unwrap();
        assert_eq!(
            address_from_public_key(&transaction.inputs[0].public_key),
            wallet.receive_address(0)
        );
        assert_eq!(validate_pending_transaction(&state, &transaction), Ok(1));
    }

    #[test]
    fn automatic_tickets_consolidate_dust_but_preserve_the_largest_liquid_output() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let (_, mut state) = pinned_genesis().unwrap();
        let budget = automatic_test_ticket_budget();
        let required = budget + 1;
        state.utxos.clear();
        let output = |value| Output {
            owner: wallet.address(),
            value,
        };
        state.utxos.insert(
            OutPoint {
                transaction_id: [0; 32],
                output_index: 0,
            },
            output(required),
        );
        for (index, value) in [1, 2, 3, 4].into_iter().enumerate() {
            state.utxos.insert(
                OutPoint {
                    transaction_id: [u8::try_from(index + 1).unwrap(); 32],
                    output_index: 0,
                },
                output(value),
            );
        }
        let largest = OutPoint {
            transaction_id: [9; 32],
            output_index: 0,
        };
        state
            .utxos
            .insert(largest.clone(), output(required.saturating_mul(2)));
        let parameters = TicketParameters {
            budget,
            duration: 10,
            fee: 1,
            mining_public_key: wallet.public_key(),
            lottery_seed: [42; 32],
        };

        let manual = wallet
            .build_ticket(&state, &BTreeSet::new(), parameters.clone())
            .unwrap();
        assert_eq!(manual.inputs.len(), 1);

        let automatic = wallet
            .build_consolidating_ticket(&state, &BTreeSet::new(), parameters)
            .unwrap();
        assert_eq!(automatic.inputs.len(), 5);
        assert!(
            automatic
                .inputs
                .iter()
                .all(|input| input.previous_output != largest)
        );
        assert_eq!(automatic.change.as_ref().unwrap().value, 10);
        assert_eq!(validate_pending_ticket(&state, &automatic).unwrap(), 1);
    }

    #[test]
    fn automatic_ticket_consolidation_is_bounded() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let (_, mut state) = pinned_genesis().unwrap();
        let budget = automatic_test_ticket_budget();
        let required = budget + 1;
        state.utxos.clear();
        for index in 0_u8..40 {
            state.utxos.insert(
                OutPoint {
                    transaction_id: [index; 32],
                    output_index: 0,
                },
                Output {
                    owner: wallet.address(),
                    value: required,
                },
            );
        }
        let ticket = wallet
            .build_consolidating_ticket(
                &state,
                &BTreeSet::new(),
                TicketParameters {
                    budget,
                    duration: 10,
                    fee: 1,
                    mining_public_key: wallet.public_key(),
                    lottery_seed: [43; 32],
                },
            )
            .unwrap();

        assert_eq!(ticket.inputs.len(), AUTOMATIC_TICKET_MAX_INPUTS);
        assert!(ticket.canonical_size() < crate::consensus::MAX_HEARTBEAT_BLOCK_BYTES);
    }

    #[test]
    fn reserved_inputs_are_not_selected_twice() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let (_, genesis) = pinned_genesis().unwrap();
        let reserved = genesis.utxos.keys().cloned().collect();

        assert!(
            wallet
                .build_transaction(&genesis, &reserved, [94; 32], 1, 0)
                .is_err()
        );
        assert!(
            wallet
                .build_ticket(
                    &genesis,
                    &reserved,
                    TicketParameters {
                        budget: minimum_test_ticket_budget(),
                        duration: 1,
                        fee: 1,
                        mining_public_key: [95; 32],
                        lottery_seed: [96; 32],
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn application_commands_build_reserve_and_submit_wallet_objects() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let mut transaction_node = Node::open(MemoryRepository::default()).unwrap();
        let receipt = wallet
            .submit_transaction(&mut transaction_node, [97; 32], 10_000_000, 7)
            .unwrap();
        assert_eq!(receipt.fee, 7);
        assert!(transaction_node.has_pending_transaction(&receipt.id));
        assert_eq!(transaction_node.reserved_inputs().len(), 1);

        let mut ticket_node = Node::open(MemoryRepository::default()).unwrap();
        let receipt = wallet
            .submit_ticket(
                &mut ticket_node,
                TicketParameters {
                    budget: minimum_test_ticket_budget(),
                    duration: 1,
                    fee: 3,
                    mining_public_key: [98; 32],
                    lottery_seed: [99; 32],
                },
            )
            .unwrap();
        assert_eq!(receipt.fee, 3);
        assert!(ticket_node.has_pending_ticket(&receipt.id));
        assert_eq!(ticket_node.reserved_inputs().len(), 1);
    }

    #[test]
    fn rejected_submissions_do_not_consume_change_addresses() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        for _ in 0..3 {
            assert!(
                wallet
                    .submit_transaction(&mut node, [97; 32], 10_000_000, 0)
                    .is_err()
            );
        }
        let receipt = wallet
            .submit_transaction(&mut node, [97; 32], 10_000_000, 7)
            .unwrap();
        let transaction = node
            .pending_transactions()
            .into_iter()
            .find(|transaction| transaction.id() == receipt.id)
            .unwrap();
        assert_eq!(transaction.outputs[1].owner, wallet.change_address(0));
    }
}
