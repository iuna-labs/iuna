use anyhow::Result;

use crate::domain::{
    Block, BurnBundle, ChainSnapshot, Ledger, Transaction, TransactionSubmitOutcome, TransactionV2,
    ValidationError, decode_hex, error_has_validation, hex_encode,
};

use super::{GossipEnvelope, IMPORT_REBROADCAST_LIMIT, NodeCore};

impl NodeCore {
    pub fn receive_transaction(&mut self, tx: Transaction) -> Result<TransactionSubmitOutcome> {
        let outcome = self.ledger.submit_transaction_with_outcome(tx.clone())?;
        Ok(outcome)
    }

    pub fn receive_gossiped_transaction(&mut self, tx: Transaction) -> Result<()> {
        let outcome = match self.ledger.submit_transaction_with_outcome(tx.clone()) {
            Ok(outcome) => outcome,
            Err(error)
                if matches!(&tx, Transaction::Mine { .. })
                    && error_has_validation(&error, |kind| {
                        kind == ValidationError::MineAnchorLimitReached
                    }) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if outcome.added() {
            self.outbox.push(GossipEnvelope::Transaction(tx));
        }
        Ok(())
    }

    pub fn receive_gossiped_transaction_v2(&mut self, envelope: String) -> Result<()> {
        let encoded = decode_hex(&envelope)?;
        let transaction = self.ledger.decode_transaction_v2(&encoded)?;
        let outcome = self.ledger.submit_transaction_v2(transaction)?;
        if outcome.added() {
            self.outbox.push(GossipEnvelope::TransactionV2 {
                envelope: hex_encode(encoded),
            });
        }
        Ok(())
    }

    pub(super) fn submit_public_transaction_v2(
        &mut self,
        transaction: TransactionV2,
    ) -> Result<TransactionV2> {
        let domain = self.ledger.transaction_v2_domain()?;
        let envelope = hex_encode(transaction.encode(&domain)?);
        if self
            .ledger
            .submit_transaction_v2(transaction.clone())?
            .added()
        {
            self.outbox.push(GossipEnvelope::TransactionV2 { envelope });
        }
        Ok(transaction)
    }

    pub fn receive_burn_bundle(&mut self, bundle: BurnBundle) -> Result<()> {
        let next_height = self.ledger.height().saturating_add(1);
        if bundle.height <= self.ledger.height() {
            return Ok(());
        }
        if bundle.height > next_height {
            return Ok(());
        }
        let key = (bundle.height, bundle.slot, bundle.member.clone());
        self.ledger.precheck_next_block_burn_bundle(&bundle)?;
        for burn in &bundle.burns {
            self.receive_gossiped_transaction(burn.clone())?;
        }
        for envelope in &bundle.burns_v2 {
            self.receive_gossiped_transaction_v2(envelope.clone())?;
        }
        self.ledger
            .validate_next_block_burn_bundles(vec![bundle.clone()])?;
        if self.equivocated_burn_bundle_slots.contains(&key) {
            return Ok(());
        }
        if let Some(existing) = self.burn_bundles.get(&key) {
            if existing.canonical() != bundle.canonical() {
                self.burn_bundles.remove(&key);
                self.equivocated_burn_bundle_slots.insert(key);
            }
            return Ok(());
        }
        self.burn_bundles.insert(key, bundle.clone());
        self.outbox.push(GossipEnvelope::BurnBundle(bundle));
        Ok(())
    }

    pub fn receive(&mut self, envelope: GossipEnvelope) -> Result<()> {
        match envelope {
            GossipEnvelope::Hello(_)
            | GossipEnvelope::PeerStatus { .. }
            | GossipEnvelope::ChainBootstrapRequest
            | GossipEnvelope::ChainBootstrap(_)
            | GossipEnvelope::BlockLocatorRequest { .. }
            | GossipEnvelope::BlockRangeRequest { .. }
            | GossipEnvelope::BlockRequest { .. }
            | GossipEnvelope::Inventory { .. } => Ok(()),
            GossipEnvelope::Transaction(tx) => self.receive_gossiped_transaction(tx),
            GossipEnvelope::Transactions { transactions } => {
                for tx in transactions {
                    self.receive_gossiped_transaction(tx)?;
                }
                Ok(())
            }
            GossipEnvelope::TransactionV2 { envelope } => {
                self.receive_gossiped_transaction_v2(envelope)
            }
            GossipEnvelope::TransactionsV2 { envelopes } => {
                for envelope in envelopes {
                    self.receive_gossiped_transaction_v2(envelope)?;
                }
                Ok(())
            }
            GossipEnvelope::BurnBundle(bundle) => self.receive_burn_bundle(bundle),
            GossipEnvelope::BurnBundles { bundles } => {
                for bundle in bundles {
                    self.receive_burn_bundle(bundle)?;
                }
                Ok(())
            }
            GossipEnvelope::BurnBundleRequest { .. } => Ok(()),
            GossipEnvelope::Block(block) => {
                let previous_height = self.ledger.height();
                self.ledger.apply_block(block.clone())?;
                if self.ledger.height() > previous_height {
                    self.clear_stale_local_block_anchor();
                    self.clear_stale_burn_bundle_collection();
                    self.prune_burn_bundles();
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
                        self.clear_stale_burn_bundle_collection();
                        self.prune_burn_bundles();
                        imported.push(block);
                    }
                }
                for block in imported {
                    self.outbox.push(GossipEnvelope::Block(block));
                }
                Ok(())
            }
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
            self.clear_stale_burn_bundle_collection();
            self.prune_burn_bundles();
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
        if self.network_migration_from().is_some() {
            anyhow::bail!("local chain reset is required before joining the new network");
        }
        let previous_height = self.ledger.height();
        let imported = self.ledger.extend_from_snapshot(snapshot)?;
        if imported {
            self.reset_automatic_mining_progress();
            self.clear_stale_local_block_anchor();
            self.clear_stale_burn_bundle_collection();
            self.prune_burn_bundles();
            self.enqueue_imported_blocks(previous_height)?;
        }
        Ok(())
    }

    pub(crate) fn import_verified_ledger(&mut self, ledger: Ledger) -> Result<bool> {
        if self.network_migration_from().is_some() {
            anyhow::bail!("local chain reset is required before joining the new network");
        }
        let replaces_setup_placeholder = self.ledger.is_setup_placeholder()
            && ledger.genesis_hash() != self.ledger.genesis_hash();
        if ledger.genesis_hash() != self.ledger.genesis_hash() && !replaces_setup_placeholder {
            anyhow::bail!("chain snapshot genesis does not match local chain");
        }
        let previous_height = self.ledger.height();
        if !replaces_setup_placeholder {
            if ledger.height() < previous_height {
                return Ok(false);
            }
            if ledger.height() == previous_height && ledger.tip_hash() == self.ledger.tip_hash() {
                return Ok(false);
            }
        }

        self.ledger = ledger;
        self.reset_automatic_mining_progress();
        self.clear_stale_local_block_anchor();
        self.clear_stale_burn_bundle_collection();
        self.prune_burn_bundles();
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
        for _ in &blocks {
            self.prune_burn_bundles();
            self.clear_stale_burn_bundle_collection();
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
        app::NodeCore,
        domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet},
    };

    fn funded_ledger(wallets: &[Wallet]) -> Ledger {
        let allocations = wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), 10 * MICRO_IUNA))
            .collect::<BTreeMap<_, _>>();
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect::<Vec<_>>();
        Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap()
    }

    #[test]
    fn verified_same_height_fork_replaces_local_ledger() {
        let wallet = Wallet::from_seed("verified-same-height-fork");
        let parent = funded_ledger(std::slice::from_ref(&wallet));
        let mut local = parent.clone();
        let burn = local.build_burn(&wallet, 1, 1).unwrap();
        local.submit_transaction(burn.clone()).unwrap();
        let local_block = local.mine_next_block(&wallet, 1).unwrap();
        local.apply_locally_mined_block(local_block).unwrap();

        let mut remote = parent;
        remote.submit_transaction(burn).unwrap();
        let remote_block = remote.mine_next_block(&wallet, 2).unwrap();
        remote
            .apply_locally_mined_block(remote_block.clone())
            .unwrap();
        assert_eq!(local.height(), remote.height());
        assert_ne!(local.tip_hash(), remote.tip_hash());

        let mut node = NodeCore::from_ledger(wallet, local, 0);
        assert!(node.import_verified_ledger(remote).unwrap());
        assert_eq!(node.ledger().tip_hash(), remote_block.hash);
    }

    #[test]
    fn gossiped_mine_over_anchor_limit_is_silently_ignored() {
        let wallets = (0..4)
            .map(|index| {
                let seed = format!("gossip-mine-limit-{index}");
                Wallet::from_seed(&seed)
            })
            .collect::<Vec<_>>();
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mine_actions = wallets
            .iter()
            .map(|wallet| ledger.build_mine(wallet.address()).unwrap())
            .collect::<Vec<_>>();
        let mut node = NodeCore::from_ledger(wallets[0].clone(), ledger, 0);

        node.receive_gossiped_transaction(mine_actions[0].clone())
            .unwrap();
        node.receive_gossiped_transaction(mine_actions[1].clone())
            .unwrap();
        node.receive_gossiped_transaction(mine_actions[2].clone())
            .unwrap();

        assert_eq!(node.ledger().pending().len(), 2);
        assert!(
            node.ledger()
                .pending()
                .iter()
                .all(|transaction| transaction.signature() != mine_actions[2].signature())
        );
        let error = node
            .receive_transaction(mine_actions[3].clone())
            .unwrap_err();
        assert_eq!(error.to_string(), "mine transaction anchor limit reached");
    }

    #[test]
    fn burn_bundle_imports_new_signed_burns_to_mempool() {
        let alice = Wallet::from_seed("bundle-import-new-burn-alice");
        let bob = Wallet::from_seed("bundle-import-new-burn-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = ledger.expected_leader_for_next_block().unwrap();
        let signer = wallets
            .iter()
            .find(|wallet| wallet.address() == finalizer)
            .expect("test ledger should include selected finalizer")
            .clone();
        let burner = wallets
            .iter()
            .find(|wallet| wallet.address() != signer.address())
            .expect("test ledger should include a non-finalizer")
            .clone();
        let burn = ledger.build_burn(&burner, 1, 1).unwrap();
        let mut signer_ledger = ledger.clone();
        signer_ledger.submit_transaction(burn.clone()).unwrap();
        let bundle = signer_ledger.build_burn_bundle(&signer).unwrap().unwrap();
        let mut receiver = NodeCore::from_ledger(signer, ledger, 0);

        receiver.receive_burn_bundle(bundle).unwrap();

        assert!(
            receiver
                .ledger()
                .pending()
                .iter()
                .any(|transaction| transaction.signature() == burn.signature())
        );
    }

    #[test]
    fn oversized_burn_bundle_does_not_import_embedded_burns() {
        let alice = Wallet::from_seed("oversized-bundle-alice");
        let bob = Wallet::from_seed("oversized-bundle-bob");
        let wallets = [alice.clone(), bob.clone()];
        let ledger = funded_ledger(&wallets);
        let finalizer = ledger.expected_leader_for_next_block().unwrap();
        let signer = wallets
            .iter()
            .find(|wallet| wallet.address() == finalizer)
            .expect("test ledger should include selected finalizer")
            .clone();
        let burner = wallets
            .iter()
            .find(|wallet| wallet.address() != signer.address())
            .expect("test ledger should include a non-finalizer")
            .clone();
        let mut signer_ledger = ledger.clone();
        let mut burns = Vec::new();
        let oversized_bundle = loop {
            let burn = signer_ledger.build_burn(&burner, 1, 1).unwrap();
            signer_ledger.submit_transaction(burn.clone()).unwrap();
            burns.push(burn);
            let bundle = signer_ledger.test_burn_bundle(&signer, burns.clone());
            if bundle.serialized_size_bytes().unwrap() > 10_000 {
                break bundle;
            }
        };
        let first_burn_signature = oversized_bundle.burns[0].signature().to_string();
        let mut receiver = NodeCore::from_ledger(signer, ledger, 0);

        let error = receiver.receive_burn_bundle(oversized_bundle).unwrap_err();

        assert!(
            error.to_string().contains("burn bundle exceeds max size"),
            "{error:#}"
        );
        assert!(
            receiver
                .ledger()
                .pending()
                .iter()
                .all(|transaction| transaction.signature() != first_burn_signature)
        );
        assert!(receiver.drain_outbox().is_empty());
    }
}
