use anyhow::Result;

use crate::domain::{
    BlindedTransaction, Block, BuiltBlindedTransaction, Ledger,
    MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, OwnedBlindedTransaction, Transaction,
};

use super::{GossipEnvelope, NodeCore};

impl NodeCore {
    pub fn owned_blinded_payloads(&self) -> Vec<Transaction> {
        self.owned_blinded_payloads.values().cloned().collect()
    }

    pub fn owned_blinded_outbox_version(&self) -> u64 {
        self.owned_blinded_outbox_version
    }

    pub fn owned_blinded_transactions(&self) -> Vec<OwnedBlindedTransaction> {
        self.owned_blinded_transactions
            .iter()
            .filter_map(|(commitment, transaction)| {
                let payload = self.owned_blinded_payloads.get(commitment)?;
                let reveal = self.owned_blinded_reveals.get(commitment)?;
                Some(OwnedBlindedTransaction {
                    transaction: transaction.clone(),
                    payload: payload.clone(),
                    reveal: reveal.clone(),
                })
            })
            .collect()
    }

    pub fn mark_owned_blinded_outbox_persisted(&mut self, version: u64) {
        if self.owned_blinded_outbox_version == version {
            self.owned_blinded_outbox_version = 0;
        }
    }

    pub fn restore_owned_blinded_transactions(
        &mut self,
        transactions: Vec<OwnedBlindedTransaction>,
    ) -> Result<()> {
        self.owned_blinded_transactions.clear();
        self.owned_blinded_reveals.clear();
        self.owned_blinded_payloads.clear();
        for owned in transactions {
            let commitment = owned.transaction.commitment.clone();
            if owned.reveal.commitment != commitment {
                continue;
            }
            if self.ledger.has_blinded_reveal(&commitment)
                || !self.ledger.has_unrevealed_blinded_transaction(&commitment)
                    && owned.transaction.expires_at_height <= self.ledger.height().saturating_add(1)
            {
                continue;
            }
            if !self.ledger.has_blinded_transaction(&commitment) {
                match self
                    .ledger
                    .submit_blinded_transaction(owned.transaction.clone())
                {
                    Ok(true) => self.outbox.push(GossipEnvelope::BlindedTransaction(
                        owned.transaction.clone(),
                    )),
                    Ok(false) => {}
                    Err(_) => continue,
                }
            }
            if !self.ledger.has_unrevealed_blinded_transaction(&commitment) {
                continue;
            }
            self.owned_blinded_transactions
                .insert(commitment.clone(), owned.transaction);
            self.owned_blinded_payloads
                .insert(commitment.clone(), owned.payload);
            self.owned_blinded_reveals
                .insert(commitment.clone(), owned.reveal);
        }
        self.publish_owned_reveals_for_active_commits()?;
        self.bump_owned_blinded_outbox_version();
        Ok(())
    }

    pub(super) fn submit_owned_blinded_transaction(
        &mut self,
        built: BuiltBlindedTransaction,
    ) -> Result<BlindedTransaction> {
        let transaction = built.transaction;
        self.owned_blinded_transactions
            .insert(transaction.commitment.clone(), transaction.clone());
        self.owned_blinded_payloads
            .insert(transaction.commitment.clone(), built.payload);
        self.owned_blinded_reveals
            .insert(transaction.commitment.clone(), built.reveal);
        self.bump_owned_blinded_outbox_version();
        if self
            .ledger
            .submit_blinded_transaction(transaction.clone())?
        {
            self.outbox
                .push(GossipEnvelope::BlindedTransaction(transaction.clone()));
        }
        Ok(transaction)
    }

    pub(super) fn submit_transaction_as_owned_blinded(
        &mut self,
        tx: Transaction,
    ) -> Result<Transaction> {
        let built = self.ledger.build_blinded_transaction(
            self.wallet.unlocked()?,
            tx.clone(),
            self.default_blinded_transaction_expiry_height(),
        )?;
        self.submit_owned_blinded_transaction(built)?;
        Ok(tx)
    }

    pub(super) fn default_blinded_transaction_expiry_height(&self) -> u64 {
        self.ledger
            .height()
            .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
    }

    pub(super) fn publish_owned_reveals_for_block(&mut self, block: &Block) -> Result<()> {
        for transaction in &block.blinded_transactions {
            let Some(reveal) = self.owned_blinded_reveals.get(&transaction.commitment) else {
                continue;
            };
            if self.ledger.submit_blinded_reveal(reveal.clone())? {
                self.outbox
                    .push(GossipEnvelope::BlindedReveal(reveal.clone()));
            }
        }
        Ok(())
    }

    pub(super) fn publish_owned_reveals_for_active_commits(&mut self) -> Result<()> {
        let reveals = self
            .owned_blinded_reveals
            .iter()
            .filter(|(commitment, _)| self.ledger.has_active_blinded_transaction(commitment))
            .map(|(_, reveal)| reveal.clone())
            .collect::<Vec<_>>();
        for reveal in reveals {
            if self.ledger.submit_blinded_reveal(reveal.clone())? {
                self.outbox.push(GossipEnvelope::BlindedReveal(reveal));
            }
        }
        Ok(())
    }

    pub(super) fn prune_owned_blinded_payloads_for_block(&mut self, block: &Block) {
        let before = self.owned_blinded_transactions.len()
            + self.owned_blinded_reveals.len()
            + self.owned_blinded_payloads.len();
        for reveal in block.all_blinded_reveals() {
            self.owned_blinded_transactions.remove(&reveal.commitment);
            self.owned_blinded_reveals.remove(&reveal.commitment);
            self.owned_blinded_payloads.remove(&reveal.commitment);
        }
        let unrevealed = self
            .owned_blinded_transactions
            .keys()
            .filter(|commitment| self.ledger.has_unrevealed_blinded_transaction(commitment))
            .cloned()
            .collect::<Vec<_>>();
        self.owned_blinded_transactions
            .retain(|commitment, _| unrevealed.contains(commitment));
        self.owned_blinded_reveals
            .retain(|commitment, _| unrevealed.contains(commitment));
        self.owned_blinded_payloads
            .retain(|commitment, _| unrevealed.contains(commitment));
        let after = self.owned_blinded_transactions.len()
            + self.owned_blinded_reveals.len()
            + self.owned_blinded_payloads.len();
        if before != after {
            self.bump_owned_blinded_outbox_version();
        }
    }

    pub(super) fn queue_owned_blinded_payloads(&self, ledger: &mut Ledger) -> Result<()> {
        for (commitment, payload) in &self.owned_blinded_payloads {
            if self.ledger.has_unrevealed_blinded_transaction(commitment)
                && !ledger.has_transaction(payload.signature())
            {
                let _ = ledger.submit_transaction(payload.clone());
            }
        }
        Ok(())
    }

    pub(super) fn bump_owned_blinded_outbox_version(&mut self) {
        self.owned_blinded_outbox_version = self.owned_blinded_outbox_version.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{GossipEnvelope, NodeCore},
        domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet},
    };

    #[test]
    fn owned_blinded_transaction_reveals_after_commit_block_import() {
        let alice = Wallet::from_seed("owned-blinded-reveal-alice");
        let bob = Wallet::from_seed("owned-blinded-reveal-bob");
        let carol = Wallet::from_seed("owned-blinded-reveal-carol");
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
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(3, 7, wallet_node.chain_height() + 4)
            .unwrap();
        wallet_node.drain_outbox();
        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();

        wallet_node
            .receive(GossipEnvelope::Block(commit_block))
            .unwrap();
        let outbox = wallet_node.drain_outbox();

        assert!(
            wallet_node
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == blinded.commitment)
        );
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
        )));
    }

    #[test]
    fn status_wallet_balance_includes_owned_blinded_change_before_and_after_commit() {
        let alice = Wallet::from_seed("owned-blinded-balance-alice");
        let bob = Wallet::from_seed("owned-blinded-balance-bob");
        let carol = Wallet::from_seed("owned-blinded-balance-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let starting_balance = 2 * MICRO_IUNA;
        let burn_amount = MICRO_IUNA / 10;
        let fee = MICRO_IUNA / 10;
        let expected_balance = starting_balance - burn_amount - fee;
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), starting_balance);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(burn_amount, fee, wallet_node.chain_height() + 4)
            .unwrap();

        assert_eq!(wallet_node.status().wallet_balance, expected_balance);

        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();

        wallet_node
            .receive(GossipEnvelope::Block(commit_block))
            .unwrap();

        assert_eq!(wallet_node.ledger().balance_of(carol.address()), 0);
        assert_eq!(wallet_node.status().wallet_balance, expected_balance);
    }

    #[test]
    fn owned_blinded_transaction_restore_requeues_pending_commit() {
        let alice = Wallet::from_seed("owned-blinded-restore-pending-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-pending-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new(allocations, 1);
        let mut node = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);

        let blinded = node
            .blinded_transfer_with_fee(bob.address(), MICRO_IUNA, 7, node.chain_height() + 4)
            .unwrap();
        let owned = node.owned_blinded_transactions();
        let mut restarted = NodeCore::from_ledger(alice, ledger, 0);

        restarted.restore_owned_blinded_transactions(owned).unwrap();

        assert_eq!(
            restarted.ledger().pending_blinded_transactions(),
            std::slice::from_ref(&blinded)
        );
        let outbox = restarted.drain_outbox();
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedTransaction(transaction)
                if transaction.commitment == blinded.commitment
        )));
        assert!(!outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
        )));
    }

    #[test]
    fn owned_blinded_transaction_restore_skips_stale_commit_with_spent_input() {
        let alice = Wallet::from_seed("owned-blinded-restore-stale-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-stale-bob");
        let finalizers = [bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), MICRO_IUNA);
        allocations.insert(bob.address().to_string(), MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let mut wallet_node = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);

        wallet_node
            .blinded_burn_with_fee(MICRO_IUNA / 10, 7, wallet_node.chain_height() + 4)
            .unwrap();
        let owned = wallet_node.owned_blinded_transactions();
        let stale_conflict = ledger.build_burn(&alice, MICRO_IUNA / 10, 7).unwrap();
        let mut advanced_ledger = ledger;
        advanced_ledger.submit_transaction(stale_conflict).unwrap();
        let leader = advanced_ledger.expected_leader_for_next_block().unwrap();
        assert_eq!(leader, bob.address());
        let anchor_burn = advanced_ledger.build_burn(&bob, 1, 0).unwrap();
        advanced_ledger.submit_transaction(anchor_burn).unwrap();
        let block = advanced_ledger.mine_next_block(&bob, 1).unwrap();
        advanced_ledger.apply_block(block).unwrap();
        let mut restarted = NodeCore::from_ledger(alice, advanced_ledger, 0);

        restarted.restore_owned_blinded_transactions(owned).unwrap();

        assert!(restarted.owned_blinded_transactions().is_empty());
        assert!(restarted.ledger().pending_blinded_transactions().is_empty());
        assert!(restarted.drain_outbox().is_empty());
    }

    #[test]
    fn owned_blinded_transaction_restore_publishes_reveal_after_commit() {
        let alice = Wallet::from_seed("owned-blinded-restore-reveal-alice");
        let bob = Wallet::from_seed("owned-blinded-restore-reveal-bob");
        let carol = Wallet::from_seed("owned-blinded-restore-reveal-carol");
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
        let mut wallet_node = NodeCore::from_ledger(carol.clone(), ledger.clone(), 0);
        let mut finalizer_ledger = ledger;

        let blinded = wallet_node
            .blinded_burn_with_fee(3, 7, wallet_node.chain_height() + 4)
            .unwrap();
        let owned = wallet_node.owned_blinded_transactions();
        finalizer_ledger
            .submit_blinded_transaction(blinded.clone())
            .unwrap();
        let leader = finalizer_ledger.expected_leader_for_next_block().unwrap();
        let finalizer = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = finalizer_ledger.build_burn(finalizer, 1, 0).unwrap();
        finalizer_ledger.submit_transaction(burn).unwrap();
        let commit_block = finalizer_ledger.mine_next_block(finalizer, 1).unwrap();
        finalizer_ledger.apply_block(commit_block.clone()).unwrap();
        let mut restarted_ledger = NodeCore::from_ledger(
            carol,
            Ledger::from_snapshot(finalizer_ledger.snapshot()).unwrap(),
            0,
        );

        restarted_ledger
            .restore_owned_blinded_transactions(owned)
            .unwrap();

        assert!(
            restarted_ledger
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == blinded.commitment)
        );
        assert!(
            restarted_ledger
                .drain_outbox()
                .iter()
                .any(|envelope| matches!(
                    envelope,
                    GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == blinded.commitment
                ))
        );
        assert!(
            commit_block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.commitment)
        );
    }
}
