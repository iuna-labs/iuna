use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Amount = u64;
pub const BLOCK_REWARD: Amount = 100;
pub const VDF_TARGET_BLOCK_MS: u64 = 60_000;
const MAX_PENDING_TRANSACTIONS: usize = 10_000;
const MAX_BLOCK_TRANSACTIONS: usize = 1_000;
const DEFAULT_TICKET_MATURITY_DELAY: u64 = 1;
const MIN_VDF_ROUNDS: u32 = 1;
const VDF_RETARGET_WINDOW_BLOCKS: usize = 10;
const MAX_VDF_RETARGET_STEP_PERCENT: u128 = 10;
const FORK_FINALITY_DEPTH: u64 = 6;
const VDF_MODULUS: u128 = 4_611_685_975_477_714_963;
const VDF_CHALLENGE_MIN: u64 = 1_073_741_827;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wallet {
    address: String,
    secret: String,
}

impl Wallet {
    pub fn from_seed(seed: &str) -> Self {
        let seed_hash = Sha256::digest(format!("mivora-wallet-seed:{seed}").as_bytes());
        let mut signing_seed = [0_u8; 32];
        signing_seed.copy_from_slice(&seed_hash);
        let signing_key = SigningKey::from_bytes(&signing_seed);
        let secret = hex_encode(signing_seed);
        let address = hex_encode(signing_key.verifying_key().to_bytes());
        Self { address, secret }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn burn(&self, amount: Amount, nonce: u64) -> Transaction {
        let unsigned = UnsignedTransaction::Burn {
            from: self.address.clone(),
            amount,
            nonce,
        };
        unsigned.sign(self)
    }

    pub fn transfer(&self, to: impl Into<String>, amount: Amount, nonce: u64) -> Transaction {
        let unsigned = UnsignedTransaction::Transfer {
            from: self.address.clone(),
            to: to.into(),
            amount,
            nonce,
        };
        unsigned.sign(self)
    }

    fn sign_payload(&self, payload: &str) -> String {
        let seed = decode_hex_array::<32>(&self.secret).expect("wallet secret is valid hex");
        let signing_key = SigningKey::from_bytes(&seed);
        let signature: Signature = signing_key.sign(payload.as_bytes());
        hex_encode(signature.to_bytes())
    }

    fn leader_proof(&self, payload: &LeaderProofPayload) -> LeaderProof {
        let signature = self.sign_payload(&payload.canonical());
        LeaderProof {
            ticket_id: payload.ticket_id.clone(),
            public_key: self.address.clone(),
            signature,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsignedTransaction {
    Transfer {
        from: String,
        to: String,
        amount: Amount,
        nonce: u64,
    },
    Burn {
        from: String,
        amount: Amount,
        nonce: u64,
    },
}

impl UnsignedTransaction {
    fn sign(self, wallet: &Wallet) -> Transaction {
        let signature = wallet.sign_payload(&self.canonical());
        match self {
            Self::Transfer {
                from,
                to,
                amount,
                nonce,
            } => Transaction::Transfer {
                from,
                to,
                amount,
                nonce,
                signature,
            },
            Self::Burn {
                from,
                amount,
                nonce,
            } => Transaction::Burn {
                from,
                amount,
                nonce,
                signature,
            },
        }
    }

    fn canonical(&self) -> String {
        match self {
            Self::Transfer {
                from,
                to,
                amount,
                nonce,
            } => format!("transfer:{from}:{to}:{amount}:{nonce}"),
            Self::Burn {
                from,
                amount,
                nonce,
            } => format!("burn:{from}:{amount}:{nonce}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transaction {
    Transfer {
        from: String,
        to: String,
        amount: Amount,
        nonce: u64,
        signature: String,
    },
    Burn {
        from: String,
        amount: Amount,
        nonce: u64,
        signature: String,
    },
}

impl Transaction {
    pub fn genesis_burn(from: impl Into<String>, amount: Amount) -> Self {
        let from = from.into();
        let signature = hex_hash(format!("mivora-genesis-burn:{from}:{amount}"));
        Self::Burn {
            from,
            amount,
            nonce: 0,
            signature,
        }
    }

    pub fn sender(&self) -> &str {
        match self {
            Self::Transfer { from, .. } | Self::Burn { from, .. } => from,
        }
    }

    pub fn nonce(&self) -> u64 {
        match self {
            Self::Transfer { nonce, .. } | Self::Burn { nonce, .. } => *nonce,
        }
    }

    pub fn amount(&self) -> Amount {
        match self {
            Self::Transfer { amount, .. } | Self::Burn { amount, .. } => *amount,
        }
    }

    pub fn signature(&self) -> &str {
        match self {
            Self::Transfer { signature, .. } | Self::Burn { signature, .. } => signature,
        }
    }

    pub fn is_burn(&self) -> bool {
        matches!(self, Self::Burn { .. })
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", self.signing_payload(), self.signature())
    }

    fn signing_payload(&self) -> String {
        match self {
            Self::Transfer {
                from,
                to,
                amount,
                nonce,
                ..
            } => format!("transfer:{from}:{to}:{amount}:{nonce}"),
            Self::Burn {
                from,
                amount,
                nonce,
                ..
            } => format!("burn:{from}:{amount}:{nonce}"),
        }
    }

    fn verify_signature(&self) -> Result<()> {
        let public_key = decode_hex_array::<32>(self.sender())
            .with_context(|| format!("invalid public key for {}", self.sender()))?;
        let signature =
            decode_hex_array::<64>(self.signature()).context("invalid signature hex")?;
        let verifying_key =
            VerifyingKey::from_bytes(&public_key).context("invalid transaction public key")?;
        let signature = Signature::from_bytes(&signature);
        verifying_key
            .verify(self.signing_payload().as_bytes(), &signature)
            .context("transaction signature is invalid")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Block {
    pub height: u64,
    pub prev_hash: String,
    pub timestamp_ms: u64,
    pub miner: String,
    pub reward: Amount,
    pub vdf_rounds: u32,
    pub vdf_output: String,
    pub leader_proof: Option<LeaderProof>,
    pub transactions: Vec<Transaction>,
    pub hash: String,
}

impl Block {
    fn new(draft: BlockDraft) -> Self {
        let mut block = Self {
            height: draft.height,
            prev_hash: draft.prev_hash,
            timestamp_ms: draft.timestamp_ms,
            miner: draft.miner,
            reward: draft.reward,
            vdf_rounds: draft.vdf_rounds,
            vdf_output: draft.vdf_output,
            leader_proof: draft.leader_proof,
            transactions: draft.transactions,
            hash: String::new(),
        };
        block.hash = block.compute_hash();
        block
    }

    pub fn compute_hash(&self) -> String {
        hex_hash(format!(
            "block:{}:{}:{}",
            self.content_hash(),
            self.vdf_seed(),
            self.vdf_output,
        ))
    }

    pub fn vdf_seed(&self) -> String {
        vdf_seed_for_child(&self.prev_hash, self.height)
    }

    fn content_hash(&self) -> String {
        let txs = self
            .transactions
            .iter()
            .map(Transaction::canonical)
            .collect::<Vec<_>>()
            .join("|");
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
        hex_hash(format!(
            "block-content:{}:{}:{}:{}:{}:{}:{}:{}",
            self.height,
            self.prev_hash,
            self.timestamp_ms,
            self.miner,
            self.reward,
            self.vdf_rounds,
            leader_proof,
            txs
        ))
    }

    fn leader_score(&self) -> LeaderScore {
        LeaderScore(
            self.leader_proof
                .as_ref()
                .map(LeaderProof::rank)
                .unwrap_or_else(|| self.hash.clone()),
        )
    }
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
            "mivora-leader-rank:{}:{}",
            self.ticket_id, self.signature
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LeaderProofPayload {
    height: u64,
    prev_hash: String,
    vdf_output: String,
    ticket_id: String,
    ticket_amount: Amount,
    ticket_owner: String,
}

impl LeaderProofPayload {
    fn canonical(&self) -> String {
        format!(
            "mivora-leader-proof:{}:{}:{}:{}:{}:{}",
            self.height,
            self.prev_hash,
            self.vdf_output,
            self.ticket_id,
            self.ticket_amount,
            self.ticket_owner
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BurnTicket {
    id: String,
    owner: String,
    amount: Amount,
    target_height: u64,
    maturity_height: u64,
}

#[derive(Clone, Debug)]
pub struct PreparedBlock {
    height: u64,
    prev_hash: String,
    timestamp_ms: u64,
    miner: String,
    reward: Amount,
    vdf_rounds: u32,
    vdf_seed: String,
    leader_ticket: BurnTicket,
    transactions: Vec<Transaction>,
}

impl PreparedBlock {
    pub fn vdf_seed(&self) -> &str {
        &self.vdf_seed
    }

    pub fn vdf_rounds(&self) -> u32 {
        self.vdf_rounds
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    pub fn finish(self, wallet: &Wallet, vdf_output: String) -> Block {
        let proof_payload = LeaderProofPayload {
            height: self.height,
            prev_hash: self.prev_hash.clone(),
            vdf_output: vdf_output.clone(),
            ticket_id: self.leader_ticket.id.clone(),
            ticket_amount: self.leader_ticket.amount,
            ticket_owner: self.leader_ticket.owner.clone(),
        };
        Block::new(BlockDraft {
            height: self.height,
            prev_hash: self.prev_hash,
            timestamp_ms: self.timestamp_ms,
            miner: self.miner,
            reward: self.reward,
            vdf_rounds: self.vdf_rounds,
            vdf_output,
            leader_proof: Some(wallet.leader_proof(&proof_payload)),
            transactions: self.transactions,
        })
    }
}

#[derive(Clone, Debug)]
struct BlockDraft {
    height: u64,
    prev_hash: String,
    timestamp_ms: u64,
    miner: String,
    reward: Amount,
    vdf_rounds: u32,
    vdf_output: String,
    leader_proof: Option<LeaderProof>,
    transactions: Vec<Transaction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainStatus {
    pub height: u64,
    pub tip_hash: String,
    pub next_leader: Option<String>,
    pub launch_profile_hash: String,
    pub block_reward: Amount,
    pub balances: BTreeMap<String, Amount>,
    pub pending_transactions: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainSnapshot {
    pub genesis_allocations: BTreeMap<String, Amount>,
    pub vdf_rounds: u32,
    pub launch_profile: LaunchProfile,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchProfile {
    pub profile_id: String,
    pub ticket_maturity_delay_heights: u64,
    pub max_pending_transactions: usize,
    pub max_block_transactions: usize,
}

impl Default for LaunchProfile {
    fn default() -> Self {
        Self {
            profile_id: "mivora-devnet-v1".to_string(),
            ticket_maturity_delay_heights: DEFAULT_TICKET_MATURITY_DELAY,
            max_pending_transactions: MAX_PENDING_TRANSACTIONS,
            max_block_transactions: MAX_BLOCK_TRANSACTIONS,
        }
    }
}

impl LaunchProfile {
    pub fn hash(&self) -> String {
        hex_hash(format!(
            "mivora-launch-profile:{}:{}:{}:{}",
            self.profile_id,
            self.ticket_maturity_delay_heights,
            self.max_pending_transactions,
            self.max_block_transactions
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenesisBurn {
    pub from: String,
    pub amount: Amount,
}

impl GenesisBurn {
    pub fn new(from: impl Into<String>, amount: Amount) -> Self {
        Self {
            from: from.into(),
            amount,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ForkPoint {
    common_ancestor_height: u64,
}

impl ForkPoint {
    fn first_diverging_height(self) -> u64 {
        self.common_ancestor_height + 1
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LeaderScore(String);

impl Ord for LeaderScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for LeaderScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkQuality {
    LocalBetter,
    RemoteBetter,
    Equal,
}

impl From<std::cmp::Ordering> for ForkQuality {
    fn from(ordering: std::cmp::Ordering) -> Self {
        match ordering {
            std::cmp::Ordering::Less => Self::LocalBetter,
            std::cmp::Ordering::Equal => Self::Equal,
            std::cmp::Ordering::Greater => Self::RemoteBetter,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkChoice {
    KeepLocal,
    SwitchToCandidate,
}

#[derive(Clone, Debug)]
pub struct Ledger {
    chain: Vec<Block>,
    genesis_allocations: BTreeMap<String, Amount>,
    balances: BTreeMap<String, Amount>,
    nonces: BTreeMap<String, u64>,
    tickets: Vec<BurnTicket>,
    pending: Vec<Transaction>,
    block_reward: Amount,
    initial_vdf_rounds: u32,
    vdf_rounds: u32,
    launch_profile: LaunchProfile,
}

impl Ledger {
    pub fn new(genesis_allocations: BTreeMap<String, Amount>, vdf_rounds: u32) -> Self {
        Self::new_with_genesis_transactions(genesis_allocations, Vec::new(), vdf_rounds)
            .expect("empty genesis transactions are valid")
    }

    pub fn new_with_genesis_burns(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_burns: Vec<GenesisBurn>,
        vdf_rounds: u32,
    ) -> Result<Self> {
        let transactions = genesis_burns
            .into_iter()
            .map(|burn| Transaction::genesis_burn(burn.from, burn.amount))
            .collect();
        Self::new_with_genesis_transactions(genesis_allocations, transactions, vdf_rounds)
    }

    fn new_with_genesis_transactions(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_transactions: Vec<Transaction>,
        vdf_rounds: u32,
    ) -> Result<Self> {
        let launch_profile = LaunchProfile::default();
        let balances = balances_after_genesis(&genesis_allocations, &genesis_transactions)?;
        let genesis = build_genesis_block(&genesis_allocations, genesis_transactions);
        let tickets = genesis_tickets(&genesis_allocations, &genesis, &launch_profile)?;
        Ok(Self {
            chain: vec![genesis],
            genesis_allocations: genesis_allocations.clone(),
            balances,
            nonces: BTreeMap::new(),
            tickets,
            pending: Vec::new(),
            block_reward: BLOCK_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
        })
    }

    pub fn from_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_with_vdf_policy(snapshot, true)
    }

    fn from_snapshot_with_vdf_policy(snapshot: ChainSnapshot, verify_vdf: bool) -> Result<Self> {
        let ChainSnapshot {
            genesis_allocations,
            vdf_rounds,
            launch_profile,
            blocks,
        } = snapshot;

        if blocks.is_empty() {
            bail!("chain snapshot is empty");
        }

        let genesis = blocks[0].clone();
        validate_genesis_block(&genesis)?;
        let balances = balances_after_genesis(&genesis_allocations, &genesis.transactions)?;
        let expected_genesis =
            build_genesis_block(&genesis_allocations, genesis.transactions.clone());
        if genesis != expected_genesis {
            bail!("chain snapshot genesis does not match its allocations and transactions");
        }

        let mut ledger = Self {
            chain: vec![genesis],
            genesis_allocations,
            balances,
            nonces: BTreeMap::new(),
            tickets: Vec::new(),
            pending: Vec::new(),
            block_reward: BLOCK_REWARD,
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
                ledger.apply_block(block)?;
            } else {
                ledger.apply_preverified_block(block)?;
            }
        }
        Ok(ledger)
    }

    pub fn extend_from_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, true)
    }

    pub(crate) fn extend_from_preverified_snapshot(
        &mut self,
        snapshot: ChainSnapshot,
    ) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, false)
    }

    pub(crate) fn missing_snapshot_blocks(&self, snapshot: &ChainSnapshot) -> Result<Vec<Block>> {
        let remote_height = self.validate_snapshot_identity(snapshot)?;
        if remote_height <= self.height() {
            return Ok(Vec::new());
        }
        let common_ancestor_height = self.common_ancestor_height(snapshot)?;

        Ok(snapshot
            .blocks
            .iter()
            .skip(common_ancestor_height as usize + 1)
            .cloned()
            .collect())
    }

    fn extend_from_snapshot_with_vdf_policy(
        &mut self,
        snapshot: ChainSnapshot,
        verify_vdf: bool,
    ) -> Result<bool> {
        self.validate_snapshot_identity(&snapshot)?;
        let candidate = Self::from_snapshot_with_vdf_policy(snapshot, verify_vdf)?;
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

    fn common_ancestor_height(&self, snapshot: &ChainSnapshot) -> Result<u64> {
        self.validate_snapshot_identity(snapshot)?;
        let max_common_index = self.chain.len().min(snapshot.blocks.len()) - 1;
        for index in 0..=max_common_index {
            if self.chain[index] != snapshot.blocks[index] {
                if index == 0 {
                    bail!("chain snapshot has no common genesis block");
                }
                return Ok(index as u64 - 1);
            }
        }
        Ok(max_common_index as u64)
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

        let finalized_floor = local_height.saturating_sub(FORK_FINALITY_DEPTH);
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
        for block in self
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize)
        {
            carry_forward.extend(block.transactions.clone());
        }

        let mined_signatures = candidate
            .chain
            .iter()
            .flat_map(|block| block.transactions.iter())
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();

        for transaction in carry_forward {
            if !mined_signatures.contains(transaction.signature()) {
                let _ = candidate.submit_transaction(transaction);
            }
        }

        *self = candidate;
    }

    pub fn snapshot(&self) -> ChainSnapshot {
        ChainSnapshot {
            genesis_allocations: self.genesis_allocations.clone(),
            vdf_rounds: self.initial_vdf_rounds,
            launch_profile: self.launch_profile.clone(),
            blocks: self.chain.clone(),
        }
    }

    pub fn status(&self) -> ChainStatus {
        ChainStatus {
            height: self.tip().height,
            tip_hash: self.tip().hash.clone(),
            next_leader: self.expected_leader_for_next_block(),
            launch_profile_hash: self.launch_profile.hash(),
            block_reward: self.block_reward,
            balances: self.balances.clone(),
            pending_transactions: self.pending.len(),
        }
    }

    pub fn chain(&self) -> &[Block] {
        &self.chain
    }

    pub fn genesis_hash(&self) -> &str {
        &self.chain[0].hash
    }

    pub fn height(&self) -> u64 {
        self.tip().height
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<Block> {
        self.chain.iter().rev().take(limit).cloned().collect()
    }

    pub fn blocks_before(&self, before_height: u64, limit: usize) -> Vec<Block> {
        self.chain
            .iter()
            .rev()
            .filter(|block| block.height < before_height)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn blocks_from(&self, from_height: u64, limit: usize) -> Vec<Block> {
        if limit == 0 {
            return Vec::new();
        }
        self.chain
            .iter()
            .filter(|block| block.height >= from_height)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn block_by_hash(&self, hash: &str) -> Option<Block> {
        self.chain.iter().find(|block| block.hash == hash).cloned()
    }

    pub fn has_block(&self, hash: &str) -> bool {
        self.chain.iter().any(|block| block.hash == hash)
    }

    pub fn pending(&self) -> &[Transaction] {
        &self.pending
    }

    pub fn transaction_by_signature(&self, signature: &str) -> Option<Transaction> {
        self.pending
            .iter()
            .chain(
                self.chain
                    .iter()
                    .flat_map(|block| block.transactions.iter()),
            )
            .find(|tx| tx.signature() == signature)
            .cloned()
    }

    pub fn has_transaction(&self, signature: &str) -> bool {
        self.transaction_by_signature(signature).is_some()
    }

    pub fn vdf_rounds(&self) -> u32 {
        self.vdf_rounds
    }

    pub fn launch_profile(&self) -> &LaunchProfile {
        &self.launch_profile
    }

    pub fn balance_of(&self, address: &str) -> Amount {
        self.balances.get(address).copied().unwrap_or(0)
    }

    pub fn next_nonce(&self, address: &str) -> u64 {
        let base = self.nonces.get(address).copied().unwrap_or(0);
        let Some(mut next) = base.checked_add(1) else {
            return u64::MAX;
        };
        while self
            .pending
            .iter()
            .any(|tx| tx.sender() == address && tx.nonce() == next)
        {
            let Some(candidate) = next.checked_add(1) else {
                return u64::MAX;
            };
            next = candidate;
        }
        next
    }

    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<bool> {
        if self
            .pending
            .iter()
            .any(|tx| tx.signature() == transaction.signature())
        {
            return Ok(false);
        }

        transaction.verify_signature()?;

        if self
            .pending
            .iter()
            .any(|tx| tx.sender() == transaction.sender() && tx.nonce() == transaction.nonce())
        {
            return Ok(false);
        }

        if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("mempool is full");
        }

        let mut balances = self.balances.clone();
        let mut nonces = self.nonces.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut balances, &mut nonces)?;
        }

        let expected_nonce = next_expected_nonce(&nonces, transaction.sender())?;
        if transaction.nonce() < expected_nonce {
            return Ok(false);
        }
        if transaction.nonce() > expected_nonce {
            self.pending.push(transaction);
            return Ok(true);
        }

        apply_transaction(&transaction, &mut balances, &mut nonces)?;
        self.pending.push(transaction);
        Ok(true)
    }

    pub fn mine_next_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<Block> {
        let prepared = self.prepare_next_block(wallet.address(), timestamp_ms)?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn prepare_next_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        let Some(leader_ticket) = self.selected_ticket_for_height(height) else {
            bail!("cannot mine block without a mature burn ticket");
        };
        if let Some(leader) = self.expected_leader_for_next_block() {
            if leader != miner {
                bail!("wallet {miner} is not the selected leader; expected {leader}");
            }
        } else {
            bail!("no selected leader for block {height}");
        }

        let transactions = self
            .valid_pending_transactions()
            .into_iter()
            .take(self.launch_profile.max_block_transactions)
            .collect::<Vec<_>>();
        ensure_block_has_burn(&transactions)?;

        let tip = self.tip();
        let prev_hash = tip.hash.clone();
        let timestamp_ms = timestamp_ms.max(tip.timestamp_ms + 1);
        let vdf_seed = vdf_seed_for_child(&prev_hash, height);
        Ok(PreparedBlock {
            height,
            prev_hash,
            timestamp_ms,
            miner: miner.to_string(),
            reward: self.block_reward,
            vdf_rounds: self.vdf_rounds,
            vdf_seed,
            leader_ticket,
            transactions,
        })
    }

    pub fn apply_block(&mut self, block: Block) -> Result<()> {
        self.apply_block_with_vdf_policy(block, true)
    }

    pub(crate) fn block_requires_vdf_verification(&self, block: &Block) -> Result<bool> {
        self.precheck_block_without_vdf(block)
    }

    pub fn apply_locally_mined_block(&mut self, block: Block) -> Result<()> {
        self.apply_preverified_block(block)
    }

    pub(crate) fn apply_preverified_block(&mut self, block: Block) -> Result<()> {
        self.apply_block_with_vdf_policy(block, false)
    }

    fn apply_block_with_vdf_policy(&mut self, block: Block, should_verify_vdf: bool) -> Result<()> {
        if !self.precheck_block_without_vdf(&block)? {
            return Ok(());
        }

        if should_verify_vdf && !verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output)
        {
            bail!("block VDF output is invalid");
        }

        let mut balances = self.balances.clone();
        let mut nonces = self.nonces.clone();
        let mut signatures = BTreeSet::new();
        for tx in &block.transactions {
            if !signatures.insert(tx.signature()) {
                bail!("duplicate transaction in block");
            }
            apply_transaction(tx, &mut balances, &mut nonces)?;
        }
        let mut tickets = self.tickets.clone();
        consume_leader_ticket(&block, &mut tickets)?;
        credit_balance(&mut balances, &block.miner, block.reward)?;
        tickets.extend(tickets_created_by_block(&block, &self.launch_profile)?);

        let mined_signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        self.balances = balances;
        self.nonces = nonces;
        self.tickets = tickets;
        self.pending.retain(|tx| {
            !mined_signatures.contains(tx.signature())
                && tx.nonce() > self.nonces.get(tx.sender()).copied().unwrap_or(0)
        });
        self.chain.push(block);
        self.vdf_rounds = self.next_vdf_rounds_after_tip();
        Ok(())
    }

    fn precheck_block_without_vdf(&self, block: &Block) -> Result<bool> {
        if block.height <= self.tip().height {
            let existing = self
                .chain
                .get(block.height as usize)
                .with_context(|| format!("local chain has no block at height {}", block.height))?;
            if existing.hash == block.hash {
                return Ok(false);
            }
            bail!(
                "block at height {} conflicts with local chain",
                block.height
            );
        }

        let expected_height = self.tip().height + 1;
        if block.height != expected_height {
            bail!(
                "expected block height {expected_height}, got {}",
                block.height
            );
        }
        if block.prev_hash != self.tip().hash {
            bail!("block does not extend local tip");
        }
        if block.compute_hash() != block.hash {
            bail!("block hash is invalid");
        }
        if block.reward != self.block_reward {
            bail!("block reward is invalid");
        }
        if block.vdf_rounds != self.vdf_rounds {
            bail!("block VDF rounds are invalid");
        }
        if block.timestamp_ms <= self.tip().timestamp_ms {
            bail!("block timestamp must increase");
        }
        if block.transactions.len() > self.launch_profile.max_block_transactions {
            bail!("block has too many transactions");
        }
        ensure_block_has_burn(&block.transactions)?;
        let Some(leader) = self.expected_leader_for_next_block() else {
            bail!("no selected leader for block {}", block.height);
        };
        if leader != block.miner {
            bail!(
                "block miner {} is not selected leader {leader}",
                block.miner
            );
        }
        let selected_ticket = self
            .selected_ticket_for_height(block.height)
            .context("no selected ticket for leader block")?;
        if block
            .leader_proof
            .as_ref()
            .is_none_or(|proof| proof.ticket_id != selected_ticket.id)
        {
            bail!("block does not prove the selected leader ticket");
        }
        verify_leader_proof(block, &self.tickets)?;

        Ok(true)
    }

    fn next_vdf_rounds_after_tip(&self) -> u32 {
        let Some(tip) = self.chain.last() else {
            return self.vdf_rounds;
        };
        if tip.height < 2 {
            return self.vdf_rounds;
        }

        let mut total_observed_ms = 0_u128;
        let mut observed_blocks = 0_u128;
        for pair in self.chain.windows(2).rev().take(VDF_RETARGET_WINDOW_BLOCKS) {
            total_observed_ms += u128::from(pair[1].timestamp_ms - pair[0].timestamp_ms);
            observed_blocks += 1;
        }
        if observed_blocks == 0 {
            return self.vdf_rounds;
        }

        let average_observed_ms = (total_observed_ms / observed_blocks) as u64;
        retarget_vdf_rounds(tip.vdf_rounds, average_observed_ms)
    }

    pub fn expected_leader_for_next_block(&self) -> Option<String> {
        self.selected_ticket_for_height(self.tip().height + 1)
            .map(|ticket| ticket.owner)
    }

    fn valid_pending_transactions(&self) -> Vec<Transaction> {
        let mut balances = self.balances.clone();
        let mut nonces = self.nonces.clone();
        let mut valid = Vec::new();
        let mut remaining = self.pending.iter().collect::<Vec<_>>();

        while !remaining.is_empty() {
            let mut progressed = false;
            let mut still_pending = Vec::new();

            for tx in remaining {
                if apply_transaction(tx, &mut balances, &mut nonces).is_ok() {
                    valid.push(tx.clone());
                    progressed = true;
                } else {
                    still_pending.push(tx);
                }
            }

            if !progressed {
                break;
            }

            remaining = still_pending;
        }

        valid
    }

    fn selected_ticket_for_height(&self, height: u64) -> Option<BurnTicket> {
        self.tickets
            .iter()
            .filter(|ticket| ticket.target_height == height && ticket.maturity_height <= height)
            .min_by(|left, right| {
                ticket_rank(self.tip(), height, left)
                    .cmp(&ticket_rank(self.tip(), height, right))
                    .then_with(|| left.id.cmp(&right.id))
            })
            .cloned()
    }

    fn tip(&self) -> &Block {
        self.chain
            .last()
            .expect("ledger is always initialized with genesis")
    }
}

fn ticket_rank(parent: &Block, target_height: u64, ticket: &BurnTicket) -> String {
    hex_hash(format!(
        "mivora-ticket-rank:{}:{}:{}:{}:{}",
        target_height, parent.hash, parent.vdf_output, ticket.id, ticket.amount
    ))
}

fn tickets_created_by_block(block: &Block, profile: &LaunchProfile) -> Result<Vec<BurnTicket>> {
    let mut tickets = Vec::new();
    for tx in &block.transactions {
        let Transaction::Burn {
            from,
            amount,
            signature,
            ..
        } = tx
        else {
            continue;
        };
        if *amount == 0 {
            continue;
        }
        let target_height = block
            .height
            .checked_add(profile.ticket_maturity_delay_heights)
            .with_context(|| format!("ticket target height overflow at block {}", block.height))?;
        let maturity_height = target_height;
        tickets.push(BurnTicket {
            id: signature.clone(),
            owner: from.clone(),
            amount: *amount,
            target_height,
            maturity_height,
        });
    }
    Ok(tickets)
}

fn genesis_tickets(
    genesis_allocations: &BTreeMap<String, Amount>,
    genesis: &Block,
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    let tickets = tickets_created_by_block(genesis, profile)?;
    if !tickets.is_empty() {
        return Ok(tickets);
    }

    let Some((owner, amount)) = genesis_allocations.iter().find(|(_, amount)| **amount > 0) else {
        return Ok(Vec::new());
    };
    Ok(vec![BurnTicket {
        id: hex_hash(format!(
            "mivora-genesis-ticket:{owner}:{amount}:{}",
            genesis.hash
        )),
        owner: owner.clone(),
        amount: 1,
        target_height: profile.ticket_maturity_delay_heights,
        maturity_height: profile.ticket_maturity_delay_heights,
    }])
}

fn consume_leader_ticket(block: &Block, tickets: &mut Vec<BurnTicket>) -> Result<()> {
    let Some(proof) = &block.leader_proof else {
        bail!("block is missing leader proof");
    };
    let Some(index) = tickets
        .iter()
        .position(|ticket| ticket.id == proof.ticket_id && ticket.target_height == block.height)
    else {
        bail!("leader ticket is not pending for block {}", block.height);
    };
    tickets.remove(index);
    Ok(())
}

fn ensure_block_has_burn(transactions: &[Transaction]) -> Result<()> {
    if !transactions.iter().any(Transaction::is_burn) {
        bail!("block must include at least one burn transaction");
    }
    Ok(())
}

fn verify_leader_proof(block: &Block, tickets: &[BurnTicket]) -> Result<()> {
    let Some(proof) = &block.leader_proof else {
        bail!("block is missing leader proof");
    };
    if proof.public_key != block.miner {
        bail!("leader proof public key does not match block miner");
    }
    let ticket = tickets
        .iter()
        .find(|ticket| ticket.id == proof.ticket_id && ticket.target_height == block.height)
        .context("leader ticket is not pending for this height")?;
    if ticket.owner != block.miner {
        bail!("leader ticket owner does not match block miner");
    }
    if ticket.maturity_height > block.height {
        bail!("leader ticket is not mature");
    }

    let payload = LeaderProofPayload {
        height: block.height,
        prev_hash: block.prev_hash.clone(),
        vdf_output: block.vdf_output.clone(),
        ticket_id: ticket.id.clone(),
        ticket_amount: ticket.amount,
        ticket_owner: ticket.owner.clone(),
    };
    verify_leader_signature(proof, &payload)?;
    Ok(())
}

fn verify_leader_signature(proof: &LeaderProof, payload: &LeaderProofPayload) -> Result<()> {
    let public_key = decode_hex_array::<32>(&proof.public_key)
        .with_context(|| format!("invalid leader public key {}", proof.public_key))?;
    let signature =
        decode_hex_array::<64>(&proof.signature).context("invalid leader signature hex")?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("invalid leader public key")?;
    let signature = Signature::from_bytes(&signature);
    verifying_key
        .verify(payload.canonical().as_bytes(), &signature)
        .context("leader signature is invalid")
}

fn vdf_seed_for_child(prev_hash: &str, height: u64) -> String {
    hex_hash(format!("mivora-vdf-child:{prev_hash}:{height}"))
}

fn apply_transaction(
    transaction: &Transaction,
    balances: &mut BTreeMap<String, Amount>,
    nonces: &mut BTreeMap<String, u64>,
) -> Result<()> {
    transaction.verify_signature()?;

    let from = transaction.sender();
    let expected_nonce = next_expected_nonce(nonces, from)?;
    if transaction.nonce() != expected_nonce {
        bail!(
            "invalid nonce for {from}: expected {expected_nonce}, got {}",
            transaction.nonce()
        );
    }
    debit_balance(balances, from, transaction.amount())?;
    match transaction {
        Transaction::Transfer { to, amount, .. } => {
            credit_balance(balances, to, *amount)?;
        }
        Transaction::Burn { .. } => {}
    }
    nonces.insert(from.to_string(), transaction.nonce());
    Ok(())
}

fn next_expected_nonce(nonces: &BTreeMap<String, u64>, address: &str) -> Result<u64> {
    nonces
        .get(address)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .with_context(|| format!("nonce space exhausted for {address}"))
}

fn debit_balance(
    balances: &mut BTreeMap<String, Amount>,
    address: &str,
    amount: Amount,
) -> Result<()> {
    let balance = balances.entry(address.to_string()).or_insert(0);
    if *balance < amount {
        bail!("insufficient funds for {address}");
    }
    *balance -= amount;
    Ok(())
}

fn credit_balance(
    balances: &mut BTreeMap<String, Amount>,
    address: &str,
    amount: Amount,
) -> Result<()> {
    let balance = balances.entry(address.to_string()).or_insert(0);
    *balance = balance
        .checked_add(amount)
        .with_context(|| format!("balance overflow for {address}"))?;
    Ok(())
}

fn build_genesis_block(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: Vec<Transaction>,
) -> Block {
    let miner = genesis_miner(genesis_allocations, &transactions);
    let reward = genesis_reward(genesis_allocations, &transactions);
    let txs = transactions
        .iter()
        .map(Transaction::canonical)
        .collect::<Vec<_>>()
        .join("|");
    let vdf_output = hex_hash(format!("mivora-genesis-vdf:{genesis_allocations:?}:{txs}"));
    let mut genesis = Block {
        height: 0,
        prev_hash: "0".repeat(64),
        timestamp_ms: 0,
        miner,
        reward,
        vdf_rounds: 0,
        vdf_output,
        leader_proof: None,
        transactions,
        hash: String::new(),
    };
    genesis.hash = genesis.compute_hash();
    genesis
}

fn validate_genesis_block(block: &Block) -> Result<()> {
    if block.height != 0 {
        bail!("genesis block height must be 0");
    }
    if block.prev_hash != "0".repeat(64) {
        bail!("genesis block prev_hash must be all zeroes");
    }
    if block.timestamp_ms != 0 {
        bail!("genesis block timestamp must be 0");
    }
    if block.miner == "genesis" && block.reward != 0 {
        bail!("genesis placeholder miner must not receive a reward");
    }
    if block.miner != "genesis" && block.reward != 0 && block.reward != BLOCK_REWARD {
        bail!("genesis block reward is invalid");
    }
    if block.vdf_rounds != 0 {
        bail!("genesis block VDF rounds must be 0");
    }
    if block.leader_proof.is_some() {
        bail!("genesis block must not carry a leader proof");
    }
    if block.compute_hash() != block.hash {
        bail!("genesis block hash is invalid");
    }
    Ok(())
}

fn genesis_miner(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: &[Transaction],
) -> String {
    transactions
        .iter()
        .filter_map(|transaction| match transaction {
            Transaction::Burn { from, .. } => Some(from.as_str()),
            Transaction::Transfer { .. } => None,
        })
        .find(|from| genesis_allocations.contains_key(*from))
        .or_else(|| genesis_allocations.keys().next().map(String::as_str))
        .unwrap_or("genesis")
        .to_string()
}

fn genesis_reward(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: &[Transaction],
) -> Amount {
    if genesis_allocations.is_empty() || transactions.is_empty() {
        0
    } else {
        BLOCK_REWARD
    }
}

fn balances_after_genesis(
    genesis_allocations: &BTreeMap<String, Amount>,
    transactions: &[Transaction],
) -> Result<BTreeMap<String, Amount>> {
    let mut balances = genesis_allocations.clone();
    for transaction in transactions {
        match transaction {
            Transaction::Burn { from, amount, .. } => {
                let balance = balances.entry(from.clone()).or_insert(0);
                if *balance < *amount {
                    bail!("genesis burn exceeds allocation for {from}");
                }
                *balance -= *amount;
            }
            Transaction::Transfer { .. } => bail!("genesis only supports burn transactions"),
        }
    }
    let reward = genesis_reward(genesis_allocations, transactions);
    if reward > 0 {
        let miner = genesis_miner(genesis_allocations, transactions);
        credit_balance(&mut balances, &miner, reward)?;
    }
    Ok(balances)
}

pub fn run_vdf(seed: &str, rounds: u32) -> String {
    let x = vdf_seed_element(seed);
    let mut y = x;
    for _ in 0..rounds {
        y = mul_mod(y, y);
    }

    let challenge = vdf_challenge_prime(seed, rounds, y);
    let proof = vdf_proof(x, rounds, challenge);
    encode_vdf_solution(y, proof)
}

pub fn verify_vdf(seed: &str, rounds: u32, solution: &str) -> bool {
    let Some((y, proof)) = decode_vdf_solution(solution) else {
        return false;
    };
    if y == 0 || y >= VDF_MODULUS || proof >= VDF_MODULUS {
        return false;
    }

    let x = vdf_seed_element(seed);
    let challenge = vdf_challenge_prime(seed, rounds, y);
    let remainder = pow_mod_small(2, rounds, challenge) as u128;
    let verified = mul_mod(mod_pow(proof, challenge as u128), mod_pow(x, remainder));
    verified == y
}

fn vdf_seed_element(seed: &str) -> u128 {
    let digest = Sha256::digest(format!("mivora-vdf-seed:{seed}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    2 + (u128::from_be_bytes(bytes) % (VDF_MODULUS - 3))
}

fn vdf_challenge_prime(seed: &str, rounds: u32, output: u128) -> u64 {
    let digest = Sha256::digest(format!("mivora-vdf-challenge:{seed}:{rounds}:{output:x}"));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let candidate = VDF_CHALLENGE_MIN + (u64::from_be_bytes(bytes) % VDF_CHALLENGE_MIN);
    next_odd_prime(candidate | 1)
}

fn vdf_proof(x: u128, rounds: u32, challenge: u64) -> u128 {
    let mut proof = 1_u128;
    let mut remainder = 1_u64 % challenge;
    for _ in 0..rounds {
        let doubled = remainder * 2;
        let carry = doubled >= challenge;
        proof = mul_mod(proof, proof);
        if carry {
            proof = mul_mod(proof, x);
        }
        remainder = doubled % challenge;
    }
    proof
}

fn encode_vdf_solution(output: u128, proof: u128) -> String {
    format!("{output:032x}:{proof:032x}")
}

fn decode_vdf_solution(solution: &str) -> Option<(u128, u128)> {
    let (output, proof) = solution.split_once(':')?;
    if output.len() != 32 || proof.len() != 32 {
        return None;
    }
    Some((
        u128::from_str_radix(output, 16).ok()?,
        u128::from_str_radix(proof, 16).ok()?,
    ))
}

fn mul_mod(left: u128, right: u128) -> u128 {
    (left * right) % VDF_MODULUS
}

fn mod_pow(mut base: u128, mut exponent: u128) -> u128 {
    let mut result = 1_u128;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = mul_mod(result, base);
        }
        base = mul_mod(base, base);
        exponent >>= 1;
    }
    result
}

fn pow_mod_small(base: u64, exponent: u32, modulus: u64) -> u64 {
    let mut result = 1_u128;
    let mut base = u128::from(base % modulus);
    let mut exponent = exponent;
    let modulus = u128::from(modulus);
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = (result * base) % modulus;
        }
        base = (base * base) % modulus;
        exponent >>= 1;
    }
    result as u64
}

fn next_odd_prime(mut candidate: u64) -> u64 {
    while !is_odd_prime(candidate) {
        candidate = candidate.saturating_add(2);
    }
    candidate
}

fn is_odd_prime(candidate: u64) -> bool {
    if candidate < 3 || candidate % 2 == 0 {
        return false;
    }
    let mut divisor = 3_u64;
    while divisor * divisor <= candidate {
        if candidate % divisor == 0 {
            return false;
        }
        divisor += 2;
    }
    true
}

fn retarget_vdf_rounds(current_rounds: u32, observed_block_ms: u64) -> u32 {
    let current = u128::from(current_rounds);
    let observed = u128::from(observed_block_ms.max(1));
    let raw_adjusted = current * u128::from(VDF_TARGET_BLOCK_MS) / observed;
    let max_step = (current * MAX_VDF_RETARGET_STEP_PERCENT / 100).max(1);
    let min_next = current
        .saturating_sub(max_step)
        .max(u128::from(MIN_VDF_ROUNDS));
    let max_next = current.saturating_add(max_step).min(u128::from(u32::MAX));
    raw_adjusted.clamp(min_next, max_next) as u32
}

pub fn hex_hash(input: impl AsRef<[u8]>) -> String {
    hex_encode(Sha256::digest(input.as_ref()))
}

fn decode_hex_array<const N: usize>(input: &str) -> Result<[u8; N]> {
    let bytes = decode_hex(input)?;
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| anyhow!("expected {} hex bytes, got {len}", N))
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }

    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
