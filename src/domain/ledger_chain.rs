use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

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
        Self::new_with_genesis_transactions(genesis_allocations, Vec::new(), vdf_rounds)
            .expect("empty genesis transactions are valid")
    }

    pub fn new_with_genesis_burns(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_burns: Vec<GenesisBurn>,
        vdf_rounds: u64,
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
        Self::new_with_genesis_transactions(genesis_allocations, transactions, vdf_rounds)
    }

    fn new_with_genesis_transactions(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_transactions: Vec<Transaction>,
        vdf_rounds: u64,
    ) -> Result<Self> {
        validate_genesis_allocations(&genesis_allocations)?;
        let launch_profile = LaunchProfile::default();
        let genesis = build_genesis_block(&genesis_allocations, genesis_transactions);
        let utxos = utxos_after_genesis(&genesis_allocations, &genesis)?;
        let tickets = genesis_tickets(&genesis_allocations, &genesis, &launch_profile)?;
        Ok(Self {
            chain: vec![genesis],
            genesis_allocations: genesis_allocations.clone(),
            utxos,
            tickets,
            pending: Vec::new(),
            orphans: Vec::new(),
            pending_blinded: Vec::new(),
            pending_reveals: Vec::new(),
            pending_bytes: 0,
            orphan_bytes: 0,
            pending_blinded_bytes: 0,
            pending_reveal_bytes: 0,
            active_blinded: BTreeMap::new(),
            mine_reward: MINE_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
        })
    }

    pub fn from_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, unix_now_ms())
    }

    pub fn from_persisted_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, u64::MAX)
    }

    pub(crate) fn from_snapshot_at(snapshot: ChainSnapshot, now_ms: u64) -> Result<Self> {
        Self::from_snapshot_with_vdf_policy(snapshot, true, now_ms)
    }

    fn from_snapshot_with_vdf_policy(
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

        let mut ledger = Self {
            chain: vec![genesis],
            genesis_allocations,
            utxos,
            tickets: Vec::new(),
            pending: Vec::new(),
            orphans: Vec::new(),
            pending_blinded: Vec::new(),
            pending_reveals: Vec::new(),
            pending_bytes: 0,
            orphan_bytes: 0,
            pending_blinded_bytes: 0,
            pending_reveal_bytes: 0,
            active_blinded: BTreeMap::new(),
            mine_reward: MINE_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
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

    fn extend_from_snapshot_with_vdf_policy(
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

        let finalized_floor = local_height.saturating_sub(super::FORK_FINALITY_DEPTH);
        if fork_point.common_ancestor_height < finalized_floor {
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
        let mut carry_forward_blinded = self.pending_blinded.clone();
        let mut carry_forward_reveals = self.pending_reveals.clone();
        for block in self
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize)
        {
            carry_forward.extend(block.transactions.clone());
            carry_forward_blinded.extend(block.blinded_transactions.clone());
            carry_forward_reveals.extend(block.all_blinded_reveals().into_iter().cloned());
        }

        let mined_signatures = candidate
            .chain
            .iter()
            .flat_map(|block| block.transactions.iter())
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let mined_blinded_commitments = candidate
            .chain
            .iter()
            .flat_map(|block| block.blinded_transactions.iter())
            .map(|transaction| transaction.commitment.clone())
            .collect::<BTreeSet<_>>();
        let mined_reveal_commitments = candidate
            .chain
            .iter()
            .flat_map(|block| block.all_blinded_reveals())
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();

        for transaction in carry_forward {
            if !mined_signatures.contains(transaction.signature()) {
                let _ = candidate.submit_transaction(transaction);
            }
        }
        for transaction in carry_forward_blinded {
            if !mined_blinded_commitments.contains(&transaction.commitment) {
                let _ = candidate.submit_blinded_transaction(transaction);
            }
        }
        for reveal in carry_forward_reveals {
            if !mined_reveal_commitments.contains(&reveal.commitment) {
                let _ = candidate.submit_blinded_reveal(reveal);
            }
        }

        *self = candidate;
    }
}
