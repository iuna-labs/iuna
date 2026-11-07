//! Stateful peer-session logic for `iuna-mainnet-candidate`.
//!
//! Socket supervision only has to frame [`Envelope`] values. This adapter owns range sync,
//! compact Ledger-block reconstruction and bounded relay state without leaking transport concerns
//! into the application or consensus layers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, bail};

use crate::{
    adapters::consensus_p2p::{
        CompactBlock, Envelope, Hello, MAX_BLOCKS_PER_MESSAGE, MAX_LOCATOR_HASHES,
        MAX_RELAY_TRANSACTIONS, MAX_SHORT_IDS, encode_line, short_transaction_id,
    },
    app::consensus_node::{ChainRepository, Node},
    consensus::{
        Hash, HeartbeatBlock, MAX_LEDGER_SIDECAR_BYTES, Sidecar, Transaction, check_child_header,
        check_proof_of_work,
    },
};

const MAX_CACHED_RELAY_TRANSACTIONS: usize = MAX_SHORT_IDS;
/// Four full sidecars. This cache holds unvalidated data for each of the node's peers.
const MAX_CACHED_RELAY_BYTES: usize = 4 * MAX_LEDGER_SIDECAR_BYTES;
const MAX_PENDING_COMPACT_BLOCKS: usize = 32;

#[derive(Default)]
pub struct PeerSession {
    remote_hello: Option<Hello>,
    pending_compact: BTreeMap<Hash, CompactBlock>,
    relay_transactions: BTreeMap<Hash, Transaction>,
    relay_order: VecDeque<Hash>,
    relay_bytes: usize,
    /// Last block of an unfinished range that did not become the active tip. It leads the locator
    /// so a side branch longer than one batch keeps advancing instead of restarting at the fork.
    sync_cursor: Option<Hash>,
}

impl PeerSession {
    pub fn remote_hello(&self) -> Option<&Hello> {
        self.remote_hello.as_ref()
    }

    pub fn handle<R: ChainRepository>(
        &mut self,
        node: &mut Node<R>,
        envelope: Envelope,
        now: u64,
    ) -> Result<Vec<Envelope>> {
        envelope.validate_shape()?;
        if self.remote_hello.is_none() && !matches!(envelope, Envelope::Hello(_)) {
            bail!("peer sent data before the hello");
        }

        match envelope {
            Envelope::Hello(hello) => {
                hello.validate()?;
                self.remote_hello = Some(hello.clone());
                let local = node.status();
                let mut responses = Vec::new();
                if hello.tip_hash != local.tip {
                    responses.push(Envelope::Inventory {
                        block_hashes: vec![local.tip],
                    });
                }
                Ok(responses)
            }
            Envelope::Inventory { block_hashes } => {
                if block_hashes
                    .iter()
                    .all(|hash| node.chain().block(hash).is_some())
                {
                    return Ok(Vec::new());
                }
                Ok(vec![self.block_request(node)])
            }
            Envelope::GetBlocks { locator, limit } => Ok(vec![block_batch(blocks_after_locator(
                node,
                &locator,
                usize::from(limit),
            )?)?]),
            Envelope::Blocks { blocks } => {
                let last_received = blocks.last().map(HeartbeatBlock::hash);
                for block in blocks {
                    node.submit_block(block, now)
                        .context("importing block range")?;
                }
                let Some(last_received) = last_received else {
                    self.sync_cursor = None;
                    return Ok(Vec::new());
                };
                let local = node.status();
                self.sync_cursor = (last_received != local.tip).then_some(last_received);
                let behind = self
                    .remote_hello
                    .as_ref()
                    .is_some_and(|remote| local.height < remote.height);
                if behind || self.sync_cursor.is_some() {
                    Ok(vec![self.block_request(node)])
                } else {
                    Ok(Vec::new())
                }
            }
            Envelope::CompactBlock(compact) => self.accept_compact(node, *compact, now),
            Envelope::GetTransactions {
                block_hash,
                short_ids,
            } => Ok(transaction_batches(transactions_for_short_ids(
                node, block_hash, &short_ids,
            ))),
            Envelope::Transactions { transactions } => {
                for transaction in transactions {
                    let id = transaction.id();
                    self.cache_transaction(transaction.clone());
                    if !node.has_pending_transaction(&id) {
                        let _ = node.submit_transaction(transaction);
                    }
                }
                self.retry_compact_blocks(node, now)
            }
            Envelope::Tickets { tickets } => {
                for ticket in tickets {
                    let id = ticket.id();
                    if !node.has_pending_ticket(&id)
                        && !node.chain().tip().tickets.contains_key(&id)
                    {
                        // Like transactions: a ticket that does not fit local state is not a fault.
                        let _ = node.submit_ticket(ticket);
                    }
                }
                Ok(Vec::new())
            }
        }
    }

    fn block_request<R: ChainRepository>(&self, node: &Node<R>) -> Envelope {
        Envelope::GetBlocks {
            locator: sync_locator(self.sync_cursor, block_locator(node)),
            limit: MAX_BLOCKS_PER_MESSAGE as u16,
        }
    }

    fn accept_compact<R: ChainRepository>(
        &mut self,
        node: &mut Node<R>,
        compact: CompactBlock,
        now: u64,
    ) -> Result<Vec<Envelope>> {
        compact.validate_shape()?;
        // Reconstruction hashes the whole mempool, so a header must carry work to get that far.
        check_proof_of_work(&compact.header).context("checking compact block header")?;
        let block_hash = compact.header.hash();
        if node.chain().block(&block_hash).is_some() {
            return Ok(Vec::new());
        }
        // The hash does not cover the short IDs, so a repeat must not buy another reconstruction.
        if self.pending_compact.contains_key(&block_hash) {
            return Ok(Vec::new());
        }
        if self.pending_compact.len() >= MAX_PENDING_COMPACT_BLOCKS {
            bail!("peer exceeds pending compact block limit");
        }
        self.pending_compact.insert(block_hash, compact);
        self.try_complete_compact(node, block_hash, now)
    }

    fn retry_compact_blocks<R: ChainRepository>(
        &mut self,
        node: &mut Node<R>,
        now: u64,
    ) -> Result<Vec<Envelope>> {
        let hashes = self.pending_compact.keys().copied().collect::<Vec<_>>();
        let mut responses = Vec::new();
        for hash in hashes {
            responses.extend(self.try_complete_compact(node, hash, now)?);
        }
        Ok(responses)
    }

    fn try_complete_compact<R: ChainRepository>(
        &mut self,
        node: &mut Node<R>,
        block_hash: Hash,
        now: u64,
    ) -> Result<Vec<Envelope>> {
        let Some(compact) = self.pending_compact.get(&block_hash).cloned() else {
            return Ok(Vec::new());
        };
        // Without a recent parent this node is behind or the block is an old fork: range sync
        // covers the gap, including this block, so there is nothing to retry here.
        let Some(parent) = compact
            .header
            .parent
            .and_then(|parent| node.chain().kept_state(&parent))
        else {
            self.pending_compact.remove(&block_hash);
            return Ok(vec![self.block_request(node)]);
        };
        // A header's own target proves nothing: the chain's target is what makes it cost work.
        if let Err(error) = check_child_header(parent, &compact.header) {
            self.pending_compact.remove(&block_hash);
            return Err(error).context("checking compact block header");
        }
        let available = merge_transactions(
            node.pending_transactions(),
            self.relay_transactions.values().cloned(),
        );
        // `None` marks a short ID two transactions share, which only the peer can resolve.
        let mut by_short_id = BTreeMap::new();
        for transaction in &available {
            by_short_id
                .entry(short_transaction_id(block_hash, transaction.id()))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(transaction));
        }
        let mut transactions = Vec::with_capacity(compact.transaction_short_ids.len());
        let mut missing = Vec::new();
        for short_id in &compact.transaction_short_ids {
            match by_short_id.get(short_id) {
                Some(Some(transaction)) => transactions.push((*transaction).clone()),
                _ => missing.push(*short_id),
            }
        }
        if !missing.is_empty() {
            missing.sort_unstable();
            missing.dedup();
            return Ok(vec![Envelope::GetTransactions {
                block_hash,
                short_ids: missing,
            }]);
        }

        let sidecar = compact.sidecar_header.map(|header| Sidecar {
            header,
            transactions,
        });
        let block = HeartbeatBlock {
            header: compact.header,
            tickets: compact.tickets,
            sidecar,
        };
        node.submit_block(block, now)
            .context("validating reconstructed compact block")?;
        self.pending_compact.remove(&block_hash);
        Ok(Vec::new())
    }

    fn cache_transaction(&mut self, transaction: Transaction) {
        let id = transaction.id();
        if self.relay_transactions.contains_key(&id) {
            return;
        }
        self.relay_bytes += transaction.canonical_size();
        self.relay_transactions.insert(id, transaction);
        self.relay_order.push_back(id);
        while self.relay_transactions.len() > MAX_CACHED_RELAY_TRANSACTIONS
            || self.relay_bytes > MAX_CACHED_RELAY_BYTES
        {
            let Some(oldest) = self.relay_order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.relay_transactions.remove(&oldest) {
                self.relay_bytes -= evicted.canonical_size();
            }
        }
    }
}

fn block_locator<R: ChainRepository>(node: &Node<R>) -> Vec<Hash> {
    let blocks = node.chain().canonical_blocks();
    let mut locator = Vec::new();
    let mut offset = 0usize;
    let mut step = 1usize;
    while offset < blocks.len() && locator.len() < MAX_LOCATOR_HASHES - 1 {
        let index = blocks.len() - 1 - offset;
        locator.push(blocks[index].hash());
        if locator.len() >= 10 {
            step = step.saturating_mul(2);
        }
        offset = offset.saturating_add(step);
    }
    let genesis = blocks[0].hash();
    if locator.last() != Some(&genesis) {
        locator.push(genesis);
    }
    locator
}

fn sync_locator(cursor: Option<Hash>, mut locator: Vec<Hash>) -> Vec<Hash> {
    if let Some(cursor) = cursor {
        locator.insert(0, cursor);
        if locator.len() > MAX_LOCATOR_HASHES {
            // Keep the genesis fallback at the end.
            locator.remove(locator.len() - 2);
        }
    }
    locator
}

fn blocks_after_locator<R: ChainRepository>(
    node: &Node<R>,
    locator: &[Hash],
    limit: usize,
) -> Result<Vec<HeartbeatBlock>> {
    let blocks = node.chain().canonical_blocks();
    let positions = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.hash(), index))
        .collect::<BTreeMap<_, _>>();
    let start = locator
        .iter()
        .find_map(|hash| positions.get(hash).copied())
        .context("peer locator has no common canonical ancestor")?;
    Ok(blocks
        .into_iter()
        .skip(start + 1)
        .take(limit.min(MAX_BLOCKS_PER_MESSAGE))
        .cloned()
        .collect())
}

fn transactions_for_short_ids<R: ChainRepository>(
    node: &Node<R>,
    block_hash: Hash,
    short_ids: &[u64],
) -> Vec<Transaction> {
    let wanted = short_ids.iter().copied().collect::<BTreeSet<_>>();
    let included = node
        .chain()
        .block(&block_hash)
        .and_then(|block| block.sidecar.as_ref())
        .into_iter()
        .flat_map(|sidecar| sidecar.transactions.iter().cloned());
    merge_transactions(included, node.pending_transactions())
        .into_iter()
        .filter(|transaction| wanted.contains(&short_transaction_id(block_hash, transaction.id())))
        .collect()
}

fn block_batch(blocks: Vec<HeartbeatBlock>) -> Result<Envelope> {
    let mut current = Vec::new();
    for block in blocks {
        current.push(block);
        let candidate = Envelope::Blocks {
            blocks: current.clone(),
        };
        if encode_line(&candidate).is_ok() {
            continue;
        }
        current.pop().expect("the candidate contains the new block");
        if current.is_empty() {
            bail!("one Heartbeat block exceeds the P2P wire limit");
        }
        break;
    }
    Ok(Envelope::Blocks { blocks: current })
}

fn transaction_batches(transactions: Vec<Transaction>) -> Vec<Envelope> {
    transactions
        .chunks(MAX_RELAY_TRANSACTIONS)
        .map(|transactions| Envelope::Transactions {
            transactions: transactions.to_vec(),
        })
        .collect()
}

fn merge_transactions(
    primary: impl IntoIterator<Item = Transaction>,
    secondary: impl IntoIterator<Item = Transaction>,
) -> Vec<Transaction> {
    primary
        .into_iter()
        .chain(secondary)
        .map(|transaction| (transaction.id(), transaction))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        app::{
            consensus_node::ChainRepository,
            wallet_commands::{TicketParameters, Wallet},
        },
        consensus::{
            Chain, GENESIS_TIMESTAMP, HEARTBEAT_TARGET_INTERVAL_SECS, INITIAL_LEDGER_SUBSIDY,
            OutPoint, Output, PROTOCOL_VERSION, TicketInput, TicketTransaction, Transaction,
            minimum_ticket_budget,
        },
    };

    const GENESIS_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];

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

    #[test]
    fn hello_is_required_before_other_messages() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let mut session = PeerSession::default();
        assert!(
            session
                .handle(
                    &mut node,
                    Envelope::Inventory {
                        block_hashes: vec![[1; 32]],
                    },
                    GENESIS_TIMESTAMP,
                )
                .is_err()
        );
    }

    #[test]
    fn compact_transaction_sources_are_deduplicated_by_full_id() {
        let transaction = Transaction {
            version: PROTOCOL_VERSION,
            inputs: Vec::new(),
            outputs: vec![Output {
                owner: [81; 32],
                value: 1,
            }],
        };

        let merged = merge_transactions(
            vec![transaction.clone()],
            vec![transaction.clone(), transaction],
        );

        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn missing_compact_transactions_use_bounded_nonempty_batches() {
        assert!(transaction_batches(Vec::new()).is_empty());

        let transactions = (0..=MAX_RELAY_TRANSACTIONS)
            .map(|value| Transaction {
                version: PROTOCOL_VERSION,
                inputs: Vec::new(),
                outputs: vec![Output {
                    owner: [81; 32],
                    value: value as u64 + 1,
                }],
            })
            .collect();
        let batches = transaction_batches(transactions);

        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|batch| batch.validate_shape().is_ok()));
        let lengths = batches
            .into_iter()
            .map(|batch| match batch {
                Envelope::Transactions { transactions } => transactions.len(),
                _ => unreachable!("transaction_batches only creates transaction envelopes"),
            })
            .collect::<Vec<_>>();
        assert_eq!(lengths, vec![MAX_RELAY_TRANSACTIONS, 1]);
    }

    #[test]
    fn range_sync_batches_blocks_by_encoded_wire_size() {
        let mut block = Chain::from_genesis().unwrap().canonical_blocks()[0].clone();
        block.header.network = "x".repeat(3 * 1024 * 1024);

        let batch = block_batch(vec![block.clone(), block.clone(), block]).unwrap();

        assert!(encode_line(&batch).is_ok());
        let Envelope::Blocks { blocks } = batch else {
            unreachable!("block_batch only creates a block envelope");
        };
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn mined_sidecar_transactions_remain_available_for_compact_relay() {
        let wallet = Wallet::from_seed(GENESIS_SEED);
        let budget = minimum_ticket_budget(1, INITIAL_LEDGER_SUBSIDY).unwrap();
        let (node, block, transaction) = (0u32..10_000)
            .find_map(|seed| {
                let mut node = Node::open(MemoryRepository::default()).unwrap();
                let mut lottery_seed = [0u8; 32];
                lottery_seed[28..].copy_from_slice(&seed.to_be_bytes());
                let ticket = wallet
                    .build_ticket(
                        node.chain().tip(),
                        &BTreeSet::new(),
                        TicketParameters {
                            budget,
                            duration: 1,
                            fee: 1,
                            mining_public_key: wallet.public_key(),
                            lottery_seed,
                        },
                    )
                    .unwrap();
                node.mine_block(
                    GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS,
                    [31; 32],
                    vec![ticket],
                    None,
                )
                .unwrap();
                let transaction = wallet
                    .build_transaction(
                        node.chain().tip(),
                        &BTreeSet::new(),
                        [32; 32],
                        1_000_000,
                        11,
                    )
                    .unwrap();
                node.submit_transaction(transaction.clone()).unwrap();
                for height in 2..=7 {
                    node.mine_next(
                        GENESIS_TIMESTAMP + height * HEARTBEAT_TARGET_INTERVAL_SECS,
                        [31; 32],
                    )
                    .unwrap();
                }
                let block = node
                    .mine_next(
                        GENESIS_TIMESTAMP + 8 * HEARTBEAT_TARGET_INTERVAL_SECS,
                        [31; 32],
                    )
                    .unwrap();
                let included = block
                    .sidecar
                    .as_ref()
                    .is_some_and(|sidecar| !sidecar.transactions.is_empty());
                included.then_some((node, block, transaction))
            })
            .expect("a ticket wins a Ledger lottery");

        assert_eq!(node.pending_transaction_count(), 0);
        let short_id = short_transaction_id(block.hash(), transaction.id());
        assert_eq!(
            transactions_for_short_ids(&node, block.hash(), &[short_id]),
            vec![transaction]
        );
    }

    #[test]
    fn locator_sync_imports_a_remote_heartbeat() {
        let repository = MemoryRepository::default();
        let mut source = Node::open(repository).unwrap();
        let block = source
            .mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [51; 32])
            .unwrap();
        let mut target = Node::open(MemoryRepository::default()).unwrap();
        let mut source_session = PeerSession::default();
        let mut target_session = PeerSession::default();
        source_session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(0, crate::consensus::GENESIS_HASH)),
                block.header.timestamp,
            )
            .unwrap();
        target_session
            .handle(
                &mut target,
                Envelope::Hello(Hello::new(1, block.hash())),
                block.header.timestamp,
            )
            .unwrap();

        let request = target_session
            .handle(
                &mut target,
                Envelope::Inventory {
                    block_hashes: vec![block.hash()],
                },
                block.header.timestamp,
            )
            .unwrap()
            .pop()
            .unwrap();
        let response = source_session
            .handle(&mut source, request, block.header.timestamp)
            .unwrap()
            .pop()
            .unwrap();
        target_session
            .handle(&mut target, response, block.header.timestamp)
            .unwrap();

        assert_eq!(target.status().tip, block.hash());
        assert_eq!(target.status().height, 1);
    }

    #[test]
    fn compact_heartbeat_without_sidecar_is_reconstructed() {
        let mut source = Node::open(MemoryRepository::default()).unwrap();
        let block = source
            .mine_next(GENESIS_TIMESTAMP + HEARTBEAT_TARGET_INTERVAL_SECS, [52; 32])
            .unwrap();
        let mut target = Node::open(MemoryRepository::default()).unwrap();
        let mut session = PeerSession::default();
        session
            .handle(
                &mut target,
                Envelope::Hello(Hello::new(1, block.hash())),
                block.header.timestamp,
            )
            .unwrap();

        session
            .handle(
                &mut target,
                Envelope::CompactBlock(Box::new(CompactBlock::from_block(&block))),
                block.header.timestamp,
            )
            .unwrap();

        assert_eq!(target.status().tip, block.hash());
    }

    #[test]
    fn range_sync_paginates_until_the_announced_height() {
        let mut source = Node::open(MemoryRepository::default()).unwrap();
        for height in 1..=9 {
            source
                .mine_next(
                    GENESIS_TIMESTAMP + height * HEARTBEAT_TARGET_INTERVAL_SECS,
                    [53; 32],
                )
                .unwrap();
        }
        let remote = source.status();
        let now = source.chain().tip().tip_timestamp;
        let mut target = Node::open(MemoryRepository::default()).unwrap();
        let mut source_session = PeerSession::default();
        let mut target_session = PeerSession::default();
        source_session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(0, crate::consensus::GENESIS_HASH)),
                now,
            )
            .unwrap();
        target_session
            .handle(
                &mut target,
                Envelope::Hello(Hello::new(remote.height, remote.tip)),
                now,
            )
            .unwrap();

        let mut request = target_session
            .handle(
                &mut target,
                Envelope::Inventory {
                    block_hashes: vec![remote.tip],
                },
                now,
            )
            .unwrap()
            .pop();
        while let Some(next_request) = request {
            let response = source_session
                .handle(&mut source, next_request, now)
                .unwrap()
                .pop()
                .unwrap();
            request = target_session
                .handle(&mut target, response, now)
                .unwrap()
                .pop();
        }

        assert_eq!(target.status(), remote);
    }

    fn mined_node(miner: [u8; 32], height: u64) -> Node<MemoryRepository> {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        for height in 1..=height {
            node.mine_next(
                GENESIS_TIMESTAMP + height * HEARTBEAT_TARGET_INTERVAL_SECS,
                miner,
            )
            .unwrap();
        }
        node
    }

    #[test]
    fn relay_cache_is_bounded_by_bytes_and_evicts_the_oldest() {
        let transaction = |seed: u8| Transaction {
            version: PROTOCOL_VERSION,
            inputs: Vec::new(),
            outputs: (0..1_000)
                .map(|_| Output {
                    owner: [seed; 32],
                    value: 1,
                })
                .collect(),
        };
        let mut session = PeerSession::default();
        let size = transaction(0).canonical_size();
        let capacity = MAX_CACHED_RELAY_BYTES / size;
        let first = transaction(0).id();
        for seed in 0..=capacity {
            session.cache_transaction(transaction(seed as u8));
        }

        assert!(session.relay_bytes <= MAX_CACHED_RELAY_BYTES);
        assert_eq!(session.relay_transactions.len(), capacity);
        assert!(!session.relay_transactions.contains_key(&first));
        assert!(
            session
                .relay_transactions
                .contains_key(&transaction(capacity as u8).id())
        );
    }

    #[test]
    fn compact_header_with_a_self_chosen_target_ends_the_session() {
        let mut source = mined_node([68; 32], 3);
        let tip = source.chain().tip().clone();
        let mut compact = CompactBlock::from_block(source.chain().block(&tip.tip).unwrap());
        compact.header.parent = Some(tip.tip);
        compact.header.height += 1;
        // Valid work, but for a target the header picked instead of the one consensus demands.
        compact.header.target =
            crate::consensus::expected_target(&tip, compact.header.height).unwrap();
        compact.header.target[31] ^= 1;
        while crate::consensus::check_proof_of_work(&compact.header).is_err() {
            compact.header.nonce += 1;
        }
        let mut session = PeerSession::default();
        let status = source.status();
        let now = compact.header.timestamp;
        session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(status.height, status.tip)),
                now,
            )
            .unwrap();
        let result = session.handle(&mut source, Envelope::CompactBlock(Box::new(compact)), now);

        assert!(result.is_err());
        assert!(session.pending_compact.is_empty());
    }

    #[test]
    fn a_repeated_compact_block_does_not_reconstruct_again() {
        let mut source = mined_node([69; 32], 1);
        let parent = source.chain().tip().clone();
        let timestamp = parent.tip_timestamp + HEARTBEAT_TARGET_INTERVAL_SECS;
        let (block, _) =
            crate::consensus::mine_child(&parent, timestamp, [70; 32], Vec::new(), None).unwrap();
        let compact = CompactBlock::from_block(&block);
        let mut session = PeerSession::default();
        session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(parent.height, parent.tip)),
                timestamp,
            )
            .unwrap();
        session
            .pending_compact
            .insert(block.hash(), compact.clone());

        let repeat = session
            .handle(
                &mut source,
                Envelope::CompactBlock(Box::new(compact)),
                timestamp,
            )
            .unwrap();

        // This block needs nothing, so reconstructing it would have imported it.
        assert!(repeat.is_empty());
        assert_eq!(source.status().tip, parent.tip);
    }

    #[test]
    fn compact_header_without_work_ends_the_session_before_reconstruction() {
        let mut source = mined_node([66; 32], 1);
        let mut block = source.chain().block(&source.status().tip).unwrap().clone();
        while block.header.hash() <= block.header.target {
            block.header.nonce += 1;
        }
        let mut session = PeerSession::default();
        let status = source.status();
        session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(status.height, status.tip)),
                block.header.timestamp,
            )
            .unwrap();

        let result = session.handle(
            &mut source,
            Envelope::CompactBlock(Box::new(CompactBlock::from_block(&block))),
            block.header.timestamp,
        );

        assert!(result.is_err());
        assert!(session.pending_compact.is_empty());
    }

    #[test]
    fn heavier_fork_longer_than_one_batch_activates() {
        let mut source = mined_node([54; 32], 12);
        let mut target = mined_node([55; 32], 10);
        let remote = source.status();
        let now = source.chain().tip().tip_timestamp;
        let mut source_session = PeerSession::default();
        let mut target_session = PeerSession::default();
        source_session
            .handle(
                &mut source,
                Envelope::Hello(Hello::new(10, target.status().tip)),
                now,
            )
            .unwrap();
        target_session
            .handle(
                &mut target,
                Envelope::Hello(Hello::new(remote.height, remote.tip)),
                now,
            )
            .unwrap();

        let mut request = target_session
            .handle(
                &mut target,
                Envelope::Inventory {
                    block_hashes: vec![remote.tip],
                },
                now,
            )
            .unwrap()
            .pop();
        let mut round_trips = 0;
        while let Some(next_request) = request {
            round_trips += 1;
            assert!(round_trips <= 3, "fork sync must make progress");
            let response = source_session
                .handle(&mut source, next_request, now)
                .unwrap()
                .pop()
                .unwrap();
            request = target_session
                .handle(&mut target, response, now)
                .unwrap()
                .pop();
        }

        assert_eq!(target.status(), remote);
    }

    #[test]
    fn sync_locator_keeps_the_cursor_and_genesis_within_the_limit() {
        let locator = (0..MAX_LOCATOR_HASHES as u8)
            .map(|byte| [byte; 32])
            .collect::<Vec<_>>();
        let extended = sync_locator(Some([255; 32]), locator.clone());
        assert_eq!(extended.len(), MAX_LOCATOR_HASHES);
        assert_eq!(extended.first(), Some(&[255; 32]));
        assert_eq!(extended.last(), locator.last());
    }

    #[test]
    fn compact_block_on_unknown_parent_requests_a_range() {
        let source = mined_node([56; 32], 2);
        let block = source.chain().block(&source.status().tip).unwrap().clone();
        let mut target = Node::open(MemoryRepository::default()).unwrap();
        let mut session = PeerSession::default();
        session
            .handle(
                &mut target,
                Envelope::Hello(Hello::new(2, block.hash())),
                block.header.timestamp,
            )
            .unwrap();

        let responses = session
            .handle(
                &mut target,
                Envelope::CompactBlock(Box::new(CompactBlock::from_block(&block))),
                block.header.timestamp,
            )
            .unwrap();

        assert!(matches!(responses[..], [Envelope::GetBlocks { .. }]));
        assert!(session.pending_compact.is_empty());
    }

    #[test]
    fn unusable_relayed_ticket_keeps_the_session() {
        let mut node = Node::open(MemoryRepository::default()).unwrap();
        let status = node.status();
        let mut session = PeerSession::default();
        session
            .handle(
                &mut node,
                Envelope::Hello(Hello::new(status.height, status.tip)),
                GENESIS_TIMESTAMP,
            )
            .unwrap();

        let ticket = TicketTransaction {
            version: PROTOCOL_VERSION,
            inputs: vec![TicketInput {
                previous_output: OutPoint {
                    transaction_id: [57; 32],
                    output_index: 0,
                },
                public_key: [58; 32],
                signature: vec![0; 64],
            }],
            budget: 1,
            duration: 1,
            mining_public_key: [59; 32],
            lottery_seed: [60; 32],
            change: None,
        };
        let responses = session
            .handle(
                &mut node,
                Envelope::Tickets {
                    tickets: vec![ticket],
                },
                GENESIS_TIMESTAMP,
            )
            .unwrap();

        assert!(responses.is_empty());
        assert_eq!(node.pending_ticket_count(), 0);
    }
}
