use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use crate::compact::CompactBlockContext;

use super::fork::{ForkChoice, ForkPoint, ForkQuality};
use super::genesis::{build_genesis_block, utxos_after_genesis, validate_genesis_block};
use super::ledger_ops::validate_genesis_allocations;
use super::ticket::genesis_tickets;
use super::{
    Amount, ChainSnapshot, GenesisBurn, LaunchProfile, Ledger, MINE_REWARD, Transaction,
    unix_now_ms,
};

impl Ledger {
    pub fn new(genesis_allocations: BTreeMap<String, Amount>, vdf_rounds: u64) -> Self {
        Self::new_with_genesis_transactions(
            genesis_allocations,
            Vec::new(),
            vdf_rounds,
            LaunchProfile::default(),
        )
        .expect("empty genesis transactions are valid")
    }

    pub fn new_with_genesis_burns(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_burns: Vec<GenesisBurn>,
        vdf_rounds: u64,
    ) -> Result<Self> {
        Self::new_with_genesis_burns_and_profile(
            genesis_allocations,
            genesis_burns,
            vdf_rounds,
            LaunchProfile::default(),
        )
    }

    pub fn new_with_genesis_burns_and_profile(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_burns: Vec<GenesisBurn>,
        vdf_rounds: u64,
        launch_profile: LaunchProfile,
    ) -> Result<Self> {
        let transactions = genesis_burns
            .into_iter()
            .map(|burn| {
                let allocation = genesis_allocations
                    .get(&burn.from)
                    .copied()
                    .unwrap_or_default();
                Transaction::genesis_burn_with_allocation(burn.from, burn.amount, allocation)
            })
            .collect::<Result<Vec<_>>>()?;
        Self::new_with_genesis_transactions(
            genesis_allocations,
            transactions,
            vdf_rounds,
            launch_profile,
        )
    }

    fn new_with_genesis_transactions(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_transactions: Vec<Transaction>,
        vdf_rounds: u64,
        launch_profile: LaunchProfile,
    ) -> Result<Self> {
        validate_genesis_allocations(&genesis_allocations)?;
        let genesis = build_genesis_block(&genesis_allocations, genesis_transactions);
        let utxos = utxos_after_genesis(&genesis_allocations, &genesis)?;
        let tickets = genesis_tickets(&genesis_allocations, &genesis, &launch_profile)?;
        let compact_block_context = if genesis_allocations.is_empty() {
            CompactBlockContext::default()
        } else {
            CompactBlockContext::for_chain(&genesis_allocations, std::slice::from_ref(&genesis))?
        };
        Ok(Self {
            chain: vec![genesis],
            genesis_allocations: genesis_allocations.clone(),
            utxos,
            utxo_lineage: BTreeMap::new(),
            lineage_values: BTreeMap::new(),
            lineage_owners: BTreeMap::new(),
            tickets,
            pending: Vec::new(),
            orphans: Vec::new(),
            pending_bytes: 0,
            orphan_bytes: 0,
            mine_reward: MINE_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
            compact_block_context,
        })
    }

    pub fn from_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, unix_now_ms())
    }

    pub fn from_persisted_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, u64::MAX)
    }

    pub(crate) fn from_preverified_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_with_vdf_policy(snapshot, false, u64::MAX)
    }

    pub(crate) fn from_snapshot_at(snapshot: ChainSnapshot, now_ms: u64) -> Result<Self> {
        Self::from_snapshot_with_vdf_policy(snapshot, true, now_ms)
    }

    pub(crate) fn from_snapshot_with_vdf_policy(
        snapshot: ChainSnapshot,
        verify_vdf: bool,
        now_ms: u64,
    ) -> Result<Self> {
        let ChainSnapshot {
            genesis_allocations,
            vdf_rounds,
            launch_profile,
            blocks,
        } = snapshot;

        if blocks.is_empty() {
            bail!("chain snapshot is empty");
        }

        validate_genesis_allocations(&genesis_allocations)?;
        let genesis = blocks[0].clone();
        validate_genesis_block(&genesis)?;
        let expected_genesis =
            build_genesis_block(&genesis_allocations, genesis.transactions.clone());
        if genesis != expected_genesis {
            bail!("chain snapshot genesis does not match its allocations and transactions");
        }
        let utxos = utxos_after_genesis(&genesis_allocations, &genesis)?;
        let compact_block_context =
            CompactBlockContext::for_chain(&genesis_allocations, std::slice::from_ref(&genesis))?;

        let mut ledger = Self {
            chain: vec![genesis],
            genesis_allocations,
            utxos,
            utxo_lineage: BTreeMap::new(),
            lineage_values: BTreeMap::new(),
            lineage_owners: BTreeMap::new(),
            tickets: Vec::new(),
            pending: Vec::new(),
            orphans: Vec::new(),
            pending_bytes: 0,
            orphan_bytes: 0,
            mine_reward: MINE_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
            compact_block_context,
        };
        ledger.tickets = genesis_tickets(
            &ledger.genesis_allocations,
            ledger.tip(),
            &ledger.launch_profile,
        )?;

        for block in blocks.into_iter().skip(1) {
            if verify_vdf {
                ledger.apply_block_at(block, now_ms)?;
            } else {
                ledger.apply_preverified_block_at(block, now_ms)?;
            }
        }
        Ok(ledger)
    }

    pub fn extend_from_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, true, unix_now_ms())
    }

    pub(crate) fn extend_from_snapshot_at(
        &mut self,
        snapshot: ChainSnapshot,
        now_ms: u64,
    ) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, true, now_ms)
    }

    pub(crate) fn extend_from_snapshot_with_vdf_policy(
        &mut self,
        snapshot: ChainSnapshot,
        verify_vdf: bool,
        now_ms: u64,
    ) -> Result<bool> {
        self.validate_snapshot_identity(&snapshot)?;
        let candidate = Self::from_snapshot_with_vdf_policy(snapshot, verify_vdf, now_ms)?;
        let fork_point = self.fork_point_with_candidate(&candidate)?;

        if self.choose_fork(&candidate, fork_point) == ForkChoice::KeepLocal {
            return Ok(false);
        }

        self.replace_with_better_chain(candidate, fork_point);

        Ok(true)
    }

    fn validate_snapshot_identity(&self, snapshot: &ChainSnapshot) -> Result<u64> {
        if snapshot.blocks.is_empty() {
            bail!("chain snapshot is empty");
        }
        if snapshot.vdf_rounds != self.initial_vdf_rounds {
            bail!("chain snapshot initial VDF rounds do not match local chain");
        }
        if snapshot.launch_profile != self.launch_profile {
            bail!("chain snapshot launch profile does not match local chain");
        }
        if snapshot.genesis_allocations != self.genesis_allocations {
            bail!("chain snapshot genesis allocations do not match local chain");
        }
        if snapshot.blocks[0].hash != self.genesis_hash() {
            bail!("chain snapshot genesis does not match local chain");
        }

        let remote_height = snapshot
            .blocks
            .last()
            .map(|block| block.height)
            .unwrap_or(0);

        Ok(remote_height)
    }

    fn fork_point_with_candidate(&self, candidate: &Ledger) -> Result<ForkPoint> {
        if candidate.genesis_hash() != self.genesis_hash() {
            bail!("candidate chain has no common genesis block");
        }
        let max_common_index = self.chain.len().min(candidate.chain.len()) - 1;
        for index in 0..=max_common_index {
            if self.chain[index] != candidate.chain[index] {
                if index == 0 {
                    bail!("candidate chain has no common genesis block");
                }
                return Ok(ForkPoint {
                    common_ancestor_height: index as u64 - 1,
                });
            }
        }
        Ok(ForkPoint {
            common_ancestor_height: max_common_index as u64,
        })
    }

    fn choose_fork(&self, candidate: &Ledger, fork_point: ForkPoint) -> ForkChoice {
        let local_height = self.height();
        let remote_height = candidate.height();
        if remote_height == local_height && candidate.tip().hash == self.tip().hash {
            return ForkChoice::KeepLocal;
        }

        if fork_rewrites_finalized_history(local_height, fork_point.common_ancestor_height) {
            return ForkChoice::KeepLocal;
        }

        if remote_height > local_height {
            return ForkChoice::SwitchToCandidate;
        }
        if remote_height < local_height {
            return ForkChoice::KeepLocal;
        }

        match self.fork_quality(candidate, fork_point) {
            ForkQuality::RemoteBetter => ForkChoice::SwitchToCandidate,
            ForkQuality::LocalBetter | ForkQuality::Equal => ForkChoice::KeepLocal,
        }
    }

    fn fork_quality(&self, candidate: &Ledger, fork_point: ForkPoint) -> ForkQuality {
        let local_fork = self
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize);
        let remote_fork = candidate
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize);
        for (local, remote) in local_fork.zip(remote_fork) {
            match local.leader_score().cmp(&remote.leader_score()) {
                std::cmp::Ordering::Equal => continue,
                ordering => return ForkQuality::from(ordering),
            }
        }
        ForkQuality::Equal
    }

    fn replace_with_better_chain(&mut self, mut candidate: Ledger, fork_point: ForkPoint) {
        let mut carry_forward = self.pending.clone();
        carry_forward.extend(self.orphans.clone());
        for block in self
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize)
        {
            carry_forward.extend(block.transactions.clone());
        }

        let mined_signatures = candidate
            .chain
            .iter()
            .flat_map(|block| block.transactions.iter())
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        for transaction in carry_forward {
            if !mined_signatures.contains(transaction.signature()) {
                let _ = candidate.submit_transaction(transaction);
            }
        }

        *self = candidate;
    }
}

fn fork_rewrites_finalized_history(local_height: u64, common_ancestor_height: u64) -> bool {
    let finalized_floor = local_height.saturating_sub(super::FORK_FINALITY_DEPTH);
    common_ancestor_height < finalized_floor
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::fork_rewrites_finalized_history;
    use crate::domain::{
        FORK_FINALITY_DEPTH, LaunchProfile, Ledger, StratumMineShare,
        TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT, Transaction, Wallet,
    };

    fn profile(profile_id: &str) -> LaunchProfile {
        LaunchProfile {
            profile_id: profile_id.to_string(),
            mine_difficulty_bits: 0,
            ..LaunchProfile::default()
        }
    }

    fn ledger_with_profile(allocations: BTreeMap<String, u64>, profile_id: &str) -> Ledger {
        Ledger::new_with_genesis_burns_and_profile(allocations, Vec::new(), 1, profile(profile_id))
            .unwrap()
    }

    fn set_next_height(ledger: &mut Ledger, next_height: u64) {
        ledger.chain.last_mut().unwrap().height = next_height.saturating_sub(1);
    }

    #[test]
    fn forks_may_rewrite_six_blocks_but_not_seven() {
        assert_eq!(FORK_FINALITY_DEPTH, 6);
        assert!(!fork_rewrites_finalized_history(100, 94));
        assert!(fork_rewrites_finalized_history(100, 93));
        assert!(!fork_rewrites_finalized_history(5, 0));
    }

    #[test]
    fn transfer_and_burn_signatures_cannot_replay_between_chain_ids() {
        let alice = Wallet::from_seed("chain-replay-alice");
        let bob = Wallet::from_seed("chain-replay-bob");
        let allocations = BTreeMap::from([(alice.address().to_string(), 100)]);
        let chain_ids = [
            "iuna-mainnet-candidate",
            "iuna-mainnet-v1",
            "iuna-testnet-v1",
        ];

        for foreign_chain_id in &chain_ids[1..] {
            let mut source = ledger_with_profile(allocations.clone(), chain_ids[0]);
            let mut foreign = ledger_with_profile(allocations.clone(), foreign_chain_id);
            set_next_height(&mut source, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
            set_next_height(&mut foreign, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
            assert_eq!(source.genesis_hash(), foreign.genesis_hash());
            assert_eq!(source.utxos, foreign.utxos);

            let transfer = source.build_transfer(&alice, bob.address(), 10, 1).unwrap();
            let transfer_error = foreign.submit_transaction(transfer).unwrap_err();
            assert!(
                transfer_error
                    .to_string()
                    .contains("transaction signature is invalid")
            );

            let burn = source.build_burn(&alice, 10, 1).unwrap();
            let burn_error = foreign.submit_transaction(burn).unwrap_err();
            assert!(
                burn_error
                    .to_string()
                    .contains("transaction signature is invalid")
            );
        }
    }

    #[test]
    fn legacy_signatures_remain_compatible_before_height_1000() {
        let alice = Wallet::from_seed("pre-activation-replay-alice");
        let bob = Wallet::from_seed("pre-activation-replay-bob");
        let allocations = BTreeMap::from([(alice.address().to_string(), 100)]);
        let mut source = ledger_with_profile(allocations.clone(), "legacy-chain-a");
        let mut foreign = ledger_with_profile(allocations, "legacy-chain-b");
        set_next_height(&mut source, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT - 1);
        set_next_height(&mut foreign, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT - 1);

        let transfer = source.build_transfer(&alice, bob.address(), 10, 1).unwrap();

        assert!(foreign.submit_transaction(transfer).unwrap());
    }

    #[test]
    fn signatures_cannot_replay_between_distinct_genesis_hashes() {
        let alice = Wallet::from_seed("genesis-replay-alice");
        let bob = Wallet::from_seed("genesis-replay-bob");
        let base_allocations = BTreeMap::from([(alice.address().to_string(), 100)]);
        let other_allocations = BTreeMap::from([
            (alice.address().to_string(), 100),
            (bob.address().to_string(), 1),
        ]);
        let mut source = ledger_with_profile(base_allocations, "same-chain-id");
        let mut foreign = ledger_with_profile(other_allocations, "same-chain-id");
        set_next_height(&mut source, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        set_next_height(&mut foreign, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        assert_ne!(source.genesis_hash(), foreign.genesis_hash());

        let transfer = source.build_transfer(&alice, bob.address(), 10, 1).unwrap();
        let error = foreign.submit_transaction(transfer).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("transaction signature is invalid")
        );
    }

    #[test]
    fn mine_proofs_cannot_replay_between_chain_ids() {
        let miner = Wallet::from_seed("mine-replay-miner");
        let allocations = BTreeMap::from([(miner.address().to_string(), 100)]);
        let mut source = ledger_with_profile(allocations.clone(), "mine-chain-a");
        let mut foreign = ledger_with_profile(allocations, "mine-chain-b");
        set_next_height(&mut source, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        set_next_height(&mut foreign, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        assert_eq!(source.genesis_hash(), foreign.genesis_hash());

        let mine = source.build_mine(miner.address()).unwrap();
        let error = foreign.submit_transaction(mine).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("mine transaction proof hash is invalid")
        );
    }

    #[test]
    fn stratum_mine_proofs_cannot_replay_between_chain_ids() {
        let miner = Wallet::from_seed("stratum-replay-miner");
        let allocations = BTreeMap::from([(miner.address().to_string(), 100)]);
        let mut source = ledger_with_profile(allocations.clone(), "stratum-chain-a");
        let mut foreign = ledger_with_profile(allocations, "stratum-chain-b");
        set_next_height(&mut source, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        set_next_height(&mut foreign, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        let template = source
            .stratum_mine_template(
                miner.address(),
                source.genesis_hash(),
                7,
                source.current_mine_difficulty_bits(),
            )
            .unwrap();
        let mine = (0..u32::MAX)
            .find_map(|nonce| {
                source
                    .build_stratum_mine(
                        template.clone(),
                        StratumMineShare {
                            extranonce2: [0; 4],
                            header_nonce: nonce.to_le_bytes(),
                        },
                    )
                    .ok()
            })
            .expect("test difficulty must yield a Stratum share");

        let error = foreign.submit_transaction(mine).unwrap_err();

        assert!(error.to_string().contains("proof header is invalid"));
    }

    #[test]
    fn transaction_hex_casing_cannot_be_malleated_after_signing() {
        let alice = Wallet::from_seed("hex-malleability-alice");
        let bob = Wallet::from_seed("hex-malleability-bob");
        let mut ledger = ledger_with_profile(
            BTreeMap::from([(alice.address().to_string(), 100)]),
            "hex-malleability-chain",
        );
        set_next_height(&mut ledger, TRANSACTION_SIGNING_V1_ACTIVATION_HEIGHT);
        let transaction = ledger.build_transfer(&alice, bob.address(), 10, 1).unwrap();

        let reject = |mutated| {
            let error = ledger
                .clone()
                .submit_transaction(mutated)
                .expect_err("noncanonical transaction hex must be rejected");
            assert!(
                format!("{error:#}").contains("canonical lowercase"),
                "unexpected rejection: {error:#}"
            );
        };

        let mut recipient = transaction.clone();
        let Transaction::Transfer { outputs, .. } = &mut recipient else {
            unreachable!()
        };
        outputs[0].address.make_ascii_uppercase();
        reject(recipient);

        let mut change = transaction.clone();
        let Transaction::Transfer { outputs, .. } = &mut change else {
            unreachable!()
        };
        outputs[1].address.make_ascii_uppercase();
        reject(change);

        let mut owner = transaction.clone();
        let Transaction::Transfer { inputs, .. } = &mut owner else {
            unreachable!()
        };
        inputs[0].owner.make_ascii_uppercase();
        reject(owner);

        let mut outpoint = transaction.clone();
        let Transaction::Transfer { inputs, .. } = &mut outpoint else {
            unreachable!()
        };
        inputs[0].outpoint.txid.make_ascii_uppercase();
        reject(outpoint);

        let mut signature = transaction;
        let Transaction::Transfer {
            inputs,
            signature: transaction_signature,
            ..
        } = &mut signature
        else {
            unreachable!()
        };
        transaction_signature.make_ascii_uppercase();
        for input in inputs {
            input.signature.make_ascii_uppercase();
        }
        reject(signature);
    }
}
