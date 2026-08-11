use crate::domain::{Block, ChainSnapshot, Transaction};

use super::{
    BLOCK_REQUEST_LIMIT, GossipEnvelope, NETWORK_ID, NodeCore, PROTOCOL_VERSION, ProtocolHello,
    TRANSACTION_BATCH_LIMIT, now_ms, types::BlockInventory,
};

impl NodeCore {
    pub fn mempool_gossip(&mut self) -> Vec<GossipEnvelope> {
        let _ = self.publish_reveal_bundle_for_next_block();
        let mut gossip = Vec::new();
        let mine_actions = self
            .ledger
            .pending()
            .iter()
            .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
            .cloned()
            .collect::<Vec<_>>();
        gossip.extend(mine_actions.chunks(TRANSACTION_BATCH_LIMIT).map(|chunk| {
            GossipEnvelope::MineActions {
                transactions: chunk.to_vec(),
            }
        }));
        gossip.extend(
            self.ledger
                .pending_blinded_transactions()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::BlindedTransactions {
                    transactions: chunk.to_vec(),
                }),
        );
        gossip.extend(
            self.ledger
                .pending_blinded_reveals()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::BlindedReveals {
                    reveals: chunk.to_vec(),
                }),
        );
        gossip.extend(
            self.usable_reveal_bundles()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::RevealBundles {
                    bundles: chunk.to_vec(),
                }),
        );
        gossip
    }

    pub fn chain_snapshot(&self) -> ChainSnapshot {
        self.ledger.snapshot()
    }

    pub fn hello(&self, listen_addr: Option<String>, node_id: Option<String>) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::Hello(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            network_id: NETWORK_ID.to_string(),
            genesis_hash: self.ledger.genesis_hash().to_string(),
            listen_addr,
            node_id,
            height: status.height,
            tip_hash: status.tip_hash,
            time_ms: now_ms(),
        })
    }

    pub fn peer_status(&self) -> GossipEnvelope {
        let status = self.ledger.status();
        GossipEnvelope::PeerStatus {
            height: status.height,
            tip_hash: status.tip_hash,
            time_ms: now_ms(),
        }
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        self.ledger.blocks_from(from_height, limit)
    }

    pub fn blocks_by_hash(&self, hashes: &[String]) -> Vec<Block> {
        hashes
            .iter()
            .filter_map(|hash| self.ledger.block_by_hash(hash))
            .collect()
    }

    pub fn missing_inventory_requests(&self, blocks: &[BlockInventory]) -> Vec<GossipEnvelope> {
        let local_height = self.ledger.height();
        let first_height_gap = blocks
            .iter()
            .filter(|block| !self.ledger.has_block(&block.hash))
            .filter(|block| block.height > local_height + 1)
            .map(|block| block.height)
            .min();
        let missing_blocks = blocks
            .iter()
            .filter(|block| !self.ledger.has_block(&block.hash))
            .filter(|block| first_height_gap.is_none_or(|gap| block.height < gap))
            .map(|block| block.hash.clone())
            .collect::<Vec<_>>();

        let mut requests = Vec::new();
        if !missing_blocks.is_empty() {
            requests.push(GossipEnvelope::BlockRequest {
                hashes: missing_blocks,
            });
        }
        if first_height_gap.is_some() {
            requests.push(GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: BLOCK_REQUEST_LIMIT,
            });
        }
        requests
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{BLOCK_REQUEST_LIMIT, BlockInventory, GossipEnvelope, NodeCore},
        domain::{Amount, GenesisBurn, Ledger, MICRO_IUNA, Transaction, Wallet},
    };

    #[test]
    fn mempool_gossip_includes_blinded_transactions() {
        let alice = Wallet::from_seed("blinded-gossip-alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new(genesis, 1);
        let blinded = ledger.build_blinded_burn(&alice, MICRO_IUNA, 7, 3).unwrap();
        let mut sender = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);
        let mut receiver = NodeCore::from_ledger(alice, ledger, 0);

        sender
            .receive_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        for envelope in sender.mempool_gossip() {
            receiver.receive(envelope).unwrap();
        }

        assert_eq!(
            receiver.ledger().pending_blinded_transactions(),
            std::slice::from_ref(&blinded.transaction)
        );
    }

    #[test]
    fn mempool_gossip_includes_public_mine_actions() {
        let alice = Wallet::from_seed("mine-gossip-alice");
        let ledger = Ledger::new(BTreeMap::new(), 1);
        let mine = ledger.build_mine(alice.address()).unwrap();
        let mut sender = NodeCore::from_ledger(alice.clone(), ledger.clone(), 0);
        let mut receiver = NodeCore::from_ledger(alice, ledger, 0);

        sender.submit_public_mine_action(mine.clone()).unwrap();
        for envelope in sender.mempool_gossip() {
            receiver.receive(envelope).unwrap();
        }

        assert_eq!(receiver.ledger().pending(), std::slice::from_ref(&mine));
        assert!(receiver.ledger().pending_blinded_transactions().is_empty());
    }

    #[test]
    fn inventory_requests_only_missing_objects() {
        let alice = Wallet::from_seed("missing-inv-alice");
        let bob = Wallet::from_seed("missing-inv-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let mut local = node("local", alice.clone(), allocations.clone());
        let mut remote = node("remote", alice.clone(), allocations);
        queue_plaintext_burn(&mut local, &alice, 1);
        let block = local.mine_one_at(1).unwrap();
        let inventory = [BlockInventory {
            height: block.height,
            hash: block.hash.clone(),
        }];

        let requests = remote.missing_inventory_requests(&inventory);
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], GossipEnvelope::BlockRequest { .. }));

        remote.receive(GossipEnvelope::Block(block)).unwrap();
        assert!(remote.missing_inventory_requests(&inventory).is_empty());
    }

    #[test]
    fn inventory_gap_requests_range_instead_of_orphan_block() {
        let alice = Wallet::from_seed("gap-inv-alice");
        let bob = Wallet::from_seed("gap-inv-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let mut local = node("local", alice.clone(), allocations.clone());
        let remote = node("remote", bob, allocations);

        let mut latest = None;
        for height in 1..=3 {
            queue_plaintext_burn(&mut local, &alice, 1);
            latest = Some(local.mine_one_at(height).unwrap());
        }
        let latest = latest.unwrap();

        let requests = remote.missing_inventory_requests(&[BlockInventory {
            height: latest.height,
            hash: latest.hash,
        }]);

        assert_eq!(requests.len(), 1);
        match &requests[0] {
            GossipEnvelope::BlockRangeRequest { from_height, limit } => {
                assert_eq!(*from_height, 1);
                assert_eq!(*limit, BLOCK_REQUEST_LIMIT);
            }
            other => panic!("expected block range request, got {other:?}"),
        }
    }

    fn node(_network_key: &str, wallet: Wallet, allocations: BTreeMap<String, Amount>) -> NodeCore {
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            25,
        )
        .unwrap();
        NodeCore::from_ledger(wallet, ledger, 0)
    }

    fn queue_plaintext_burn(node: &mut NodeCore, wallet: &Wallet, amount: Amount) -> Transaction {
        let tx = node.ledger().build_burn(wallet, amount, 0).unwrap();
        node.receive_transaction(tx.clone()).unwrap();
        tx
    }

    fn allocations(wallets: &[Wallet], amount: Amount) -> BTreeMap<String, Amount> {
        wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), amount))
            .collect()
    }
}
