use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    Amount, BURN_COMMITTEE_SIZE, BurnBundleSection, BurnTicket, LaunchProfile, LeaderScore,
    Transaction, Wallet, hex_hash, recovery_vdf_seed_for_child, vdf_content_commitment,
    vdf_seed_for_child,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Block {
    pub height: u64,
    pub prev_hash: String,
    pub timestamp_ms: u64,
    pub miner: String,
    #[serde(default)]
    pub finalizer_mode: FinalizerMode,
    #[serde(default)]
    pub finalizer_rank: u32,
    pub reward: Amount,
    pub vdf_rounds: u64,
    pub vdf_output: String,
    pub leader_proof: Option<LeaderProof>,
    #[serde(default)]
    pub burn_bundle_section: BurnBundleSection,
    pub transactions: Vec<Transaction>,
    /// Canonical hex-encoded transaction-v2 wire envelopes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transactions_v2: Vec<String>,
    pub hash: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalizerMode {
    #[default]
    Ticket,
    Recovery,
}

impl Block {
    pub fn compute_hash(&self) -> String {
        hex_hash(format!(
            "block:{}:{}:{}",
            self.content_hash(),
            self.vdf_seed(),
            self.vdf_output,
        ))
    }

    pub fn vdf_seed(&self) -> String {
        let bundle_hashes = self.burn_bundle_hashes();
        let content_commitment = self.vdf_content_commitment();
        match self.finalizer_mode {
            FinalizerMode::Ticket => vdf_seed_for_child(
                &self.prev_hash,
                self.height,
                &bundle_hashes,
                &content_commitment,
            ),
            FinalizerMode::Recovery => recovery_vdf_seed_for_child(
                &self.prev_hash,
                self.height,
                self.timestamp_ms,
                &bundle_hashes,
                &content_commitment,
            ),
        }
    }

    fn vdf_content_commitment(&self) -> String {
        vdf_content_commitment(
            self.height,
            &self.prev_hash,
            &self.miner,
            self.finalizer_mode,
            self.finalizer_rank,
            self.reward,
            self.vdf_rounds,
            self.leader_proof
                .as_ref()
                .map(|proof| proof.ticket_id.as_str()),
            &self.burn_bundle_section,
            &self.transactions,
            &self.transactions_v2,
        )
    }

    fn content_hash(&self) -> String {
        let txs = self
            .transactions
            .iter()
            .map(Transaction::canonical)
            .collect::<Vec<_>>()
            .join("|");
        let txs_v2 = self.transactions_v2.join("|");
        let burn_section = self.burn_bundle_section.canonical();
        let leader_proof = self
            .leader_proof
            .as_ref()
            .map(|proof| {
                format!(
                    "{}:{}:{}",
                    proof.ticket_id, proof.public_key, proof.signature
                )
            })
            .unwrap_or_default();
        if self.transactions_v2.is_empty() {
            return hex_hash(format!(
                "block-content-v4:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.timestamp_ms,
                self.miner,
                self.finalizer_rank,
                self.reward,
                self.vdf_rounds,
                leader_proof,
                txs,
                canonical_burn_block_items(&burn_section)
            ));
        }
        hex_hash(format!(
            "block-content-v5:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.height,
            self.prev_hash,
            self.timestamp_ms,
            self.miner,
            self.finalizer_rank,
            self.reward,
            self.vdf_rounds,
            leader_proof,
            txs,
            txs_v2,
            canonical_burn_block_items(&burn_section)
        ))
    }

    pub(super) fn leader_score(&self) -> LeaderScore {
        LeaderScore {
            finalizer_mode_rank: self.finalizer_mode.fork_choice_rank(),
            finalizer_rank: self.finalizer_rank,
            proof_rank: self
                .leader_proof
                .as_ref()
                .map(LeaderProof::rank)
                .unwrap_or_else(|| self.hash.clone()),
        }
    }

    pub fn json_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize block as JSON for size check")
    }

    pub fn required_burns(&self) -> Vec<&Transaction> {
        self.burn_bundle_section.required_burns()
    }

    pub fn burn_bundle_hashes(&self) -> [String; BURN_COMMITTEE_SIZE] {
        self.burn_bundle_section
            .burn_bundle_hashes(self.height, &self.prev_hash, &self.miner)
    }

    pub fn included_burn_bundle_count(&self) -> usize {
        self.burn_bundle_section.included_bundle_count()
    }
}

impl FinalizerMode {
    fn fork_choice_rank(self) -> u8 {
        match self {
            Self::Ticket => 0,
            Self::Recovery => 1,
        }
    }
}

fn canonical_burn_block_items(burn_section: &str) -> String {
    format!("burn-section-v1:{burn_section}")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeaderProof {
    pub ticket_id: String,
    pub public_key: String,
    pub signature: String,
}

impl LeaderProof {
    fn rank(&self) -> String {
        hex_hash(format!(
            "iuna-leader-rank:{}:{}",
            self.ticket_id, self.signature
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LeaderProofPayload {
    pub(super) height: u64,
    pub(super) prev_hash: String,
    pub(super) finalizer_rank: u32,
    pub(super) vdf_output: String,
    pub(super) ticket_id: String,
    pub(super) ticket_amount: Amount,
    pub(super) ticket_owner: String,
}

impl LeaderProofPayload {
    pub(super) fn canonical(&self) -> String {
        if self.finalizer_rank == 0 {
            format!(
                "iuna-leader-proof:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.vdf_output,
                self.ticket_id,
                self.ticket_amount,
                self.ticket_owner
            )
        } else {
            format!(
                "iuna-leader-proof-v2:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.finalizer_rank,
                self.vdf_output,
                self.ticket_id,
                self.ticket_amount,
                self.ticket_owner
            )
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnLeaderRank {
    pub rank: u32,
    pub ticket_id: String,
    pub owner: String,
    pub amount: Amount,
    pub eligible_from_height: u64,
    pub eligible_until_height: u64,
}

#[derive(Clone, Debug)]
pub struct PreparedBlock {
    pub(super) height: u64,
    pub(super) prev_hash: String,
    pub(super) timestamp_ms: u64,
    pub(super) miner: String,
    pub(super) finalizer_mode: FinalizerMode,
    pub(super) finalizer_rank: u32,
    pub(super) reward: Amount,
    pub(super) vdf_rounds: u64,
    pub(super) vdf_seed: String,
    pub(super) leader_ticket: Option<BurnTicket>,
    pub(super) burn_bundle_section: BurnBundleSection,
    pub(super) transactions: Vec<Transaction>,
    pub(super) transactions_v2: Vec<String>,
}

impl PreparedBlock {
    pub fn vdf_seed(&self) -> &str {
        &self.vdf_seed
    }

    pub fn vdf_rounds(&self) -> u64 {
        self.vdf_rounds
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    pub fn prev_hash(&self) -> &str {
        &self.prev_hash
    }

    pub fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    pub fn finish(self, wallet: &Wallet, vdf_output: String) -> Block {
        let timestamp_ms = self.timestamp_ms;
        self.finish_with_timestamp(wallet, vdf_output, timestamp_ms)
    }

    pub fn finish_at(self, wallet: &Wallet, vdf_output: String, timestamp_ms: u64) -> Block {
        let timestamp_ms = match self.finalizer_mode {
            FinalizerMode::Ticket => timestamp_ms.max(self.timestamp_ms),
            FinalizerMode::Recovery => self.timestamp_ms,
        };
        self.finish_with_timestamp(wallet, vdf_output, timestamp_ms)
    }

    fn finish_with_timestamp(
        self,
        wallet: &Wallet,
        vdf_output: String,
        timestamp_ms: u64,
    ) -> Block {
        let leader_proof = self.leader_ticket.as_ref().map(|leader_ticket| {
            let proof_payload = LeaderProofPayload {
                height: self.height,
                prev_hash: self.prev_hash.clone(),
                finalizer_rank: self.finalizer_rank,
                vdf_output: vdf_output.clone(),
                ticket_id: leader_ticket.id.clone(),
                ticket_amount: leader_ticket.amount,
                ticket_owner: leader_ticket.owner.clone(),
            };
            wallet.leader_proof(&proof_payload)
        });
        let mut block = Block {
            height: self.height,
            prev_hash: self.prev_hash,
            timestamp_ms,
            miner: self.miner,
            finalizer_mode: self.finalizer_mode,
            finalizer_rank: self.finalizer_rank,
            reward: self.reward,
            vdf_rounds: self.vdf_rounds,
            vdf_output,
            leader_proof,
            burn_bundle_section: self.burn_bundle_section,
            transactions: self.transactions,
            transactions_v2: self.transactions_v2,
            hash: String::new(),
        };
        block.hash = block.compute_hash();
        block
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainStatus {
    pub height: u64,
    pub tip_hash: String,
    pub finalized_height: Option<u64>,
    pub finalized_hash: Option<String>,
    pub next_leader: Option<String>,
    pub launch_profile_hash: String,
    pub mine_reward: Amount,
    pub current_mine_difficulty_bits: u32,
    pub balances: BTreeMap<String, Amount>,
    pub pending_transactions: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainSnapshot {
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u64,
    pub launch_profile: LaunchProfile,
    pub blocks: Vec<Block>,
}

#[cfg(test)]
mod tests {
    use super::{
        Block, BurnBundleSection, FinalizerMode, LeaderProofPayload, canonical_burn_block_items,
    };
    use crate::domain::{
        BurnTicket, GRINDING_RESISTANCE_ACTIVATION_HEIGHT, PreparedBlock, Transaction, Wallet,
        run_vdf, verify_vdf,
    };

    #[test]
    fn primary_leader_proof_payload_omits_rank_from_canonical_form() {
        let primary = LeaderProofPayload {
            height: 1,
            prev_hash: "prev".to_string(),
            finalizer_rank: 0,
            vdf_output: "vdf".to_string(),
            ticket_id: "ticket".to_string(),
            ticket_amount: 2,
            ticket_owner: "owner".to_string(),
        };
        let fallback = LeaderProofPayload {
            finalizer_rank: 1,
            ..primary.clone()
        };

        assert_eq!(
            primary.canonical(),
            "iuna-leader-proof:1:prev:vdf:ticket:2:owner"
        );
        assert_eq!(
            fallback.canonical(),
            "iuna-leader-proof-v2:1:prev:1:vdf:ticket:2:owner"
        );
    }

    #[test]
    fn burn_block_items_keep_canonical_prefix() {
        assert_eq!(
            canonical_burn_block_items("section"),
            "burn-section-v1:section"
        );
    }

    #[test]
    fn block_compute_hash_sets_content_and_vdf_seed_contract() {
        let mut block = Block {
            height: 1,
            prev_hash: "0".repeat(64),
            timestamp_ms: 1,
            miner: "miner".to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 0,
            vdf_rounds: 1,
            vdf_output: "out".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: vec![Transaction::genesis_burn("owner", 1)],
            transactions_v2: Vec::new(),
            hash: String::new(),
        };

        block.hash = block.compute_hash();

        assert_eq!(block.compute_hash(), block.hash);
    }

    #[test]
    fn post_activation_vdf_binds_transactions_but_allows_ticket_timestamp_completion() {
        let mut block = Block {
            height: GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
            prev_hash: "0".repeat(64),
            timestamp_ms: 1,
            miner: "1".repeat(64),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 16,
            vdf_output: String::new(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: vec![Transaction::genesis_burn("2".repeat(64), 1)],
            transactions_v2: Vec::new(),
            hash: String::new(),
        };
        let original_seed = block.vdf_seed();
        let output = run_vdf(&original_seed, block.vdf_rounds);
        block.vdf_output = output.clone();
        assert!(verify_vdf(&block.vdf_seed(), block.vdf_rounds, &output));

        let mut changed_transactions = block.clone();
        changed_transactions.transactions = vec![Transaction::genesis_burn("3".repeat(64), 1)];
        assert_ne!(changed_transactions.vdf_seed(), original_seed);
        assert!(!verify_vdf(
            &changed_transactions.vdf_seed(),
            changed_transactions.vdf_rounds,
            &output
        ));

        let mut completed_later = block.clone();
        completed_later.timestamp_ms += 1_000;
        assert_eq!(completed_later.vdf_seed(), original_seed);
        assert!(verify_vdf(
            &completed_later.vdf_seed(),
            completed_later.vdf_rounds,
            &output
        ));
    }

    #[test]
    fn transaction_v2_envelopes_are_bound_to_vdf_and_block_hash() {
        let mut block = Block {
            height: GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
            prev_hash: "0".repeat(64),
            timestamp_ms: 1,
            miner: "1".repeat(64),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 1,
            vdf_output: "output".to_string(),
            leader_proof: None,
            burn_bundle_section: BurnBundleSection::default(),
            transactions: vec![Transaction::genesis_burn("2".repeat(64), 1)],
            transactions_v2: vec!["00".to_string()],
            hash: String::new(),
        };
        block.hash = block.compute_hash();
        let original_seed = block.vdf_seed();
        let original_hash = block.hash.clone();

        block.transactions_v2[0] = "01".to_string();

        assert_ne!(block.vdf_seed(), original_seed);
        assert_ne!(block.compute_hash(), original_hash);
    }

    #[test]
    fn prepared_and_finished_blocks_share_post_activation_vdf_commitment() {
        let wallet = Wallet::from_seed("vdf-commitment-finalizer");
        let transactions = vec![Transaction::genesis_burn(wallet.address(), 1)];
        let mut prepared = PreparedBlock {
            height: GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
            prev_hash: "0".repeat(64),
            timestamp_ms: 10,
            miner: wallet.address().to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: 1,
            vdf_rounds: 1,
            vdf_seed: String::new(),
            leader_ticket: Some(BurnTicket {
                id: "4".repeat(64),
                owner: wallet.address().to_string(),
                amount: 1,
                eligible_from_height: GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
                eligible_until_height: GRINDING_RESISTANCE_ACTIVATION_HEIGHT,
            }),
            burn_bundle_section: BurnBundleSection::default(),
            transactions,
            transactions_v2: Vec::new(),
        };
        let bundle_hashes = prepared.burn_bundle_section.burn_bundle_hashes(
            prepared.height,
            &prepared.prev_hash,
            &prepared.miner,
        );
        let commitment = super::vdf_content_commitment(
            prepared.height,
            &prepared.prev_hash,
            &prepared.miner,
            prepared.finalizer_mode,
            prepared.finalizer_rank,
            prepared.reward,
            prepared.vdf_rounds,
            prepared
                .leader_ticket
                .as_ref()
                .map(|ticket| ticket.id.as_str()),
            &prepared.burn_bundle_section,
            &prepared.transactions,
            &prepared.transactions_v2,
        );
        prepared.vdf_seed = super::vdf_seed_for_child(
            &prepared.prev_hash,
            prepared.height,
            &bundle_hashes,
            &commitment,
        );
        let expected_seed = prepared.vdf_seed.clone();
        let output = run_vdf(&expected_seed, prepared.vdf_rounds);

        let block = prepared.finish_at(&wallet, output, 20);

        assert_eq!(block.vdf_seed(), expected_seed);
    }

    #[test]
    fn json_block_size_uses_canonical_node_representation() {
        let compact_wire_json = format!(
            r#"{{"height":1,"prev_hash":"{}","timestamp_ms":1,"miner":"{}","reward":0,"vdf_rounds":1,"vdf_output":"out","leader_proof":null,"transactions":[],"hash":"{}"}}"#,
            "0".repeat(64),
            "1".repeat(64),
            "2".repeat(64)
        );

        let block: Block = serde_json::from_str(&compact_wire_json).unwrap();
        let canonical_json_len = serde_json::to_vec(&block).unwrap().len();

        assert_eq!(block.finalizer_mode, FinalizerMode::Ticket);
        assert_eq!(block.finalizer_rank, 0);
        assert_eq!(block.burn_bundle_section, BurnBundleSection::default());
        assert_eq!(block.json_size_bytes().unwrap(), canonical_json_len);
        assert!(canonical_json_len > compact_wire_json.len());
    }
}
