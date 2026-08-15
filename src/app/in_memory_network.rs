use std::collections::BTreeMap;

use anyhow::Result;

use super::{GossipEnvelope, NodeCore};

#[derive(Debug, Default)]
pub struct InMemoryNetwork {
    nodes: BTreeMap<String, NodeCore>,
}

impl InMemoryNetwork {
    pub fn insert(&mut self, id: impl Into<String>, node: NodeCore) {
        self.nodes.insert(id.into(), node);
    }

    pub fn node(&self, id: &str) -> Option<&NodeCore> {
        self.nodes.get(id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut NodeCore> {
        self.nodes.get_mut(id)
    }

    pub fn deliver_until_idle(&mut self) -> Result<()> {
        loop {
            let mut outbound = Vec::new();
            for (id, node) in &mut self.nodes {
                for envelope in node.drain_outbox() {
                    outbound.push((id.clone(), envelope));
                }
            }

            if outbound.is_empty() {
                return Ok(());
            }

            for (from, envelope) in outbound {
                for (id, node) in &mut self.nodes {
                    if *id != from {
                        receive_in_memory_envelope(node, envelope.clone())?;
                    }
                }
            }
        }
    }

    pub fn gossip_mempools_once(&mut self) -> Result<()> {
        let mut outbound = Vec::new();
        for (id, node) in &mut self.nodes {
            for envelope in node.mempool_gossip() {
                outbound.push((id.clone(), envelope));
            }
        }

        for (from, envelope) in outbound {
            for (id, node) in &mut self.nodes {
                if *id != from {
                    receive_in_memory_envelope(node, envelope.clone())?;
                }
            }
        }
        Ok(())
    }

    pub fn sync_node_from_peer(&mut self, from: &str, to: &str, limit: usize) -> Result<bool> {
        let from_height = self
            .nodes
            .get(to)
            .map(|node| node.chain_height() + 1)
            .ok_or_else(|| anyhow::anyhow!("missing sync target node {to}"))?;
        let blocks = self
            .nodes
            .get(from)
            .map(|node| node.blocks_from(from_height, limit))
            .ok_or_else(|| anyhow::anyhow!("missing sync source node {from}"))?;
        if blocks.is_empty() {
            return Ok(false);
        }

        self.nodes
            .get_mut(to)
            .expect("sync target exists")
            .receive(GossipEnvelope::Blocks { blocks })?;
        Ok(true)
    }
}

fn receive_in_memory_envelope(node: &mut NodeCore, envelope: GossipEnvelope) -> Result<()> {
    let transaction_like = matches!(
        envelope,
        GossipEnvelope::BlindedTransaction(_)
            | GossipEnvelope::BlindedTransactions { .. }
            | GossipEnvelope::MineAction(_)
            | GossipEnvelope::MineActions { .. }
            | GossipEnvelope::BlindedReveal(_)
            | GossipEnvelope::BlindedReveals { .. }
    );
    match node.receive(envelope) {
        Ok(()) => Ok(()),
        Err(_) if transaction_like => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use crate::{
        app::{GossipEnvelope, InMemoryNetwork, NodeCore, REVEAL_BUNDLE_COLLECTION_MS},
        domain::{
            Block, GenesisBurn, Ledger, MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS, MICRO_IUNA,
            RECOVERY_BLOCK_DELAY_MS, VDF_TARGET_BLOCK_MS, Wallet,
        },
    };

    #[derive(Clone, Debug)]
    struct ChaosRng {
        state: u64,
    }

    impl ChaosRng {
        fn new(seed: u64) -> Self {
            Self {
                state: seed ^ 0x517c_c1b7_2722_0a95,
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
    }

    #[test]
    fn sparse_network_stays_bounded_under_generated_actions() {
        const NODES: usize = 10;
        const ROUNDS: usize = 36;
        const SETTLE_BLOCKS: u64 = MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS + 10;
        const QUIET_BLOCKS: usize = SETTLE_BLOCKS as usize + 4;

        let mut rng = ChaosRng::new(0x1aba_0100);
        let wallets = (0..NODES)
            .map(|index| Wallet::from_seed(&format!("sparse-network-chaos-{index}")))
            .collect::<Vec<_>>();
        let allocations = wallets
            .iter()
            .map(|wallet| (wallet.address().to_string(), 75 * MICRO_IUNA))
            .collect::<BTreeMap<_, _>>();
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect::<Vec<_>>();
        let ledger = Ledger::new_with_genesis_burns(allocations, genesis_burns, 1)
            .expect("chaos genesis is valid");
        let node_ids = (0..NODES)
            .map(|index| format!("n{index}"))
            .collect::<Vec<_>>();
        let peers = sparse_chaos_peers(NODES, &mut rng);
        let mut offline_until = vec![0_usize; NODES];
        let mut pending_since = BTreeMap::new();
        let mut network = InMemoryNetwork::default();

        for (index, wallet) in wallets.iter().enumerate() {
            let joined = Ledger::from_snapshot(ledger.snapshot()).expect("node joins valid chain");
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                wallet.clone(),
                joined,
                true,
                MICRO_IUNA / 10,
                0,
            );
            node.set_recovery_vdf_top_rank_percent(100);
            network.insert(node_ids[index].clone(), node);
        }

        for round in 0..ROUNDS {
            let online = online_chaos_nodes(&offline_until, round);
            for index in online.iter().copied().collect::<Vec<_>>() {
                match rng.index(32) {
                    0 => {
                        let recipient = wallets[rng.index(wallets.len())].address().to_string();
                        let expiry_height = network
                            .node(&node_ids[index])
                            .expect("actor node exists")
                            .chain_height()
                            + 16;
                        let _ = network
                            .node_mut(&node_ids[index])
                            .expect("actor node exists")
                            .blinded_transfer_with_fee(recipient, 1, 0, expiry_height);
                    }
                    1 => {
                        attempt_bounded_mine_action(
                            &mut network,
                            &node_ids[index],
                            wallets[index].address(),
                            &mut rng,
                        );
                    }
                    2 if online.len() > 1 => {
                        offline_until[index] = round + 1 + rng.index(4);
                    }
                    _ => {}
                }
            }

            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }

        for round in ROUNDS..ROUNDS + QUIET_BLOCKS {
            let online = online_chaos_nodes(&offline_until, round);
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }

        let all_online = (0..NODES).collect::<BTreeSet<_>>();
        for round in ROUNDS + QUIET_BLOCKS..ROUNDS + QUIET_BLOCKS + SETTLE_BLOCKS as usize * 4 {
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &all_online,
                chaos_timestamp(round),
                &mut rng,
            );
            if !sparse_chaos_has_pending(&network, &node_ids) {
                break;
            }
            mine_one_sparse_chaos_block(
                &mut network,
                &node_ids,
                &wallets,
                &all_online,
                chaos_timestamp(round),
            );
            deliver_sparse_chaos_until_idle(
                &mut network,
                &node_ids,
                &peers,
                &all_online,
                chaos_timestamp(round),
                &mut rng,
            );
            observe_bounded_pending(&network, &node_ids, &mut pending_since, SETTLE_BLOCKS);
        }
        assert_sparse_chaos_converged(&network, &node_ids);
        assert_sparse_chaos_mempools_empty(&network, &node_ids);
    }

    fn sparse_chaos_peers(nodes: usize, rng: &mut ChaosRng) -> Vec<Vec<usize>> {
        let mut peers = vec![BTreeSet::new(); nodes];
        for index in 0..nodes {
            let next = (index + 1) % nodes;
            peers[index].insert(next);
            peers[next].insert(index);
        }
        for index in 0..nodes {
            let target_degree = 1 + rng.index(4);
            while peers[index].len() < target_degree {
                let peer = rng.index(nodes);
                if peer != index {
                    peers[index].insert(peer);
                    peers[peer].insert(index);
                }
            }
        }
        peers
            .into_iter()
            .map(|set| set.into_iter().take(4).collect())
            .collect()
    }

    fn online_chaos_nodes(offline_until: &[usize], round: usize) -> BTreeSet<usize> {
        offline_until
            .iter()
            .enumerate()
            .filter_map(|(index, until)| (*until <= round).then_some(index))
            .collect()
    }

    fn chaos_timestamp(round: usize) -> u64 {
        (round as u64 + 1) * (RECOVERY_BLOCK_DELAY_MS + VDF_TARGET_BLOCK_MS)
    }

    fn attempt_bounded_mine_action(
        network: &mut InMemoryNetwork,
        node_id: &str,
        recipient: &str,
        rng: &mut ChaosRng,
    ) {
        let outcome = network
            .node(node_id)
            .expect("mine actor exists")
            .ledger()
            .search_mine(recipient, rng.next_u64(), 0, 4)
            .expect("bounded mine search is valid");
        let Some(transaction) = outcome.transaction else {
            return;
        };
        let _ = network
            .node_mut(node_id)
            .expect("mine actor exists")
            .receive_mine_action(transaction);
    }

    fn mine_one_sparse_chaos_block(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        wallets: &[Wallet],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
    ) {
        let Some(reference_index) = highest_online_node(network, node_ids, online) else {
            return;
        };
        let reference = network
            .node(&node_ids[reference_index])
            .expect("reference node exists");
        let leader = reference.ledger().expected_leader_for_next_block();
        let mut candidates = Vec::new();
        if let Some(index) = leader
            .as_deref()
            .and_then(|leader| {
                wallets
                    .iter()
                    .position(|wallet| wallet.address() == leader)
                    .filter(|index| online.contains(index))
            })
            .filter(|index| {
                network.node(&node_ids[*index]).unwrap().chain_height() == reference.chain_height()
            })
        {
            candidates.push(index);
        }
        candidates.push(reference_index);
        candidates.extend(online.iter().copied().filter(|index| {
            network.node(&node_ids[*index]).unwrap().chain_height() == reference.chain_height()
        }));
        let mut seen = BTreeSet::new();
        for producer_index in candidates {
            if !seen.insert(producer_index) {
                continue;
            }
            let mut producer = network
                .node(&node_ids[producer_index])
                .expect("producer node exists")
                .clone();
            let mut publish_timestamp_ms = timestamp_ms;
            let mut plan = producer.prepare_automatic_finalization(timestamp_ms);
            if plan.work.is_none()
                && plan
                    .skipped_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("collecting blinded reveals"))
            {
                publish_timestamp_ms = timestamp_ms.saturating_add(REVEAL_BUNDLE_COLLECTION_MS + 1);
                plan = producer.prepare_automatic_finalization(publish_timestamp_ms);
            }
            let Some(work) = plan.work else {
                continue;
            };
            let block = work.finish_at(
                &wallets[producer_index],
                "preverified-chaos-vdf".to_string(),
                publish_timestamp_ms,
            );
            network
                .node_mut(&node_ids[producer_index])
                .expect("producer node exists")
                .receive_preverified_block_at(block, publish_timestamp_ms)
                .expect("mock-VDF block applies locally");
            return;
        }
    }

    fn highest_online_node(
        network: &InMemoryNetwork,
        node_ids: &[String],
        online: &BTreeSet<usize>,
    ) -> Option<usize> {
        online.iter().copied().max_by_key(|index| {
            network
                .node(&node_ids[*index])
                .expect("online node exists")
                .chain_height()
        })
    }

    fn deliver_sparse_chaos_until_idle(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
        rng: &mut ChaosRng,
    ) {
        for _ in 0..512 {
            let mut progressed =
                sync_sparse_chaos_once(network, node_ids, peers, online, timestamp_ms);
            progressed |=
                deliver_sparse_chaos_once(network, node_ids, peers, online, timestamp_ms, rng);
            if !progressed {
                return;
            }
        }
        panic!("sparse chaos network did not become idle");
    }

    fn sync_sparse_chaos_once(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
    ) -> bool {
        let mut syncs = Vec::new();
        for from in online {
            let from_height = network.node(&node_ids[*from]).unwrap().chain_height();
            for to in &peers[*from] {
                if !online.contains(to) {
                    continue;
                }
                let to_height = network.node(&node_ids[*to]).unwrap().chain_height();
                if from_height > to_height {
                    let blocks = network
                        .node(&node_ids[*from])
                        .unwrap()
                        .blocks_from(to_height + 1, 16);
                    syncs.push((*to, blocks));
                }
            }
        }

        let mut progressed = false;
        for (to, blocks) in syncs {
            for block in blocks {
                let before = network.node(&node_ids[to]).unwrap().chain_height();
                receive_sparse_chaos_block(network, &node_ids[to], block, timestamp_ms);
                progressed |= network.node(&node_ids[to]).unwrap().chain_height() > before;
            }
        }
        progressed
    }

    fn deliver_sparse_chaos_once(
        network: &mut InMemoryNetwork,
        node_ids: &[String],
        peers: &[Vec<usize>],
        online: &BTreeSet<usize>,
        timestamp_ms: u64,
        rng: &mut ChaosRng,
    ) -> bool {
        let mut outbound = Vec::new();
        for from in online {
            let node = network
                .node_mut(&node_ids[*from])
                .expect("online node exists");
            outbound.extend(
                node.drain_outbox()
                    .into_iter()
                    .map(|envelope| (*from, envelope)),
            );
        }
        if outbound.is_empty() {
            return false;
        }

        while !outbound.is_empty() {
            let index = rng.index(outbound.len());
            let (from, envelope) = outbound.swap_remove(index);
            for to in &peers[from] {
                if online.contains(to) {
                    receive_sparse_chaos_envelope(
                        network,
                        &node_ids[*to],
                        envelope.clone(),
                        timestamp_ms,
                    );
                }
            }
        }
        true
    }

    fn receive_sparse_chaos_envelope(
        network: &mut InMemoryNetwork,
        node_id: &str,
        envelope: GossipEnvelope,
        timestamp_ms: u64,
    ) {
        match envelope {
            GossipEnvelope::Block(block) => {
                receive_sparse_chaos_block(network, node_id, block, timestamp_ms);
            }
            other => {
                if let Err(error) = network
                    .node_mut(node_id)
                    .expect("node exists")
                    .receive(other)
                {
                    let message = error.to_string();
                    assert!(
                        message.contains("mine transaction anchor is not on this chain")
                            || message.contains("conflicts with an existing pending transaction")
                            || message.contains("blinded transaction expired")
                            || message.contains(
                                "blinded reveal does not reference an active blinded transaction",
                            ),
                        "unexpected sparse chaos delivery error: {message}"
                    );
                }
            }
        }
    }

    fn receive_sparse_chaos_block(
        network: &mut InMemoryNetwork,
        node_id: &str,
        block: Block,
        timestamp_ms: u64,
    ) {
        if let Err(error) = network
            .node_mut(node_id)
            .expect("node exists")
            .receive_preverified_block_at(block, timestamp_ms)
        {
            let message = error.to_string();
            assert!(
                message.contains("expected block height")
                    || message.contains("same-height fork")
                    || message.contains("block is already known"),
                "unexpected sparse chaos block error: {message}"
            );
        }
    }

    fn observe_bounded_pending(
        network: &InMemoryNetwork,
        node_ids: &[String],
        pending_since: &mut BTreeMap<String, u64>,
        settle_blocks: u64,
    ) {
        let mut current = BTreeSet::new();
        for node_id in node_ids {
            let node = network.node(node_id).expect("node exists");
            for id in pending_item_ids(node) {
                current.insert(id);
            }
        }
        let max_height = node_ids
            .iter()
            .map(|id| network.node(id).unwrap().chain_height())
            .max()
            .unwrap_or_default();
        pending_since.retain(|id, _| current.contains(id));
        for id in current {
            pending_since.entry(id).or_insert(max_height);
        }
        for (id, first_height) in pending_since {
            assert!(
                max_height.saturating_sub(*first_height) <= settle_blocks,
                "pending item {id} stayed in mempool for more than {settle_blocks} blocks"
            );
        }
    }

    fn pending_item_ids(node: &NodeCore) -> Vec<String> {
        let mut ids = Vec::new();
        ids.extend(
            node.pending_transactions()
                .into_iter()
                .map(|tx| format!("tx:{}", tx.signature())),
        );
        ids.extend(
            node.pending_blinded_transactions()
                .into_iter()
                .map(|tx| format!("commit:{}@{}", tx.commitment, tx.expires_at_height)),
        );
        ids.extend(
            node.pending_blinded_reveals()
                .into_iter()
                .map(|reveal| format!("reveal:{}", reveal.commitment)),
        );
        ids
    }

    fn assert_sparse_chaos_converged(network: &InMemoryNetwork, node_ids: &[String]) {
        let first = network.node(&node_ids[0]).expect("first node exists");
        let height = first.chain_height();
        let tip = first.ledger().status().tip_hash.clone();
        for node_id in node_ids.iter().skip(1) {
            let node = network.node(node_id).expect("node exists");
            assert_eq!(node.chain_height(), height, "{node_id} height diverged");
            assert_eq!(
                node.ledger().status().tip_hash,
                tip,
                "{node_id} tip diverged"
            );
        }
    }

    fn sparse_chaos_has_pending(network: &InMemoryNetwork, node_ids: &[String]) -> bool {
        node_ids.iter().any(|node_id| {
            let node = network.node(node_id).expect("node exists");
            !node.pending_transactions().is_empty()
                || !node.pending_blinded_transactions().is_empty()
                || !node.pending_blinded_reveals().is_empty()
        })
    }

    fn assert_sparse_chaos_mempools_empty(network: &InMemoryNetwork, node_ids: &[String]) {
        for node_id in node_ids {
            let node = network.node(node_id).expect("node exists");
            let pending = pending_item_ids(node);
            assert!(
                pending.is_empty(),
                "{node_id} at height {} still has pending mempool items: {pending:?}",
                node.chain_height()
            );
        }
    }
}
