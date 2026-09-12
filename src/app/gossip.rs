use crate::domain::{Block, ChainSnapshot};

use super::{
    BLOCK_REQUEST_LIMIT, ChainBootstrap, GossipEnvelope, NETWORK_ID, NodeCore, PROTOCOL_VERSION,
    ProtocolHello, TRANSACTION_BATCH_LIMIT, now_ms, protocol_capabilities, types::BlockInventory,
};

impl NodeCore {
    pub fn mempool_gossip(&mut self) -> Vec<GossipEnvelope> {
        let mut gossip = Vec::new();
        gossip.extend(
            self.ledger
                .pending()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::Transactions {
                    transactions: chunk.to_vec(),
                }),
        );
        gossip.extend(
            self.usable_burn_bundles()
                .chunks(TRANSACTION_BATCH_LIMIT)
                .map(|chunk| GossipEnvelope::BurnBundles {
                    bundles: chunk.to_vec(),
                }),
        );
        gossip
    }

    pub fn chain_snapshot(&self) -> ChainSnapshot {
        self.ledger.snapshot()
    }

    pub fn chain_bootstrap(&self) -> ChainBootstrap {
        let snapshot = self.ledger.genesis_snapshot();
        ChainBootstrap {
            genesis_allocations: snapshot.genesis_allocations,
            vdf_rounds: snapshot.vdf_rounds,
            launch_profile: snapshot.launch_profile,
            genesis_block: snapshot.blocks[0].clone(),
            height: self.ledger.height(),
            tip_hash: self.ledger.tip_hash().to_string(),
        }
    }

    pub fn block_locator(&self) -> Vec<String> {
        self.ledger.block_locator()
    }

    pub fn blocks_after_locator(&self, locator: &[String], limit: usize) -> Vec<Block> {
        self.ledger.blocks_after_locator(locator, limit)
    }

    pub fn hello(&self, listen_addr: Option<String>, node_id: Option<String>) -> GossipEnvelope {
        GossipEnvelope::Hello(ProtocolHello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: protocol_capabilities(),
            network_id: NETWORK_ID.to_string(),
            genesis_hash: self.ledger.genesis_hash().to_string(),
            listen_addr,
            node_id,
            height: self.ledger.height(),
            tip_hash: self.ledger.tip_hash().to_string(),
            time_ms: now_ms(),
        })
    }

    pub fn peer_status(&self) -> GossipEnvelope {
        GossipEnvelope::PeerStatus {
            height: self.ledger.height(),
            tip_hash: self.ledger.tip_hash().to_string(),
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

    pub fn missing_inventory_request(&self, blocks: &[BlockInventory]) -> Option<GossipEnvelope> {
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

        if first_height_gap.is_some() {
            return Some(GossipEnvelope::BlockRangeRequest {
                from_height: local_height + 1,
                limit: BLOCK_REQUEST_LIMIT,
            });
        }

        (!missing_blocks.is_empty()).then_some(GossipEnvelope::BlockRequest {
            hashes: missing_blocks,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        app::{
            BLOCK_REQUEST_LIMIT, BlockInventory, GossipEnvelope, NodeCore, protocol_capabilities,
        },
        domain::{Amount, GenesisBurn, Ledger, MICRO_IUNA, Transaction, Wallet},
    };

    #[test]
    fn hello_advertises_current_protocol_capabilities() {
        let wallet = Wallet::from_seed("hello-capabilities");
        let node = NodeCore::from_ledger(wallet, Ledger::new(BTreeMap::new(), 1), 0);

        let GossipEnvelope::Hello(hello) = node.hello(None, None) else {
            panic!("hello builder returned another envelope type");
        };

        assert_eq!(hello.capabilities, protocol_capabilities());
    }

    #[test]
    fn mempool_gossip_rebroadcasts_public_burns() {
        let wallet = Wallet::from_seed("mempool-gossip-public-burn");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut node = NodeCore::from_ledger(wallet, ledger, 0);
        let burn = node.burn_with_fee(MICRO_IUNA / 10, 1).unwrap();
        node.drain_outbox();

        let gossip = node.mempool_gossip();

        assert!(gossip.iter().any(|envelope| {
            match envelope {
                GossipEnvelope::Transactions { transactions } => transactions
                    .iter()
                    .any(|tx| tx.signature() == burn.signature()),
                _ => false,
            }
        }));
    }

    #[test]
    fn mempool_gossip_rebroadcasts_public_transfers() {
        let alice = Wallet::from_seed("mempool-gossip-transfer-alice");
        let bob = Wallet::from_seed("mempool-gossip-transfer-bob");
        let ledger = Ledger::new(
            BTreeMap::from([(alice.address().to_string(), 10 * MICRO_IUNA)]),
            1,
        );
        let mut node = NodeCore::from_ledger(alice, ledger, 0);
        let transfer = node
            .transfer_with_fee(bob.address(), MICRO_IUNA, 1)
            .unwrap();
        node.drain_outbox();

        assert!(node.mempool_gossip().iter().any(|envelope| {
            match envelope {
                GossipEnvelope::Transactions { transactions } => transactions
                    .iter()
                    .any(|transaction| transaction.signature() == transfer.signature()),
                _ => false,
            }
        }));
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
    }

    #[test]
    fn inventory_requests_only_missing_objects() {
        let alice = Wallet::from_seed("missing-inv-alice");
        let bob = Wallet::from_seed("missing-inv-bob");
        let allocations = allocations(&[alice.clone(), bob], 1_000);
        let mut local = node(alice.clone(), allocations.clone());
        let mut remote = node(alice.clone(), allocations);
        queue_plaintext_burn(&mut local, &alice, 1);
        let block = local.mine_one_at(1).unwrap();
        let inventory = [BlockInventory {
            height: block.height,
            hash: block.hash.clone(),
        }];

        let request = remote.missing_inventory_request(&inventory);
        assert!(matches!(request, Some(GossipEnvelope::BlockRequest { .. })));

        remote.receive(GossipEnvelope::Block(block)).unwrap();
        assert!(remote.missing_inventory_request(&inventory).is_none());
    }

    #[test]
    fn inventory_gap_requests_range_instead_of_orphan_block() {
        let alice = Wallet::from_seed("gap-inv-alice");
        let bob = Wallet::from_seed("gap-inv-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let mut local = node(alice.clone(), allocations.clone());
        let remote = node(bob, allocations);

        let mut latest = None;
        for height in 1..=3 {
            queue_plaintext_burn(&mut local, &alice, 1);
            latest = Some(local.mine_one_at(height).unwrap());
        }
        let latest = latest.unwrap();

        let request = remote.missing_inventory_request(&[BlockInventory {
            height: latest.height,
            hash: latest.hash,
        }]);

        match request {
            Some(GossipEnvelope::BlockRangeRequest { from_height, limit }) => {
                assert_eq!(from_height, 1);
                assert_eq!(limit, BLOCK_REQUEST_LIMIT);
            }
            other => panic!("expected block range request, got {other:?}"),
        }
    }

    #[test]
    fn multi_block_inventory_starts_exactly_one_range_request() {
        let alice = Wallet::from_seed("multi-inventory-alice");
        let bob = Wallet::from_seed("multi-inventory-bob");
        let allocations = allocations(&[alice.clone(), bob.clone()], 1_000);
        let mut source = node(alice.clone(), allocations.clone());
        let receiver = node(bob, allocations);
        for timestamp_ms in [1, 2] {
            queue_plaintext_burn(&mut source, &alice, 1);
            source.mine_one_at(timestamp_ms).unwrap();
        }
        let inventory = source
            .ledger()
            .blocks_from(1, 2)
            .into_iter()
            .map(|block| BlockInventory {
                height: block.height,
                hash: block.hash,
            })
            .collect::<Vec<_>>();

        let request = receiver.missing_inventory_request(&inventory);

        assert!(matches!(
            request,
            Some(GossipEnvelope::BlockRangeRequest {
                from_height: 1,
                limit: BLOCK_REQUEST_LIMIT
            })
        ));
    }

    fn node(wallet: Wallet, allocations: BTreeMap<String, Amount>) -> NodeCore {
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            25,
        )
        .unwrap();
        NodeCore::from_ledger(wallet, ledger, 0)
    }

    fn queue_plaintext_burn(node: &mut NodeCore, wallet: &Wallet, amount: Amount) -> Transaction {
        let tx = node.ledger().build_burn(wallet, amount, 1).unwrap();
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
