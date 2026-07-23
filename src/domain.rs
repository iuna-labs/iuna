use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Amount = u64;
pub const BLOCK_REWARD: Amount = 100;
pub const DEFAULT_TRANSACTION_FEE: Amount = 1;
pub const MAX_BLOCK_BYTES: usize = 100_000;
pub const VDF_TARGET_BLOCK_MS: u64 = 60_000;
const MAX_PENDING_TRANSACTIONS: usize = 10_000;
const MAX_BLOCK_TRANSACTIONS: usize = 1_000;
const DEFAULT_TICKET_MATURITY_DELAY: u64 = 3;
const DEFAULT_TICKET_EXPIRY_WINDOW: u64 = 3;
const MIN_VDF_ROUNDS: u32 = 1;
const VDF_RETARGET_WINDOW_BLOCKS: usize = 10;
const MAX_VDF_RETARGET_STEP_PERCENT: u128 = 10;
const FORK_FINALITY_DEPTH: u64 = 6;
const VDF_MODULUS: u128 = 4_611_685_975_477_714_963;
const VDF_CHALLENGE_MIN: u64 = 1_073_741_827;
const WALLET_SEED_DOMAIN: &str = "luun-wallet-seed";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wallet {
    address: String,
    secret: String,
}

impl Wallet {
    pub fn from_seed(seed: &str) -> Self {
        let seed_hash = Sha256::digest(format!("{WALLET_SEED_DOMAIN}:{seed}").as_bytes());
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

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OutPoint {
    pub txid: String,
    pub index: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TxInput {
    pub outpoint: OutPoint,
    pub owner: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TxOutput {
    pub address: String,
    pub amount: Amount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transaction {
    Transfer {
        inputs: Vec<TxInput>,
        outputs: Vec<TxOutput>,
        #[serde(default)]
        fee: Amount,
        signature: String,
    },
    Burn {
        inputs: Vec<TxInput>,
        change: Vec<TxOutput>,
        amount: Amount,
        #[serde(default)]
        fee: Amount,
        signature: String,
    },
}

impl Transaction {
    pub fn genesis_burn(from: impl Into<String>, amount: Amount) -> Self {
        let from = from.into();
        Self::genesis_burn_with_change(from, amount, Vec::new())
    }

    fn genesis_burn_with_allocation(
        from: impl Into<String>,
        amount: Amount,
        allocation: Amount,
    ) -> Result<Self> {
        if amount > allocation {
            bail!("genesis burn exceeds allocation");
        }
        let from = from.into();
        let change_amount = allocation - amount;
        let change = if change_amount > 0 {
            vec![TxOutput {
                address: from.clone(),
                amount: change_amount,
            }]
        } else {
            Vec::new()
        };
        Ok(Self::genesis_burn_with_change(from, amount, change))
    }

    fn genesis_burn_with_change(from: String, amount: Amount, change: Vec<TxOutput>) -> Self {
        let input = TxInput {
            outpoint: genesis_allocation_outpoint(&from),
            owner: from.clone(),
            signature: "genesis".to_string(),
        };
        let unsigned = UnsignedUtxoTransaction::Burn {
            inputs: vec![input.without_signature()],
            change: change.clone(),
            amount,
            fee: 0,
        };
        let signature = hex_hash(format!("luun-genesis-burn:{}", unsigned.canonical()));
        Self::Burn {
            inputs: vec![input],
            change,
            amount,
            fee: 0,
            signature,
        }
    }

    pub fn sender(&self) -> &str {
        match self {
            Self::Transfer { inputs, .. } | Self::Burn { inputs, .. } => inputs
                .first()
                .map(|input| input.owner.as_str())
                .unwrap_or(""),
        }
    }

    pub fn to(&self) -> Option<&str> {
        match self {
            Self::Transfer { outputs, .. } => outputs.first().map(|output| output.address.as_str()),
            Self::Burn { .. } => None,
        }
    }

    pub fn amount(&self) -> Amount {
        match self {
            Self::Transfer { outputs, .. } => {
                outputs.first().map(|output| output.amount).unwrap_or(0)
            }
            Self::Burn { amount, .. } => *amount,
        }
    }

    pub fn fee(&self) -> Amount {
        match self {
            Self::Transfer { fee, .. } | Self::Burn { fee, .. } => *fee,
        }
    }

    pub fn total_debit(&self) -> Result<Amount> {
        self.amount()
            .checked_add(self.fee())
            .context("transaction amount plus fee overflows")
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
                inputs,
                outputs,
                fee,
                ..
            } => UnsignedUtxoTransaction::Transfer {
                inputs: unsigned_inputs(inputs),
                outputs: outputs.clone(),
                fee: *fee,
            }
            .canonical(),
            Self::Burn {
                inputs,
                change,
                amount,
                fee,
                ..
            } => UnsignedUtxoTransaction::Burn {
                inputs: unsigned_inputs(inputs),
                change: change.clone(),
                amount: *amount,
                fee: *fee,
            }
            .canonical(),
        }
    }

    fn verify_signature(&self) -> Result<()> {
        if self.signature().starts_with("luun-genesis-burn:") || self.inputs_are_genesis_signed() {
            return Ok(());
        }
        if !self
            .inputs()
            .iter()
            .all(|input| input.signature == self.signature())
        {
            bail!("transaction input signature does not match transaction signature");
        }
        let sender = self.sender();
        let public_key = decode_hex_array::<32>(sender)
            .with_context(|| format!("invalid public key for {sender}"))?;
        let signature =
            decode_hex_array::<64>(self.signature()).context("invalid signature hex")?;
        let verifying_key =
            VerifyingKey::from_bytes(&public_key).context("invalid transaction public key")?;
        let signature = Signature::from_bytes(&signature);
        verifying_key
            .verify(self.signing_payload().as_bytes(), &signature)
            .context("transaction signature is invalid")
    }

    fn inputs(&self) -> &[TxInput] {
        match self {
            Self::Transfer { inputs, .. } | Self::Burn { inputs, .. } => inputs,
        }
    }

    fn outputs(&self) -> &[TxOutput] {
        match self {
            Self::Transfer { outputs, .. } => outputs,
            Self::Burn { change, .. } => change,
        }
    }

    fn inputs_are_genesis_signed(&self) -> bool {
        self.inputs()
            .iter()
            .all(|input| input.signature == "genesis")
    }
}

impl TxInput {
    fn without_signature(&self) -> UnsignedTxInput {
        UnsignedTxInput {
            outpoint: self.outpoint.clone(),
            owner: self.owner.clone(),
        }
    }
}

impl OutPoint {
    fn id(&self) -> String {
        format!("{}:{}", self.txid, self.index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnsignedTxInput {
    outpoint: OutPoint,
    owner: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum UnsignedUtxoTransaction {
    Transfer {
        inputs: Vec<UnsignedTxInput>,
        outputs: Vec<TxOutput>,
        fee: Amount,
    },
    Burn {
        inputs: Vec<UnsignedTxInput>,
        change: Vec<TxOutput>,
        amount: Amount,
        fee: Amount,
    },
}

impl UnsignedUtxoTransaction {
    fn sign(self, wallet: &Wallet) -> Transaction {
        let signature = wallet.sign_payload(&self.canonical());
        let signed_inputs = self
            .inputs()
            .iter()
            .map(|input| TxInput {
                outpoint: input.outpoint.clone(),
                owner: input.owner.clone(),
                signature: signature.clone(),
            })
            .collect::<Vec<_>>();
        match self {
            Self::Transfer { outputs, fee, .. } => Transaction::Transfer {
                inputs: signed_inputs,
                outputs,
                fee,
                signature,
            },
            Self::Burn {
                change,
                amount,
                fee,
                ..
            } => Transaction::Burn {
                inputs: signed_inputs,
                change,
                amount,
                fee,
                signature,
            },
        }
    }

    fn inputs(&self) -> &[UnsignedTxInput] {
        match self {
            Self::Transfer { inputs, .. } | Self::Burn { inputs, .. } => inputs,
        }
    }

    fn canonical(&self) -> String {
        match self {
            Self::Transfer {
                inputs,
                outputs,
                fee,
            } => format!(
                "utxo-transfer:{}:{}:{fee}",
                canonical_inputs(inputs),
                canonical_outputs(outputs)
            ),
            Self::Burn {
                inputs,
                change,
                amount,
                fee,
            } => format!(
                "utxo-burn:{}:{}:{amount}:{fee}",
                canonical_inputs(inputs),
                canonical_outputs(change)
            ),
        }
    }
}

fn unsigned_inputs(inputs: &[TxInput]) -> Vec<UnsignedTxInput> {
    inputs.iter().map(TxInput::without_signature).collect()
}

fn canonical_inputs(inputs: &[UnsignedTxInput]) -> String {
    inputs
        .iter()
        .map(|input| {
            format!(
                "{}:{}:{}",
                input.outpoint.txid, input.outpoint.index, input.owner
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn canonical_outputs(outputs: &[TxOutput]) -> String {
    outputs
        .iter()
        .map(|output| format!("{}:{}", output.address, output.amount))
        .collect::<Vec<_>>()
        .join("|")
}

fn pending_spent_outpoints(pending: &[Transaction]) -> BTreeSet<OutPoint> {
    pending
        .iter()
        .flat_map(|tx| tx.inputs().iter().map(|input| input.outpoint.clone()))
        .collect()
}

fn transaction_inputs_spent_by(transaction: &Transaction, pending: &[Transaction]) -> bool {
    let spent = pending_spent_outpoints(pending);
    transaction
        .inputs()
        .iter()
        .any(|input| spent.contains(&input.outpoint))
}

fn transaction_inputs_available(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs()
        .iter()
        .all(|input| utxos.contains_key(&input.outpoint))
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

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize block for size check")
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
            "luun-leader-rank:{}:{}",
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
            "luun-leader-proof:{}:{}:{}:{}:{}:{}",
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
    eligible_from_height: u64,
    eligible_until_height: u64,
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
    #[serde(default = "default_ticket_expiry_window_heights")]
    pub ticket_expiry_window_heights: u64,
    pub max_pending_transactions: usize,
    pub max_block_transactions: usize,
    #[serde(default = "default_max_block_bytes")]
    pub max_block_bytes: usize,
}

impl Default for LaunchProfile {
    fn default() -> Self {
        Self {
            profile_id: "luun-devnet-v4".to_string(),
            ticket_maturity_delay_heights: DEFAULT_TICKET_MATURITY_DELAY,
            ticket_expiry_window_heights: DEFAULT_TICKET_EXPIRY_WINDOW,
            max_pending_transactions: MAX_PENDING_TRANSACTIONS,
            max_block_transactions: MAX_BLOCK_TRANSACTIONS,
            max_block_bytes: MAX_BLOCK_BYTES,
        }
    }
}

fn default_max_block_bytes() -> usize {
    MAX_BLOCK_BYTES
}

fn default_ticket_expiry_window_heights() -> u64 {
    DEFAULT_TICKET_EXPIRY_WINDOW
}

impl LaunchProfile {
    pub fn hash(&self) -> String {
        hex_hash(format!(
            "luun-launch-profile:{}:{}:{}:{}:{}:{}",
            self.profile_id,
            self.ticket_maturity_delay_heights,
            self.ticket_expiry_window_heights,
            self.max_pending_transactions,
            self.max_block_transactions,
            self.max_block_bytes
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionKind {
    Burn,
}

#[derive(Clone, Debug)]
pub struct Ledger {
    chain: Vec<Block>,
    genesis_allocations: BTreeMap<String, Amount>,
    utxos: BTreeMap<OutPoint, TxOutput>,
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
            .map(|burn| {
                let allocation = genesis_allocations
                    .get(&burn.from)
                    .copied()
                    .unwrap_or_default();
                Transaction::genesis_burn_with_allocation(burn.from, burn.amount, allocation)
            })
            .collect::<Result<Vec<_>>>()?;
        Self::new_with_genesis_transactions(genesis_allocations, transactions, vdf_rounds)
    }

    fn new_with_genesis_transactions(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_transactions: Vec<Transaction>,
        vdf_rounds: u32,
    ) -> Result<Self> {
        let launch_profile = LaunchProfile::default();
        let genesis = build_genesis_block(&genesis_allocations, genesis_transactions);
        let utxos = utxos_after_genesis(&genesis_allocations, &genesis)?;
        let tickets = genesis_tickets(&genesis_allocations, &genesis, &launch_profile)?;
        Ok(Self {
            chain: vec![genesis],
            genesis_allocations: genesis_allocations.clone(),
            utxos,
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
        let expected_genesis =
            build_genesis_block(&genesis_allocations, genesis.transactions.clone());
        if genesis != expected_genesis {
            bail!("chain snapshot genesis does not match its allocations and transactions");
        }
        let utxos = utxos_after_genesis(&genesis_allocations, &genesis)?;

        let mut ledger = Self {
            chain: vec![genesis],
            genesis_allocations,
            utxos,
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
            balances: balances_from_utxos(&self.utxos),
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
        self.utxos
            .values()
            .filter(|output| output.address == address)
            .map(|output| output.amount)
            .sum()
    }

    pub fn next_nonce(&self, address: &str) -> u64 {
        self.utxos
            .keys()
            .chain(
                self.pending
                    .iter()
                    .flat_map(|tx| tx.inputs().iter().map(|input| &input.outpoint)),
            )
            .filter(|outpoint| outpoint.txid.contains(address))
            .count() as u64
            + 1
    }

    pub fn build_transfer(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
    ) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        let mut outputs = vec![TxOutput {
            address: to.into(),
            amount,
        }];
        let change = input_total
            .checked_sub(required)
            .context("selected inputs do not cover transfer")?;
        if change > 0 {
            outputs.push(TxOutput {
                address: wallet.address().to_string(),
                amount: change,
            });
        }
        let transaction = UnsignedUtxoTransaction::Transfer {
            inputs,
            outputs,
            fee,
        }
        .sign(wallet);
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn build_burn(&self, wallet: &Wallet, amount: Amount, fee: Amount) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("burn amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        let change_amount = input_total
            .checked_sub(required)
            .context("selected inputs do not cover burn")?;
        let change = if change_amount > 0 {
            vec![TxOutput {
                address: wallet.address().to_string(),
                amount: change_amount,
            }]
        } else {
            Vec::new()
        };
        let transaction = UnsignedUtxoTransaction::Burn {
            inputs,
            change,
            amount,
            fee,
        }
        .sign(wallet);
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
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

        if transaction_inputs_spent_by(&transaction, &self.pending) {
            return Ok(false);
        }

        if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("mempool is full");
        }

        let mut utxos = self.utxos_after_valid_pending()?;
        if transaction_has_missing_inputs(&transaction, &utxos) {
            self.pending.push(transaction);
            return Ok(true);
        }
        apply_transaction(&transaction, &mut utxos)?;
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

        let transactions = self.select_block_transactions()?;
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
            reward: reward_with_fees(self.block_reward, &transactions)?,
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

        let mut utxos = self.utxos.clone();
        let mut signatures = BTreeSet::new();
        for tx in &block.transactions {
            if !signatures.insert(tx.signature()) {
                bail!("duplicate transaction in block");
            }
            apply_transaction(tx, &mut utxos)?;
        }
        if block.reward != reward_with_fees(self.block_reward, &block.transactions)? {
            bail!("block reward is invalid");
        }
        let mut tickets = self.tickets.clone();
        consume_leader_ticket(&block, &mut tickets)?;
        credit_reward_output(&mut utxos, &block)?;
        tickets.extend(tickets_created_by_block(&block, &self.launch_profile)?);

        let mined_signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        self.utxos = utxos;
        self.tickets = tickets;
        let available = self.utxos.clone();
        self.pending.retain(|tx| {
            !mined_signatures.contains(tx.signature())
                && transaction_inputs_available(tx, &available)
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
        if block.reward != reward_with_fees(self.block_reward, &block.transactions)? {
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
        if block.serialized_size_bytes()? > self.launch_profile.max_block_bytes {
            bail!("block exceeds max block size");
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
        let mut utxos = self.utxos.clone();
        let mut valid = Vec::new();
        let mut remaining = self.pending.iter().collect::<Vec<_>>();

        while !remaining.is_empty() {
            let mut progressed = false;
            let mut still_pending = Vec::new();

            for tx in remaining {
                if apply_transaction(tx, &mut utxos).is_ok() {
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

    fn select_block_transactions(&self) -> Result<Vec<Transaction>> {
        let mut utxos = self.utxos.clone();
        let mut remaining = self.valid_pending_transactions();
        let mut selected = Vec::new();

        if let Some(index) =
            best_selectable_transaction_index(&remaining, &utxos, Some(TransactionKind::Burn))
        {
            let tx = remaining.remove(index);
            let mut candidate = selected.clone();
            candidate.push(tx.clone());
            if estimated_block_size_bytes(&candidate)? <= self.launch_profile.max_block_bytes {
                apply_transaction(&tx, &mut utxos)?;
                selected.push(tx);
            }
        }

        while selected.len() < self.launch_profile.max_block_transactions {
            let Some(index) = best_selectable_transaction_index(&remaining, &utxos, None) else {
                break;
            };
            let tx = remaining.remove(index);
            let mut candidate = selected.clone();
            candidate.push(tx.clone());
            if estimated_block_size_bytes(&candidate)? <= self.launch_profile.max_block_bytes {
                apply_transaction(&tx, &mut utxos)?;
                selected.push(tx);
            }
        }
        Ok(selected)
    }

    fn select_inputs(
        &self,
        address: &str,
        amount: Amount,
    ) -> Result<(Vec<UnsignedTxInput>, Amount)> {
        let utxos = self.utxos_after_valid_pending()?;
        let mut selected = Vec::new();
        let mut total = 0_u64;
        for (outpoint, output) in &utxos {
            if output.address != address {
                continue;
            }
            selected.push(UnsignedTxInput {
                outpoint: outpoint.clone(),
                owner: address.to_string(),
            });
            total = total
                .checked_add(output.amount)
                .context("selected input total overflows")?;
            if total >= amount {
                return Ok((selected, total));
            }
        }
        bail!("insufficient funds for {address}")
    }

    fn validate_new_transaction(&self, transaction: &Transaction) -> Result<()> {
        let mut utxos = self.utxos_after_valid_pending()?;
        apply_transaction(transaction, &mut utxos)
    }

    fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut utxos)?;
        }
        Ok(utxos)
    }

    fn selected_ticket_for_height(&self, height: u64) -> Option<BurnTicket> {
        select_weighted_ticket(self.tip(), height, &self.tickets)
    }

    fn tip(&self) -> &Block {
        self.chain
            .last()
            .expect("ledger is always initialized with genesis")
    }
}

fn select_weighted_ticket(
    parent: &Block,
    target_height: u64,
    tickets: &[BurnTicket],
) -> Option<BurnTicket> {
    let eligible = tickets
        .iter()
        .filter(|ticket| ticket_is_eligible_for_height(ticket, target_height))
        .collect::<Vec<_>>();
    let total_weight = eligible.iter().try_fold(0_u128, |total, ticket| {
        total.checked_add(u128::from(ticket.amount))
    })?;
    if total_weight == 0 {
        return None;
    }

    let draw = weighted_ticket_draw(parent, target_height, total_weight);
    let mut cumulative = 0_u128;
    for ticket in eligible {
        cumulative = cumulative.checked_add(u128::from(ticket.amount))?;
        if draw < cumulative {
            return Some(ticket.clone());
        }
    }
    None
}

fn weighted_ticket_draw(parent: &Block, target_height: u64, total_weight: u128) -> u128 {
    let digest = Sha256::digest(
        format!(
            "luun-ticket-draw:{}:{}:{}",
            target_height, parent.hash, parent.vdf_output
        )
        .as_bytes(),
    );
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(bytes) % total_weight
}

fn tickets_created_by_block(block: &Block, profile: &LaunchProfile) -> Result<Vec<BurnTicket>> {
    if profile.ticket_expiry_window_heights == 0 {
        bail!("ticket expiry window must be at least one height");
    }
    let mut tickets = Vec::new();
    for tx in &block.transactions {
        let Transaction::Burn {
            inputs,
            amount,
            signature,
            ..
        } = tx
        else {
            continue;
        };
        let Some(owner) = inputs.first().map(|input| input.owner.clone()) else {
            continue;
        };
        if *amount == 0 {
            continue;
        }
        let target_height = block
            .height
            .checked_add(profile.ticket_maturity_delay_heights)
            .with_context(|| format!("ticket target height overflow at block {}", block.height))?;
        let eligible_until_height = target_height
            .checked_add(profile.ticket_expiry_window_heights - 1)
            .with_context(|| format!("ticket expiry height overflow at block {}", block.height))?;
        tickets.push(BurnTicket {
            id: signature.clone(),
            owner,
            amount: *amount,
            eligible_from_height: target_height,
            eligible_until_height,
        });
    }
    Ok(tickets)
}

fn genesis_tickets(
    genesis_allocations: &BTreeMap<String, Amount>,
    genesis: &Block,
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    if profile.ticket_maturity_delay_heights == 0 {
        return tickets_created_by_block(genesis, profile);
    }

    let burn_tickets = genesis
        .transactions
        .iter()
        .filter_map(|tx| {
            let Transaction::Burn {
                inputs,
                amount,
                signature,
                ..
            } = tx
            else {
                return None;
            };
            let owner = inputs.first()?.owner.clone();
            (*amount > 0).then(|| (owner, *amount, signature.clone()))
        })
        .collect::<Vec<_>>();

    if !burn_tickets.is_empty() {
        return genesis_bootstrap_tickets(burn_tickets, profile, genesis);
    }

    let Some((owner, amount)) = genesis_allocations
        .iter()
        .rev()
        .find(|(_, amount)| **amount > 0)
    else {
        return Ok(Vec::new());
    };
    genesis_bootstrap_tickets(
        vec![(
            owner.clone(),
            1,
            hex_hash(format!(
                "luun-genesis-ticket:{owner}:{amount}:{}",
                genesis.hash
            )),
        )],
        profile,
        genesis,
    )
}

fn genesis_bootstrap_tickets(
    source_tickets: Vec<(String, Amount, String)>,
    profile: &LaunchProfile,
    genesis: &Block,
) -> Result<Vec<BurnTicket>> {
    let mut tickets = Vec::new();
    for height in 1..=profile.ticket_maturity_delay_heights {
        for (owner, amount, source_id) in &source_tickets {
            tickets.push(BurnTicket {
                id: hex_hash(format!(
                    "luun-genesis-bootstrap-ticket:{}:{source_id}:{height}",
                    genesis.hash
                )),
                owner: owner.clone(),
                amount: *amount,
                eligible_from_height: height,
                eligible_until_height: height,
            });
        }
    }
    Ok(tickets)
}

fn consume_leader_ticket(block: &Block, tickets: &mut Vec<BurnTicket>) -> Result<()> {
    let Some(proof) = &block.leader_proof else {
        bail!("block is missing leader proof");
    };
    let Some(index) = tickets.iter().position(|ticket| {
        ticket.id == proof.ticket_id && ticket_is_eligible_for_height(ticket, block.height)
    }) else {
        bail!("leader ticket is not pending for block {}", block.height);
    };
    tickets.remove(index);
    tickets.retain(|ticket| ticket.eligible_until_height > block.height);
    Ok(())
}

fn ticket_is_eligible_for_height(ticket: &BurnTicket, height: u64) -> bool {
    ticket.eligible_from_height <= height && height <= ticket.eligible_until_height
}

fn ensure_block_has_burn(transactions: &[Transaction]) -> Result<()> {
    if !transactions.iter().any(Transaction::is_burn) {
        bail!("block must include at least one burn transaction");
    }
    Ok(())
}

fn fee_rate_key(transaction: &Transaction) -> u128 {
    let size = serialized_transaction_size_bytes(transaction).unwrap_or(usize::MAX);
    if size == 0 || size == usize::MAX {
        return 0;
    }
    u128::from(transaction.fee()) * 1_000_000 / size as u128
}

fn best_selectable_transaction_index(
    transactions: &[Transaction],
    utxos: &BTreeMap<OutPoint, TxOutput>,
    required_kind: Option<TransactionKind>,
) -> Option<usize> {
    transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| match required_kind {
            Some(TransactionKind::Burn) => tx.is_burn(),
            None => true,
        })
        .filter(|(_, tx)| {
            let mut utxos = utxos.clone();
            apply_transaction(tx, &mut utxos).is_ok()
        })
        .max_by(|(_, left), (_, right)| {
            fee_rate_key(left)
                .cmp(&fee_rate_key(right))
                .then_with(|| left.fee().cmp(&right.fee()))
                .then_with(|| left.is_burn().cmp(&right.is_burn()))
                .then_with(|| right.signature().cmp(left.signature()))
        })
        .map(|(index, _)| index)
}

fn serialized_transaction_size_bytes(transaction: &Transaction) -> Result<usize> {
    serde_json::to_vec(transaction)
        .map(|bytes| bytes.len())
        .context("failed to serialize transaction for size check")
}

fn estimated_block_size_bytes(transactions: &[Transaction]) -> Result<usize> {
    let block = Block {
        height: u64::MAX,
        prev_hash: "f".repeat(64),
        timestamp_ms: u64::MAX,
        miner: "f".repeat(64),
        reward: u64::MAX,
        vdf_rounds: u32::MAX,
        vdf_output: "f".repeat(64),
        leader_proof: Some(LeaderProof {
            ticket_id: "f".repeat(64),
            public_key: "f".repeat(64),
            signature: "f".repeat(128),
        }),
        transactions: transactions.to_vec(),
        hash: "f".repeat(64),
    };
    block.serialized_size_bytes()
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
        .find(|ticket| {
            ticket.id == proof.ticket_id && ticket_is_eligible_for_height(ticket, block.height)
        })
        .context("leader ticket is not pending for this height")?;
    if ticket.owner != block.miner {
        bail!("leader ticket owner does not match block miner");
    }
    if ticket.eligible_from_height > block.height {
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
    hex_hash(format!("luun-vdf-child:{prev_hash}:{height}"))
}

fn apply_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    transaction.verify_signature()?;
    ensure_single_input_owner(transaction)?;
    let input_total = spend_inputs(transaction, utxos)?;
    let output_total = transaction
        .outputs()
        .iter()
        .try_fold(0_u64, |total, output| {
            total
                .checked_add(output.amount)
                .context("transaction outputs overflow")
        })?;
    let required = output_total
        .checked_add(transaction.fee())
        .context("transaction outputs plus fee overflow")?
        .checked_add(match transaction {
            Transaction::Burn { amount, .. } => *amount,
            Transaction::Transfer { .. } => 0,
        })
        .context("transaction outputs plus burn overflow")?;
    if input_total != required {
        bail!("transaction inputs do not balance outputs, burn, and fee");
    }
    ensure_outputs_do_not_overflow(utxos, transaction.outputs())?;
    for (index, output) in transaction.outputs().iter().enumerate() {
        utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: index as u32,
            },
            output.clone(),
        );
    }
    Ok(())
}

fn reward_with_fees(base_reward: Amount, transactions: &[Transaction]) -> Result<Amount> {
    transactions.iter().try_fold(base_reward, |total, tx| {
        total
            .checked_add(tx.fee())
            .context("block reward plus fees overflows")
    })
}

fn spend_inputs(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<Amount> {
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for input in transaction.inputs() {
        if !seen.insert(input.outpoint.clone()) {
            bail!("duplicate input in transaction");
        }
        let output = utxos.remove(&input.outpoint).with_context(|| {
            format!("transaction spends missing output {}", input.outpoint.id())
        })?;
        if output.address != input.owner {
            bail!("transaction input owner does not match spent output");
        }
        total = total
            .checked_add(output.amount)
            .context("transaction input total overflows")?;
    }
    Ok(total)
}

fn transaction_has_missing_inputs(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs()
        .iter()
        .any(|input| !utxos.contains_key(&input.outpoint))
}

fn ensure_single_input_owner(transaction: &Transaction) -> Result<()> {
    let Some(first) = transaction.inputs().first() else {
        bail!("transaction has no inputs");
    };
    if transaction
        .inputs()
        .iter()
        .any(|input| input.owner != first.owner)
    {
        bail!("transaction inputs must have one owner");
    }
    Ok(())
}

fn credit_reward_output(utxos: &mut BTreeMap<OutPoint, TxOutput>, block: &Block) -> Result<()> {
    if block.reward == 0 {
        return Ok(());
    }
    let output = TxOutput {
        address: block.miner.clone(),
        amount: block.reward,
    };
    ensure_outputs_do_not_overflow(utxos, std::slice::from_ref(&output))?;
    utxos.insert(reward_outpoint(&block.hash), output);
    Ok(())
}

fn ensure_outputs_do_not_overflow(
    utxos: &BTreeMap<OutPoint, TxOutput>,
    outputs: &[TxOutput],
) -> Result<()> {
    let mut balances = BTreeMap::new();
    for output in utxos.values() {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance
            .checked_add(output.amount)
            .with_context(|| format!("balance overflow for {}", output.address))?;
    }
    for output in outputs {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance
            .checked_add(output.amount)
            .with_context(|| format!("balance overflow for {}", output.address))?;
    }
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
    let vdf_output = hex_hash(format!("luun-genesis-vdf:{genesis_allocations:?}:{txs}"));
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

fn utxos_after_genesis(
    genesis_allocations: &BTreeMap<String, Amount>,
    genesis: &Block,
) -> Result<BTreeMap<OutPoint, TxOutput>> {
    let mut utxos = genesis_allocation_utxos(genesis_allocations);
    for transaction in &genesis.transactions {
        match transaction {
            Transaction::Burn { .. } => apply_transaction(transaction, &mut utxos)?,
            Transaction::Transfer { .. } => bail!("genesis only supports burn transactions"),
        }
    }
    credit_reward_output(&mut utxos, genesis)?;
    Ok(utxos)
}

fn genesis_allocation_utxos(
    genesis_allocations: &BTreeMap<String, Amount>,
) -> BTreeMap<OutPoint, TxOutput> {
    genesis_allocations
        .iter()
        .filter(|(_, amount)| **amount > 0)
        .map(|(address, amount)| {
            (
                genesis_allocation_outpoint(address),
                TxOutput {
                    address: address.clone(),
                    amount: *amount,
                },
            )
        })
        .collect()
}

fn balances_from_utxos(utxos: &BTreeMap<OutPoint, TxOutput>) -> BTreeMap<String, Amount> {
    let mut balances = BTreeMap::new();
    for output in utxos.values() {
        let balance = balances.entry(output.address.clone()).or_insert(0_u64);
        *balance = balance.saturating_add(output.amount);
    }
    balances
}

fn genesis_allocation_outpoint(address: &str) -> OutPoint {
    OutPoint {
        txid: hex_hash(format!("luun-genesis-allocation:{address}")),
        index: 0,
    }
}

fn reward_outpoint(block_hash: &str) -> OutPoint {
    OutPoint {
        txid: block_hash.to_string(),
        index: u32::MAX,
    }
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
            Transaction::Burn { inputs, .. } => inputs.first().map(|input| input.owner.as_str()),
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
    let digest = Sha256::digest(format!("luun-vdf-seed:{seed}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    2 + (u128::from_be_bytes(bytes) % (VDF_MODULUS - 3))
}

fn vdf_challenge_prime(seed: &str, rounds: u32, output: u128) -> u64 {
    let digest = Sha256::digest(format!("luun-vdf-challenge:{seed}:{rounds}:{output:x}"));
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
