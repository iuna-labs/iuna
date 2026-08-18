use anyhow::{Result, bail};

use crate::domain::{
    BlindedReveal, BlindedTransaction, Block, ChainSnapshot, Ledger, RevealBundle, Transaction,
    TransactionSubmitOutcome,
};

use super::{
    GossipEnvelope, IMPORT_REBROADCAST_LIMIT, NodeCore, helpers::transaction_input_outpoints,
};

impl NodeCore {
    pub fn receive_transaction(&mut self, tx: Transaction) -> Result<TransactionSubmitOutcome> {
        let outcome = self.ledger.submit_transaction_with_outcome(tx.clone())?;
        Ok(outcome)
    }

    pub fn receive_mine_action(&mut self, tx: Transaction) -> Result<()> {
        if !matches!(tx, Transaction::Mine { .. }) {
            bail!("only mine actions may be gossiped as plaintext");
        }
        if self
            .ledger
            .submit_transaction_with_outcome(tx.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::MineAction(tx));
        }
        Ok(())
    }

    pub fn receive_blinded_transaction(&mut self, tx: BlindedTransaction) -> Result<()> {
        if self.blinded_transaction_conflicts_with_local_anchor(&tx) {
            return Ok(());
        }
        if self.ledger.submit_blinded_transaction(tx.clone())? {
            self.outbox.push(GossipEnvelope::BlindedTransaction(tx));
        }
        Ok(())
    }

    fn blinded_transaction_conflicts_with_local_anchor(&self, tx: &BlindedTransaction) -> bool {
        let Some((height, burn)) = &self.local_block_anchor_burn else {
            return false;
        };
        if *height != self.ledger.height() || self.ledger.has_transaction(burn.signature()) {
            return false;
        }
        let anchor_inputs = transaction_input_outpoints(burn);
        tx.inputs
            .iter()
            .any(|input| anchor_inputs.contains(&input.outpoint))
    }

    pub fn receive_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<()> {
        self.receive_blinded_reveal_without_bundle_publish(reveal)?;
        Ok(())
    }

    fn receive_blinded_reveal_without_bundle_publish(
        &mut self,
        reveal: BlindedReveal,
    ) -> Result<bool> {
        if self.ledger.submit_blinded_reveal(reveal.clone())? {
            self.outbox.push(GossipEnvelope::BlindedReveal(reveal));
            return Ok(true);
        }
        Ok(false)
    }

    pub fn receive_reveal_bundle(&mut self, bundle: RevealBundle) -> Result<()> {
        let next_height = self.ledger.height().saturating_add(1);
        if bundle.height <= self.ledger.height() {
            return Ok(());
        }
        if bundle.height > next_height {
            return Ok(());
        }
        let key = (bundle.height, bundle.slot);
        self.ledger
            .validate_next_block_reveal_bundles(vec![bundle.clone()])?;
        if self.equivocated_reveal_bundle_slots.contains(&key) {
            return Ok(());
        }
        if let Some(existing) = self.reveal_bundles.get(&key) {
            if existing.canonical() != bundle.canonical() {
                self.reveal_bundles.remove(&key);
                self.equivocated_reveal_bundle_slots.insert(key);
            }
            return Ok(());
        }
        self.reveal_bundles.insert(key, bundle.clone());
        self.outbox.push(GossipEnvelope::RevealBundle(bundle));
        Ok(())
    }

    pub fn receive(&mut self, envelope: GossipEnvelope) -> Result<()> {
        match envelope {
            GossipEnvelope::Hello(_)
            | GossipEnvelope::PeerStatus { .. }
            | GossipEnvelope::ChainSnapshotRequest
            | GossipEnvelope::BlockRangeRequest { .. }
            | GossipEnvelope::BlockRequest { .. }
            | GossipEnvelope::Inventory { .. } => Ok(()),
            GossipEnvelope::BlindedTransaction(tx) => self.receive_blinded_transaction(tx),
            GossipEnvelope::BlindedTransactions { transactions } => {
                for tx in transactions {
                    self.receive_blinded_transaction(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::MineAction(tx) => self.receive_mine_action(tx),
            GossipEnvelope::MineActions { transactions } => {
                for tx in transactions {
                    self.receive_mine_action(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::BlindedReveal(reveal) => self.receive_blinded_reveal(reveal),
            GossipEnvelope::BlindedReveals { reveals } => {
                for reveal in reveals {
                    self.receive_blinded_reveal_without_bundle_publish(reveal)?;
                }
                Ok(())
            }
            GossipEnvelope::RevealBundle(bundle) => self.receive_reveal_bundle(bundle),
            GossipEnvelope::RevealBundles { bundles } => {
                for bundle in bundles {
                    self.receive_reveal_bundle(bundle)?;
                }
                Ok(())
            }
            GossipEnvelope::Block(block) => {
                let previous_height = self.ledger.height();
                self.ledger.apply_block(block.clone())?;
                if self.ledger.height() > previous_height {
                    self.clear_stale_local_block_anchor();
                    self.clear_stale_reveal_bundle_collection();
                    self.prune_reveal_bundles();
                    self.prune_owned_blinded_payloads_for_block(&block);
                    self.publish_owned_reveals_for_block(&block)?;
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
            GossipEnvelope::Blocks { blocks } => {
                let mut imported = Vec::new();
                for block in blocks {
                    let previous_height = self.ledger.height();
                    self.ledger.apply_block(block.clone())?;
                    if self.ledger.height() > previous_height {
                        self.clear_stale_local_block_anchor();
                        self.clear_stale_reveal_bundle_collection();
                        self.prune_reveal_bundles();
                        self.prune_owned_blinded_payloads_for_block(&block);
                        self.publish_owned_reveals_for_block(&block)?;
                        imported.push(block);
                    }
                }
                for block in imported {
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
            GossipEnvelope::ChainSnapshot(snapshot) => self.import_chain_snapshot(snapshot),
            GossipEnvelope::PeerAnnouncement { .. }
            | GossipEnvelope::PeerVerificationChallenge { .. }
            | GossipEnvelope::PeerVerificationResponse { .. }
            | GossipEnvelope::PeerList { .. } => Ok(()),
        }
    }

    pub(crate) fn receive_preverified_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        let previous_height = self.ledger.height();
        self.ledger
            .apply_preverified_block_at(block.clone(), now_ms)?;
        if self.ledger.height() > previous_height {
            self.clear_stale_local_block_anchor();
            self.clear_stale_reveal_bundle_collection();
            self.prune_reveal_bundles();
            self.prune_owned_blinded_payloads_for_block(&block);
            self.publish_owned_reveals_for_block(&block)?;
            self.outbox.push(GossipEnvelope::Block(block));
        }
        Ok(())
    }

    pub(crate) fn block_requires_vdf_verification_at(
        &self,
        block: &Block,
        now_ms: u64,
    ) -> Result<bool> {
        self.ledger
            .block_requires_vdf_verification_at(block, now_ms)
    }

    pub fn import_chain_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<()> {
        let previous_height = self.ledger.height();
        let imported = self.ledger.extend_from_snapshot(snapshot)?;
        if imported {
            self.reset_automatic_mining_progress();
            self.clear_stale_local_block_anchor();
            self.clear_stale_reveal_bundle_collection();
            self.prune_reveal_bundles();
            self.enqueue_imported_blocks(previous_height)?;
        }
        Ok(())
    }

    pub(crate) fn import_verified_ledger(&mut self, ledger: Ledger) -> Result<bool> {
        let replaces_setup_placeholder = self.ledger.is_setup_placeholder()
            && ledger.genesis_hash() != self.ledger.genesis_hash();
        if ledger.genesis_hash() != self.ledger.genesis_hash() && !replaces_setup_placeholder {
            anyhow::bail!("chain snapshot genesis does not match local chain");
        }
        let previous_height = self.ledger.height();
        if !replaces_setup_placeholder && ledger.height() <= previous_height {
            return Ok(false);
        }

        self.ledger = ledger;
        self.reset_automatic_mining_progress();
        self.clear_stale_local_block_anchor();
        self.clear_stale_reveal_bundle_collection();
        self.prune_reveal_bundles();
        self.enqueue_imported_blocks(previous_height)?;
        Ok(true)
    }

    pub fn drain_outbox(&mut self) -> Vec<GossipEnvelope> {
        std::mem::take(&mut self.outbox)
    }

    fn enqueue_imported_blocks(&mut self, previous_height: u64) -> Result<()> {
        if self.ledger.height() <= previous_height {
            return Ok(());
        }
        let blocks = self
            .ledger
            .blocks_from(previous_height + 1, IMPORT_REBROADCAST_LIMIT);
        for block in &blocks {
            self.prune_reveal_bundles();
            self.clear_stale_reveal_bundle_collection();
            self.prune_owned_blinded_payloads_for_block(block);
            self.publish_owned_reveals_for_block(block)?;
        }
        if !blocks.is_empty() {
            self.outbox.push(GossipEnvelope::Blocks { blocks });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{
            GossipEnvelope, NodeCore, REVEAL_BUNDLE_COLLECTION_MS,
            helpers::transaction_input_outpoints,
        },
        domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet},
    };

    fn wallet_for_address<'a>(wallets: &'a [Wallet], address: &str) -> &'a Wallet {
        wallets
            .iter()
            .find(|wallet| wallet.address() == address)
            .unwrap_or_else(|| panic!("missing wallet for address {address}"))
    }

    #[test]
    fn receiving_blinded_reveal_batch_waits_before_signing_committee_bundle() {
        let alice = Wallet::from_seed("immediate-bundle-alice");
        let bob = Wallet::from_seed("immediate-bundle-bob");
        let carol = Wallet::from_seed("immediate-bundle-carol");
        let dave = Wallet::from_seed("immediate-bundle-dave");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(dave.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let first = ledger
            .build_blinded_burn(&carol, 3, 100, ledger.height() + 4)
            .unwrap();
        let second = ledger
            .build_blinded_burn(&dave, 4, 100, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(first.transaction.clone())
            .unwrap();
        ledger
            .submit_blinded_transaction(second.transaction.clone())
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = wallet_for_address(&finalizers, &leader);
        let burn = ledger.build_burn(leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let commit_block = ledger.mine_next_block(leader_wallet, 1).unwrap();
        ledger.apply_locally_mined_block(commit_block).unwrap();

        let committee = ledger.reveal_committee_for_next_block();
        let committee_wallet = committee
            .iter()
            .filter_map(|member| {
                finalizers
                    .iter()
                    .find(|wallet| wallet.address() == member.owner)
            })
            .next()
            .expect("test finalizer should be in reveal committee");
        let mut committee_node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            committee_wallet.clone(),
            ledger,
            true,
            0,
            0,
        );

        committee_node
            .receive(GossipEnvelope::BlindedReveals {
                reveals: vec![first.reveal.clone()],
            })
            .unwrap();
        let outbox = committee_node.drain_outbox();

        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == first.reveal.commitment
        )));
        assert!(
            !outbox
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::RevealBundle(_)))
        );

        let early = committee_node.prepare_automatic_finalization(2);
        assert!(early.work.is_none());
        assert!(
            early
                .skipped_reason
                .as_deref()
                .unwrap_or_default()
                .contains("collecting blinded reveals")
        );
        assert!(
            !committee_node
                .drain_outbox()
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::RevealBundle(_)))
        );

        committee_node
            .receive(GossipEnvelope::BlindedReveals {
                reveals: vec![second.reveal.clone()],
            })
            .unwrap();
        let outbox = committee_node.drain_outbox();
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::BlindedReveal(reveal) if reveal.commitment == second.reveal.commitment
        )));
        assert!(
            !outbox
                .iter()
                .any(|envelope| matches!(envelope, GossipEnvelope::RevealBundle(_)))
        );

        let ready = committee_node.prepare_automatic_finalization(REVEAL_BUNDLE_COLLECTION_MS + 3);
        let _ = ready;
        let outbox = committee_node.drain_outbox();
        assert!(outbox.iter().any(|envelope| matches!(
            envelope,
            GossipEnvelope::RevealBundle(bundle)
                if bundle.member == committee_wallet.address()
                    && bundle.reveals.len() == 2
                    && bundle.reveals.iter().any(|reveal| reveal.commitment == first.reveal.commitment)
                    && bundle.reveals.iter().any(|reveal| reveal.commitment == second.reveal.commitment)
        )));
    }

    #[test]
    fn invalid_conflicting_reveal_bundle_does_not_poison_stored_slot() {
        const TEST_SIGNATURE_BYTES: usize = 64;

        let alice = Wallet::from_seed("invalid-conflict-bundle-alice");
        let bob = Wallet::from_seed("invalid-conflict-bundle-bob");
        let carol = Wallet::from_seed("invalid-conflict-bundle-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(carol.address().to_string(), 10 * MICRO_IUNA);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap();
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 100, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = wallet_for_address(&finalizers, &leader);
        let burn = ledger.build_burn(leader_wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let commit_block = ledger.mine_next_block(leader_wallet, 1).unwrap();
        ledger.apply_locally_mined_block(commit_block).unwrap();
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();

        let committee_member = ledger.reveal_committee_for_next_block()[0].clone();
        let committee_wallet = wallet_for_address(&finalizers, &committee_member.owner);
        let valid_bundle = ledger
            .build_reveal_bundle(committee_wallet)
            .unwrap()
            .unwrap();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            committee_wallet.clone(),
            ledger,
            true,
            0,
            0,
        );

        node.receive_reveal_bundle(valid_bundle.clone()).unwrap();
        node.drain_outbox();

        let mut invalid_conflict = valid_bundle.clone();
        invalid_conflict.signature = "00".repeat(TEST_SIGNATURE_BYTES);
        let error = node.receive_reveal_bundle(invalid_conflict).unwrap_err();

        assert!(format!("{error:#}").contains("reveal bundle signature is invalid"));
        let key = (valid_bundle.height, valid_bundle.slot);
        assert_eq!(node.reveal_bundles.get(&key), Some(&valid_bundle));
        assert!(!node.equivocated_reveal_bundle_slots.contains(&key));
        assert_eq!(node.usable_reveal_bundles(), vec![valid_bundle]);
        assert!(node.drain_outbox().is_empty());
    }

    #[test]
    fn inbound_blinded_transaction_conflicting_with_local_anchor_is_not_queued() {
        let alice = Wallet::from_seed("local-anchor-inbound-alice");
        let bob = Wallet::from_seed("local-anchor-inbound-bob");
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
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = finalizers
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap()
            .clone();
        let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
            leader_wallet.clone(),
            ledger,
            true,
            MICRO_IUNA / 10,
            1,
        );

        let plan = node.prepare_automatic_finalization(1);
        assert!(plan.burned.is_some());
        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        let automatic_burn_commitment = node.ledger().pending_blinded_transactions()[0]
            .commitment
            .clone();
        assert_eq!(
            node.ledger().pending_blinded_transactions()[0].commitment,
            automatic_burn_commitment
        );
        node.drain_outbox();
        let (_, anchor_burn) = node
            .local_block_anchor_burn
            .clone()
            .expect("leader burn should be held as a local block anchor");
        let anchor_inputs = transaction_input_outpoints(&anchor_burn)
            .into_iter()
            .collect::<Vec<_>>();
        let conflicting_payload = node
            .ledger()
            .build_transfer_with_inputs(&leader_wallet, bob.address(), 1, 0, &anchor_inputs)
            .unwrap();
        let conflicting = node
            .ledger()
            .build_blinded_transaction(&leader_wallet, conflicting_payload, node.chain_height() + 4)
            .unwrap();

        node.receive_blinded_transaction(conflicting.transaction)
            .unwrap();

        assert_eq!(node.ledger().pending_blinded_transactions().len(), 1);
        assert_eq!(
            node.ledger().pending_blinded_transactions()[0].commitment,
            automatic_burn_commitment
        );
        assert!(node.drain_outbox().is_empty());
        assert!(node.prepare_automatic_finalization(1).work.is_some());
    }
}
