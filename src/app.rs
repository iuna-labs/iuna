use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::domain::{
    Amount, BlindedReveal, BlindedTransaction, BuiltBlindedTransaction, Ledger,
    MINE_ACTIONS_PER_ANCHOR_LIMIT, PreparedBlock, RevealBundle, Transaction, run_vdf,
};

mod automatic_mining;
mod gossip;
mod helpers;
mod in_memory_network;
mod ledger_view;
mod node_lifecycle;
mod owned_blinded;
mod peer_book;
mod receive;
mod status;
mod types;
mod wallet;
pub use in_memory_network::InMemoryNetwork;
pub use peer_book::{PeerBook, PeerDirection, PeerInfo};
pub use types::{
    AutoMineOutcome, AutoMinePlan, BlockInventory, ExternalMineJob, FeeEstimate, GossipEnvelope,
    LaunchProfileStatus, MiningStatus, NodeConfig, NodeStatus, ProtocolHello, StratumStatus,
};
use wallet::NodeWallet;

pub type SharedNode = Arc<Mutex<NodeCore>>;
pub type SharedPeerBook = Arc<Mutex<PeerBook>>;

pub const DEFAULT_BURN_PER_BLOCK: Amount = 0;
pub const DEFAULT_VDF_ROUNDS: u32 = 67_000_000;
pub const PROTOCOL_VERSION: u32 = 1;
pub const NETWORK_ID: &str = "iuna-devnet-v3";
pub const BLOCK_REQUEST_LIMIT: usize = 128;
pub const TRANSACTION_BATCH_LIMIT: usize = 128;
const IMPORT_REBROADCAST_LIMIT: usize = 128;
pub const PEER_MISBEHAVIOR_BAN_SCORE: u32 = 3;
pub const PEER_MISBEHAVIOR_BAN_MS: u64 = 10 * 60 * 1_000;
pub const PEER_CLOCK_OFFSET_ACCEPTANCE_MS: i64 = 10 * 60 * 1_000;
const PEER_CLOCK_OFFSET_STALE_MS: u64 = 20 * 60 * 1_000;
const AUTO_POW_NONCE_ATTEMPTS_PER_WORKER_TICK: u64 = 100_000;
const AUTO_PLAINTEXT_BURN_BEFORE_RECOVERY_MS: u64 = 60_000;
const REVEAL_BUNDLE_COLLECTION_MS: u64 = 30_000;
const AUTO_BLOCK_ANCHOR_BURN_AMOUNT: Amount = 1;
const AUTO_BLOCK_ANCHOR_BURN_FEE: Amount = 0;
static DEBUG_LOGGING: AtomicBool = AtomicBool::new(false);

pub fn set_debug_logging(enabled: bool) {
    DEBUG_LOGGING.store(enabled, Ordering::Relaxed);
}

pub fn debug_logging_enabled() -> bool {
    DEBUG_LOGGING.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AutoPowMineCursor {
    anchor: String,
    salt: u64,
    next_nonce: u64,
    searched: u64,
}

#[derive(Clone, Debug)]
pub struct AutoPowMineJob {
    ledger: Ledger,
    recipient: String,
    anchor: String,
    salt: u64,
    start_nonce: u64,
    max_attempts: u64,
}

impl AutoPowMineJob {
    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    pub fn search(self) -> Result<(Self, crate::domain::MineSearchOutcome)> {
        let outcome = self.ledger.search_mine(
            self.recipient.clone(),
            self.salt,
            self.start_nonce,
            self.max_attempts,
        )?;
        Ok((self, outcome))
    }
}

#[derive(Clone, Debug)]
pub struct NodeCore {
    wallet: NodeWallet,
    ledger: Ledger,
    automatic_mining_enabled: bool,
    pow_mining_enabled: bool,
    pow_mining_workers: u8,
    burn_per_block: Amount,
    burn_fee: Amount,
    recovery_vdf_top_rank_percent: u8,
    last_auto_burn_height: Option<u64>,
    last_auto_anchor_burn_height: Option<u64>,
    last_auto_pow_mine_anchor: Option<String>,
    last_auto_pow_mine_status: Option<String>,
    auto_pow_mine_cursor: Option<AutoPowMineCursor>,
    owned_blinded_transactions: BTreeMap<String, BlindedTransaction>,
    owned_blinded_reveals: BTreeMap<String, BlindedReveal>,
    owned_blinded_payloads: BTreeMap<String, Transaction>,
    owned_blinded_outbox_version: u64,
    reveal_bundles: BTreeMap<(u64, u8), RevealBundle>,
    equivocated_reveal_bundle_slots: BTreeSet<(u64, u8)>,
    reveal_bundle_collection_started: Option<(u64, u64)>,
    local_block_anchor_burn: Option<(u64, Transaction)>,
    outbox: Vec<GossipEnvelope>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is before unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::domain::{
        GenesisBurn, Ledger, MICRO_IUNA, OutPoint, Transaction, VDF_TARGET_BLOCK_MS, Wallet,
        run_vdf,
    };

    use super::{
        InMemoryNetwork, NodeCore, REVEAL_BUNDLE_COLLECTION_MS,
        helpers::transaction_input_outpoints,
    };

    fn wallet_for_address<'a>(wallets: &'a [Wallet], address: &str) -> &'a Wallet {
        wallets
            .iter()
            .find(|wallet| wallet.address() == address)
            .unwrap_or_else(|| panic!("missing wallet for address {address}"))
    }

    fn queue_auto_pow_mine_action(node: &mut NodeCore) -> Transaction {
        node.set_pow_mining_enabled(true);
        (0..10_000)
            .find_map(|timestamp| node.prepare_automatic_mining(timestamp).pow_mined)
            .expect("test node should find a PoW mine action")
    }

    fn assert_block_has_mine_action(block: &crate::domain::Block) {
        assert!(
            block
                .transactions
                .iter()
                .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
            "block {} should include a mine action",
            block.height
        );
    }

    #[test]
    fn automatic_finalization_includes_reveals_with_two_nodes_and_one_burner() {
        let finalizer = Wallet::from_seed("single-burner-reveal-finalizer");
        let wallet = Wallet::from_seed("single-burner-reveal-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
            1,
        )
        .unwrap();
        let mut network = InMemoryNetwork::default();
        network.insert(
            "finalizer",
            NodeCore::from_ledger_with_burn_fee_and_enabled(
                finalizer.clone(),
                ledger.clone(),
                true,
                MICRO_IUNA / 10,
                1,
            ),
        );
        network.insert("wallet", NodeCore::from_ledger(wallet.clone(), ledger, 0));

        let blinded = network
            .node_mut("wallet")
            .unwrap()
            .blinded_burn_with_fee(MICRO_IUNA / 10, 1, 4)
            .unwrap();
        network.deliver_until_idle().unwrap();

        let commit_plan = network
            .node_mut("finalizer")
            .unwrap()
            .prepare_automatic_finalization(1);
        let commit_work = commit_plan
            .work
            .expect("finalizer should prepare commit block");
        let commit_vdf = run_vdf(commit_work.vdf_seed(), commit_work.vdf_rounds());
        let commit_block = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(commit_work, commit_vdf, 1)
            .unwrap();
        let wallet_commitment = blinded.commitment.clone();
        assert!(
            commit_block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == wallet_commitment),
            "first block should commit the wallet's blinded burn"
        );
        network.deliver_until_idle().unwrap();
        assert!(
            network
                .node("finalizer")
                .unwrap()
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "finalizer should have received the reveal before building the next block"
        );
        assert!(
            network
                .node("wallet")
                .unwrap()
                .ledger()
                .pending_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "wallet node should also keep the reveal in its mempool"
        );

        let reveal_plan = network
            .node_mut("finalizer")
            .unwrap()
            .prepare_automatic_finalization(2);
        assert!(reveal_plan.work.is_none());
        assert!(
            reveal_plan
                .skipped_reason
                .as_deref()
                .unwrap_or_default()
                .contains("collecting blinded reveals")
        );
        let reveal_plan = network
            .node_mut("finalizer")
            .unwrap()
            .prepare_automatic_finalization(REVEAL_BUNDLE_COLLECTION_MS + 2);
        let reveal_work = reveal_plan
            .work
            .expect("finalizer should prepare reveal block");
        let reveal_vdf = run_vdf(reveal_work.vdf_seed(), reveal_work.vdf_rounds());
        let reveal_block = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(reveal_work, reveal_vdf, 2)
            .unwrap();

        assert!(
            reveal_block
                .all_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == wallet_commitment),
            "automatic finalization should include the pending reveal without requiring an extra mempool poll"
        );
    }

    #[test]
    fn genesis_transfer_arriving_during_vdf_with_peer_mines_does_not_stall_following_blocks() {
        let miner = Wallet::from_seed("during-vdf-miner");
        let (_finalizer, finalizer_node, block2_work) = (0..1_000)
            .find_map(|seed_index| {
                let finalizer = Wallet::from_seed(&format!("during-vdf-finalizer-{seed_index}"));
                let mut allocations = BTreeMap::new();
                allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
                let mut ledger = Ledger::new_with_genesis_burns(
                    allocations,
                    vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
                    1,
                )
                .unwrap();
                let split = ledger
                    .build_transfer(&finalizer, finalizer.address(), MICRO_IUNA / 10, 0)
                    .ok()?;
                let split_change = OutPoint {
                    txid: split.signature().to_string(),
                    index: 1,
                };
                ledger.submit_transaction(split).ok()?;
                let burn = ledger
                    .build_burn_with_inputs(&finalizer, 1, 0, &[split_change])
                    .ok()?;
                ledger.submit_transaction(burn).ok()?;
                let block1 = ledger.mine_next_block(&finalizer, 1).ok()?;
                assert!(block1.blinded_transactions.is_empty());
                assert!(
                    !block1
                        .transactions
                        .iter()
                        .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
                    "node B joins after block 1, so block 1 should not include B's mine action"
                );
                ledger.apply_block(block1).ok()?;
                let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                    finalizer.clone(),
                    ledger,
                    true,
                    0,
                    100,
                );
                let block2_plan = node.prepare_automatic_finalization(2);
                let block2_work = block2_plan.work?;
                let (_, anchor_burn) = node.local_block_anchor_burn.clone()?;
                let anchor_inputs = transaction_input_outpoints(&anchor_burn);
                let anchor_total = node
                    .ledger()
                    .utxos_for_address(finalizer.address())
                    .iter()
                    .filter(|(outpoint, _)| anchor_inputs.contains(outpoint))
                    .map(|(_, output)| output.amount)
                    .sum::<u64>();
                if anchor_total >= 200_000 {
                    return None;
                }
                Some((finalizer, node, block2_work))
            })
            .expect("test should find a seed where block 2 anchor uses the small reward UTXO");
        let mut network = InMemoryNetwork::default();
        network.insert("finalizer", finalizer_node);

        let miner_ledger =
            Ledger::from_snapshot(network.node("finalizer").unwrap().chain_snapshot()).unwrap();
        network.insert(
            "miner",
            NodeCore::from_ledger(miner.clone(), miner_ledger, 0),
        );

        queue_auto_pow_mine_action(network.node_mut("miner").unwrap());
        network.deliver_until_idle().unwrap();
        network
            .node_mut("finalizer")
            .unwrap()
            .transfer_with_fee_rate(miner.address(), MICRO_IUNA / 10, 100, &[])
            .unwrap();
        let blinded = network
            .node("finalizer")
            .unwrap()
            .ledger()
            .pending_blinded_transactions()
            .last()
            .cloned()
            .expect("A should queue the A -> B blinded transfer while block 2 VDF is running");
        network.deliver_until_idle().unwrap();
        assert!(
            network
                .node("finalizer")
                .unwrap()
                .ledger()
                .pending_blinded_transactions()
                .iter()
                .any(|tx| tx.commitment == blinded.commitment),
            "the finalizer should receive the blinded tx while block 2 VDF is running"
        );

        let block2_vdf = run_vdf(block2_work.vdf_seed(), block2_work.vdf_rounds());
        let block2 = network
            .node_mut("finalizer")
            .unwrap()
            .complete_prepared_block_at(block2_work, block2_vdf, 2)
            .unwrap();
        assert!(
            block2.blinded_transactions.is_empty(),
            "block 2 work was prepared before the blinded tx arrived"
        );
        assert!(
            !block2
                .transactions
                .iter()
                .any(|transaction| matches!(transaction, Transaction::Mine { .. })),
            "block 2 work was prepared before B's mine action arrived"
        );
        network.deliver_until_idle().unwrap();

        let block3_outcome = network
            .node_mut("finalizer")
            .unwrap()
            .automatic_mine_once(3);
        assert!(
            block3_outcome.block.is_some(),
            "finalizer should keep producing after the during-VDF blinded tx: {:?}",
            block3_outcome.skipped_reason
        );
        let block3 = block3_outcome.block.unwrap();
        assert!(
            block3
                .transactions
                .first()
                .is_some_and(Transaction::is_burn),
            "the mandatory anchor burn must be selected before during-VDF mempool items"
        );
        let committed_blinded = block3
            .blinded_transactions
            .iter()
            .any(|tx| tx.commitment == blinded.commitment);
        assert_block_has_mine_action(&block3);
        network.deliver_until_idle().unwrap();

        queue_auto_pow_mine_action(network.node_mut("miner").unwrap());
        network.deliver_until_idle().unwrap();
        let block4_started_at = block3.timestamp_ms.saturating_add(1);
        let mut block4_outcome = network
            .node_mut("finalizer")
            .unwrap()
            .automatic_mine_once(block4_started_at);
        if block4_outcome
            .skipped_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("collecting blinded reveals"))
        {
            block4_outcome = network.node_mut("finalizer").unwrap().automatic_mine_once(
                block4_started_at.saturating_add(REVEAL_BUNDLE_COLLECTION_MS + 1),
            );
        }
        let block4 = block4_outcome
            .block
            .expect("finalizer should keep producing the next block");
        if committed_blinded {
            assert!(
                block4
                    .all_blinded_reveals()
                    .iter()
                    .any(|reveal| reveal.commitment == blinded.commitment),
                "the committed during-VDF blinded tx should reveal in a later block"
            );
        } else {
            assert!(
                network
                    .node("finalizer")
                    .unwrap()
                    .ledger()
                    .pending_blinded_transactions()
                    .is_empty(),
                "a conflicting during-VDF blinded tx should be pruned after the anchor burn spends its input"
            );
            assert!(
                network
                    .node("finalizer")
                    .unwrap()
                    .owned_blinded_transactions()
                    .is_empty(),
                "owned blinded state should not keep rebroadcasting a pruned tx"
            );
            assert!(
                block4.all_blinded_reveals().is_empty(),
                "a pruned blinded tx was never committed, so there should be no reveal"
            );
        }
        assert_block_has_mine_action(&block4);
    }

    #[test]
    fn own_blinded_transaction_arriving_during_vdf_does_not_starve_next_anchor_burn() {
        let recipient = Wallet::from_seed("during-vdf-own-recipient");
        let (mut node, block2_work, transfer_outpoint, transfer_amount) = (0..1_000)
            .find_map(|seed_index| {
                let finalizer =
                    Wallet::from_seed(&format!("during-vdf-own-finalizer-{seed_index}"));
                let mut allocations = BTreeMap::new();
                allocations.insert(finalizer.address().to_string(), 10 * MICRO_IUNA);
                let ledger = Ledger::new_with_genesis_burns(
                    allocations,
                    vec![GenesisBurn::new(finalizer.address(), MICRO_IUNA)],
                    1,
                )
                .unwrap();
                let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                    finalizer.clone(),
                    ledger,
                    true,
                    0,
                    1_000,
                );
                node.set_pow_mining_enabled(true);
                let block1 = node.automatic_mine_once(1).block?;
                assert!(block1.blinded_transactions.is_empty());

                let block2_plan = node.prepare_automatic_finalization(2);
                let block2_work = block2_plan.work?;
                let (_, anchor_burn) = node.local_block_anchor_burn.clone()?;
                let anchor_inputs = transaction_input_outpoints(&anchor_burn);
                let utxos = node.ledger().utxos_for_address(finalizer.address());
                let anchor_total = utxos
                    .iter()
                    .filter(|(outpoint, _)| anchor_inputs.contains(outpoint))
                    .map(|(_, output)| output.amount)
                    .sum::<u64>();
                let (transfer_outpoint, transfer_output) =
                    utxos.into_iter().find(|(outpoint, output)| {
                        !anchor_inputs.contains(outpoint) && output.amount > MICRO_IUNA
                    })?;
                if anchor_total >= 2_000_000 {
                    return None;
                }
                Some((
                    node,
                    block2_work,
                    transfer_outpoint,
                    transfer_output.amount.min(MICRO_IUNA) / 10,
                ))
            })
            .expect("test should find a seed with live-like small-anchor/large-change UTXOs");

        node.transfer_with_fee_spending(
            recipient.address(),
            transfer_amount,
            1,
            &[transfer_outpoint],
        )
        .expect("wallet tx created while block 2 VDF is running");
        let blinded = node
            .ledger()
            .pending_blinded_transactions()
            .last()
            .cloned()
            .expect("wallet tx should be queued as a blinded transaction");

        let block2_vdf = run_vdf(block2_work.vdf_seed(), block2_work.vdf_rounds());
        let block2 = node
            .complete_prepared_block_at(block2_work, block2_vdf, 2)
            .unwrap();
        assert!(
            block2.blinded_transactions.is_empty(),
            "block 2 work was prepared before the blinded tx arrived"
        );
        assert!(
            node.ledger()
                .pending_blinded_transactions()
                .iter()
                .any(|tx| tx.commitment == blinded.commitment),
            "the during-VDF blinded tx should remain pending for block 3"
        );

        let block3_outcome = node.automatic_mine_once(3);
        assert!(
            block3_outcome.block.is_some(),
            "pending own blinded tx must not starve the next anchor burn: {:?}",
            block3_outcome.skipped_reason
        );
        let block3 = block3_outcome.block.unwrap();
        assert!(
            block3.blinded_transactions.is_empty()
                || block3
                    .blinded_transactions
                    .iter()
                    .any(|tx| tx.commitment == blinded.commitment),
            "block 3 may include the during-VDF tx, but must not stall when the anchor burn has priority"
        );
    }

    #[test]
    fn locally_produced_blocks_import_on_independent_peer_ledger() {
        let alice = Wallet::from_seed("producer-parity-alice");
        let bob = Wallet::from_seed("producer-parity-bob");
        let carol = Wallet::from_seed("producer-parity-carol");
        let wallets = [alice.clone(), bob.clone(), carol.clone()];
        let mut allocations = BTreeMap::new();
        for wallet in &wallets {
            allocations.insert(wallet.address().to_string(), 20 * MICRO_IUNA);
        }
        let genesis_burns = wallets
            .iter()
            .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
            .collect();
        let mut producer_ledger =
            Ledger::new_with_genesis_burns(allocations, genesis_burns, 1).unwrap();
        let mut peer_ledger = producer_ledger.clone();

        for step in 0..8 {
            assert_eq!(
                producer_ledger.status().tip_hash,
                peer_ledger.status().tip_hash
            );
            let leader = producer_ledger
                .expected_leader_for_next_block()
                .expect("test chain should have an eligible leader");
            let leader_wallet = wallet_for_address(&wallets, &leader).clone();
            let timestamp_ms = (step + 1) as u64 * VDF_TARGET_BLOCK_MS;
            let mut node = NodeCore::from_ledger_with_burn_fee_and_enabled(
                leader_wallet.clone(),
                producer_ledger.clone(),
                true,
                MICRO_IUNA / 10,
                1,
            );
            let plan = node.prepare_automatic_finalization(timestamp_ms);
            assert!(plan.burned.is_some());

            match step % 3 {
                0 => {
                    let _ = node.blinded_burn_with_fee(1, 0, node.chain_height() + 4);
                }
                1 => {
                    let recipient = wallets[(step + 1) % wallets.len()].address();
                    let _ =
                        node.blinded_transfer_with_fee(recipient, 1, 0, node.chain_height() + 4);
                }
                _ => {}
            }

            let work = node
                .prepare_next_block_with_local_anchor(timestamp_ms)
                .unwrap();
            let vdf_output = run_vdf(work.vdf_seed(), work.vdf_rounds());
            let block = node
                .complete_prepared_block_at(work, vdf_output, timestamp_ms)
                .unwrap();
            peer_ledger.apply_block_at(block, u64::MAX).unwrap();
            producer_ledger = node.clone_ledger();
            assert_eq!(
                producer_ledger.status().tip_hash,
                peer_ledger.status().tip_hash
            );
        }
    }
}
