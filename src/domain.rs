use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Amount = u64;
pub const MICRO_IUNA: Amount = 1_000_000;
pub const BLOCK_REWARD: Amount = 100 * MICRO_IUNA;
pub const MINE_REWARD: Amount = MICRO_IUNA;
pub const DEFAULT_MINE_FEE: Amount = MINE_REWARD / 100;
pub const DEFAULT_TRANSACTION_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_FEE_PER_BYTE: Amount = 1;
pub const MAX_BLOCK_BYTES: usize = 100_000;
pub const VDF_TARGET_BLOCK_MS: u64 = 10 * 60 * 1_000;
pub const MINE_DIFFICULTY_BITS: u32 = 12;
const MINE_RETARGET_WINDOW_BLOCKS: u64 = 10;
const MINE_TARGET_ACTIONS_PER_BLOCK: u64 = 1;
const MINE_MAX_RETARGET_STEP_BITS: u32 = 2;
const MINE_MIN_DIFFICULTY_BITS: u32 = 1;
const MINE_MAX_DIFFICULTY_BITS: u32 = 32;
const MINE_MAX_ANCHOR_AGE_BLOCKS: u64 = MINE_RETARGET_WINDOW_BLOCKS;
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
const WALLET_SEED_DOMAIN: &str = "iuna-wallet-seed";

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
    Mine {
        output: TxOutput,
        anchor: String,
        #[serde(default)]
        salt: u64,
        nonce: u64,
        difficulty_bits: u32,
        #[serde(default)]
        fee: Amount,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof_header: Option<String>,
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
        let signature = hex_hash(format!("iuna-genesis-burn:{}", unsigned.canonical()));
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
            Self::Mine { output, .. } => output.address.as_str(),
        }
    }

    pub fn to(&self) -> Option<&str> {
        match self {
            Self::Transfer { outputs, .. } => outputs.first().map(|output| output.address.as_str()),
            Self::Burn { .. } => None,
            Self::Mine { output, .. } => Some(output.address.as_str()),
        }
    }

    pub fn amount(&self) -> Amount {
        match self {
            Self::Transfer { outputs, .. } => {
                outputs.first().map(|output| output.amount).unwrap_or(0)
            }
            Self::Burn { amount, .. } => *amount,
            Self::Mine { output, .. } => output.amount,
        }
    }

    pub fn fee(&self) -> Amount {
        match self {
            Self::Transfer { fee, .. } | Self::Burn { fee, .. } => *fee,
            Self::Mine { fee, .. } => *fee,
        }
    }

    pub fn total_debit(&self) -> Result<Amount> {
        if matches!(self, Self::Mine { .. }) {
            return Ok(0);
        }
        self.amount()
            .checked_add(self.fee())
            .context("transaction amount plus fee overflows")
    }

    pub fn signature(&self) -> &str {
        match self {
            Self::Transfer { signature, .. } | Self::Burn { signature, .. } => signature,
            Self::Mine { signature, .. } => signature,
        }
    }

    pub fn is_burn(&self) -> bool {
        matches!(self, Self::Burn { .. })
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", self.signing_payload(), self.signature())
    }

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serialized_transaction_size_bytes(self)
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
            Self::Mine {
                output,
                anchor,
                salt,
                nonce,
                difficulty_bits,
                fee,
                ..
            } => mine_payload(output, anchor, *salt, *nonce, *difficulty_bits, *fee),
        }
    }

    fn verify_signature(&self) -> Result<()> {
        if let Self::Mine {
            output,
            anchor,
            salt,
            nonce,
            difficulty_bits,
            fee,
            proof_header,
            signature,
        } = self
        {
            let expected = if let Some(proof_header) = proof_header {
                let header = stratum_mine_header_bytes(
                    output,
                    anchor,
                    *salt,
                    *nonce,
                    *difficulty_bits,
                    *fee,
                )?;
                let expected_header = hex_encode(header);
                if *proof_header != expected_header {
                    bail!("mine transaction proof header is invalid");
                }
                stratum_mine_signature(&header)
            } else {
                mine_signature(output, anchor, *salt, *nonce, *difficulty_bits, *fee)
            };
            if *signature != expected {
                bail!("mine transaction proof hash is invalid");
            }
            if !hash_meets_difficulty(signature, *difficulty_bits) {
                bail!("mine transaction proof does not meet difficulty");
            }
            return Ok(());
        }
        if self.signature().starts_with("iuna-genesis-burn:") || self.inputs_are_genesis_signed() {
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
            Self::Mine { .. } => &[],
        }
    }

    fn outputs(&self) -> &[TxOutput] {
        match self {
            Self::Transfer { outputs, .. } => outputs,
            Self::Burn { change, .. } => change,
            Self::Mine { output, .. } => std::slice::from_ref(output),
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

fn mine_payload(
    output: &TxOutput,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
    fee: Amount,
) -> String {
    let payload = if salt == 0 {
        format!(
            "iuna-mine:{}:{}:{}:{}",
            output.address, output.amount, anchor, nonce
        )
    } else {
        format!(
            "iuna-mine:{}:{}:{}:{}:{}",
            output.address, output.amount, anchor, salt, nonce
        )
    } + &format!(":{difficulty_bits}");
    if fee == 0 {
        payload
    } else {
        format!("{payload}:{fee}")
    }
}

fn mine_signature(
    output: &TxOutput,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
    fee: Amount,
) -> String {
    hex_hash(mine_payload(
        output,
        anchor,
        salt,
        nonce,
        difficulty_bits,
        fee,
    ))
}

pub const STRATUM_EXTRANONCE1_HEX: &str = "00000000";
pub const STRATUM_EXTRANONCE2_SIZE: usize = 4;
const STRATUM_MINE_VERSION: [u8; 4] = [1, 0, 0, 0];
const STRATUM_MINE_NTIME: [u8; 4] = [0, 0, 0, 0];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StratumMineTemplate {
    pub recipient: String,
    pub output_amount: Amount,
    pub fee: Amount,
    pub anchor: String,
    pub salt: u64,
    pub difficulty_bits: u32,
    pub coinbase_prefix: Vec<u8>,
    pub version_hex: String,
    pub prev_hash_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
}

impl StratumMineTemplate {
    pub fn coinb1_hex(&self) -> String {
        hex_encode(&self.coinbase_prefix)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StratumMineShare {
    pub extranonce2: [u8; 4],
    pub header_nonce: [u8; 4],
}

pub fn pack_stratum_nonce(extranonce2: [u8; 4], header_nonce: [u8; 4]) -> u64 {
    let extra = u32::from_be_bytes(extranonce2) as u64;
    let nonce = u32::from_le_bytes(header_nonce) as u64;
    (extra << 32) | nonce
}

fn unpack_stratum_nonce(nonce: u64) -> ([u8; 4], [u8; 4]) {
    (
        ((nonce >> 32) as u32).to_be_bytes(),
        (nonce as u32).to_le_bytes(),
    )
}

fn stratum_coinbase_prefix(
    output: &TxOutput,
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
    fee: Amount,
) -> Vec<u8> {
    if salt == 0 {
        format!(
            "iuna-stratum-mine:{}:{}:{}:{}:{}:",
            output.address, output.amount, fee, anchor, difficulty_bits
        )
    } else {
        format!(
            "iuna-stratum-mine:{}:{}:{}:{}:{}:{}:",
            output.address, output.amount, fee, anchor, salt, difficulty_bits
        )
    }
    .into_bytes()
}

fn stratum_coinbase_bytes(
    output: &TxOutput,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
    fee: Amount,
) -> Vec<u8> {
    let (extranonce2, _) = unpack_stratum_nonce(nonce);
    let mut coinbase = stratum_coinbase_prefix(output, anchor, salt, difficulty_bits, fee);
    coinbase.extend_from_slice(&[0, 0, 0, 0]);
    coinbase.extend_from_slice(&extranonce2);
    coinbase
}

fn double_sha256(bytes: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    second.into()
}

fn stratum_mine_header_bytes(
    output: &TxOutput,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
    fee: Amount,
) -> Result<[u8; 80]> {
    let mut header = [0_u8; 80];
    header[0..4].copy_from_slice(&STRATUM_MINE_VERSION);
    let anchor_bytes =
        decode_hex_array::<32>(anchor).context("mine transaction anchor is not hex")?;
    header[4..36].copy_from_slice(&anchor_bytes);
    let merkle_root = double_sha256(&stratum_coinbase_bytes(
        output,
        anchor,
        salt,
        nonce,
        difficulty_bits,
        fee,
    ));
    header[36..68].copy_from_slice(&merkle_root);
    header[68..72].copy_from_slice(&STRATUM_MINE_NTIME);
    header[72..76].copy_from_slice(&difficulty_bits.to_le_bytes());
    let (_, header_nonce) = unpack_stratum_nonce(nonce);
    header[76..80].copy_from_slice(&header_nonce);
    Ok(header)
}

fn stratum_mine_signature(header: &[u8; 80]) -> String {
    let mut digest = double_sha256(header);
    digest.reverse();
    hex_encode(digest)
}

fn stratum_mine_template(
    recipient: impl Into<String>,
    mine_reward: Amount,
    fee: Amount,
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
) -> Result<StratumMineTemplate> {
    let recipient = recipient.into();
    if fee > mine_reward {
        bail!("mine transaction fee exceeds reward");
    }
    let output = TxOutput {
        address: recipient.clone(),
        amount: mine_reward - fee,
    };
    let anchor_bytes =
        decode_hex_array::<32>(anchor).context("mine transaction anchor is not hex")?;
    Ok(StratumMineTemplate {
        recipient,
        output_amount: output.amount,
        fee,
        anchor: anchor.to_string(),
        salt,
        difficulty_bits,
        coinbase_prefix: stratum_coinbase_prefix(&output, anchor, salt, difficulty_bits, fee),
        version_hex: hex_encode(STRATUM_MINE_VERSION),
        prev_hash_hex: hex_encode(anchor_bytes),
        nbits_hex: hex_encode(difficulty_bits.to_le_bytes()),
        ntime_hex: hex_encode(STRATUM_MINE_NTIME),
    })
}

fn hash_meets_difficulty(hash: &str, difficulty_bits: u32) -> bool {
    let full_zero_nibbles = (difficulty_bits / 4) as usize;
    let remaining_bits = difficulty_bits % 4;
    if hash.len() < full_zero_nibbles + usize::from(remaining_bits > 0) {
        return false;
    }
    if !hash.as_bytes()[..full_zero_nibbles]
        .iter()
        .all(|byte| *byte == b'0')
    {
        return false;
    }
    if remaining_bits == 0 {
        return true;
    }
    let Some(next) = hash.as_bytes().get(full_zero_nibbles).copied() else {
        return false;
    };
    let Some(value) = (next as char).to_digit(16) else {
        return false;
    };
    value < (1 << (4 - remaining_bits))
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
    #[serde(default)]
    pub finalizer_rank: u32,
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
            finalizer_rank: draft.finalizer_rank,
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
            "{}:{}",
            self.legacy_content_hash_prefix(&leader_proof),
            txs
        ))
    }

    fn legacy_content_hash_prefix(&self, leader_proof: &str) -> String {
        if self.finalizer_rank == 0 {
            format!(
                "block-content:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.timestamp_ms,
                self.miner,
                self.reward,
                self.vdf_rounds,
                leader_proof
            )
        } else {
            format!(
                "block-content-v2:{}:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.timestamp_ms,
                self.miner,
                self.finalizer_rank,
                self.reward,
                self.vdf_rounds,
                leader_proof
            )
        }
    }

    fn leader_score(&self) -> LeaderScore {
        LeaderScore {
            finalizer_rank: self.finalizer_rank,
            proof_rank: self
                .leader_proof
                .as_ref()
                .map(LeaderProof::rank)
                .unwrap_or_else(|| self.hash.clone()),
        }
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
            "iuna-leader-rank:{}:{}",
            self.ticket_id, self.signature
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LeaderProofPayload {
    height: u64,
    prev_hash: String,
    finalizer_rank: u32,
    vdf_output: String,
    ticket_id: String,
    ticket_amount: Amount,
    ticket_owner: String,
}

impl LeaderProofPayload {
    fn canonical(&self) -> String {
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
    finalizer_rank: u32,
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
            finalizer_rank: self.finalizer_rank,
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
            finalizer_rank: self.finalizer_rank,
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
    finalizer_rank: u32,
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
    pub mine_reward: Amount,
    pub current_mine_difficulty_bits: u32,
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
    #[serde(default = "default_mine_difficulty_bits")]
    pub mine_difficulty_bits: u32,
    pub max_pending_transactions: usize,
    pub max_block_transactions: usize,
    #[serde(default = "default_max_block_bytes")]
    pub max_block_bytes: usize,
}

impl Default for LaunchProfile {
    fn default() -> Self {
        Self {
            profile_id: "iuna-devnet-v5".to_string(),
            ticket_maturity_delay_heights: DEFAULT_TICKET_MATURITY_DELAY,
            ticket_expiry_window_heights: DEFAULT_TICKET_EXPIRY_WINDOW,
            mine_difficulty_bits: MINE_DIFFICULTY_BITS,
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

fn default_mine_difficulty_bits() -> u32 {
    MINE_DIFFICULTY_BITS
}

impl LaunchProfile {
    pub fn hash(&self) -> String {
        hex_hash(format!(
            "iuna-launch-profile:{}:{}:{}:{}:{}:{}:{}",
            self.profile_id,
            self.ticket_maturity_delay_heights,
            self.ticket_expiry_window_heights,
            self.mine_difficulty_bits,
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
struct LeaderScore {
    finalizer_rank: u32,
    proof_rank: String,
}

impl Ord for LeaderScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.finalizer_rank
            .cmp(&other.finalizer_rank)
            .then_with(|| self.proof_rank.cmp(&other.proof_rank))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionSubmitOutcome {
    Added,
    AlreadyKnown,
    ConflictsWithPending,
}

impl TransactionSubmitOutcome {
    pub fn added(self) -> bool {
        matches!(self, Self::Added)
    }
}

#[derive(Clone, Debug)]
pub struct Ledger {
    chain: Vec<Block>,
    genesis_allocations: BTreeMap<String, Amount>,
    utxos: BTreeMap<OutPoint, TxOutput>,
    tickets: Vec<BurnTicket>,
    pending: Vec<Transaction>,
    mine_reward: Amount,
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
            mine_reward: MINE_REWARD,
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
            mine_reward: MINE_REWARD,
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
            mine_reward: self.mine_reward,
            current_mine_difficulty_bits: self.current_mine_difficulty_bits(),
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

    pub fn is_setup_placeholder(&self) -> bool {
        self.height() == 0
            && self.genesis_allocations.is_empty()
            && self.chain[0].transactions.is_empty()
            && self.pending.is_empty()
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

    pub fn current_mine_difficulty_bits(&self) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(self.tip().height)
    }

    pub fn balance_of(&self, address: &str) -> Amount {
        self.utxos
            .values()
            .filter(|output| output.address == address)
            .map(|output| output.amount)
            .sum()
    }

    pub fn utxos_for_address(&self, address: &str) -> Vec<(OutPoint, TxOutput)> {
        self.utxos
            .iter()
            .filter(|(_, output)| output.address == address)
            .map(|(outpoint, output)| (outpoint.clone(), output.clone()))
            .collect()
    }

    pub fn available_utxos_for_address(&self, address: &str) -> Result<Vec<(OutPoint, TxOutput)>> {
        Ok(self
            .utxos_after_valid_pending()?
            .into_iter()
            .filter(|(_, output)| output.address == address)
            .collect())
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

    pub fn build_transfer_with_inputs(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        outpoints: &[OutPoint],
    ) -> Result<Transaction> {
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let (inputs, input_total) =
            self.select_inputs_by_outpoint(wallet.address(), required, outpoints)?;
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

    pub fn build_mine(&self, recipient: impl Into<String>) -> Result<Transaction> {
        self.build_mine_with_fee(recipient, 0)
    }

    pub fn build_mine_with_fee(
        &self,
        recipient: impl Into<String>,
        fee: Amount,
    ) -> Result<Transaction> {
        let recipient = recipient.into();
        if fee > self.mine_reward {
            bail!("mine transaction fee exceeds reward");
        }
        let output = TxOutput {
            address: recipient,
            amount: self.mine_reward - fee,
        };
        let anchor = self.tip().hash.clone();
        let salt = 1;
        let difficulty_bits = self.current_mine_difficulty_bits();
        for nonce in 0..u64::MAX {
            let signature = mine_signature(&output, &anchor, salt, nonce, difficulty_bits, fee);
            if !hash_meets_difficulty(&signature, difficulty_bits) {
                continue;
            }
            let transaction = Transaction::Mine {
                output: output.clone(),
                anchor: anchor.clone(),
                salt,
                nonce,
                difficulty_bits,
                fee,
                proof_header: None,
                signature,
            };
            if self.has_transaction(transaction.signature()) {
                continue;
            }
            self.validate_new_transaction(&transaction)?;
            return Ok(transaction);
        }
        bail!("could not find valid mine proof");
    }

    pub fn stratum_mine_template(
        &self,
        recipient: impl Into<String>,
        fee: Amount,
        anchor: impl AsRef<str>,
        salt: u64,
        difficulty_bits: u32,
    ) -> Result<StratumMineTemplate> {
        stratum_mine_template(
            recipient,
            self.mine_reward,
            fee,
            anchor.as_ref(),
            salt,
            difficulty_bits,
        )
    }

    pub fn build_stratum_mine(
        &self,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let output = TxOutput {
            address: template.recipient,
            amount: template.output_amount,
        };
        let nonce = pack_stratum_nonce(share.extranonce2, share.header_nonce);
        let header = stratum_mine_header_bytes(
            &output,
            &template.anchor,
            template.salt,
            nonce,
            template.difficulty_bits,
            template.fee,
        )?;
        let transaction = Transaction::Mine {
            output,
            anchor: template.anchor,
            salt: template.salt,
            nonce,
            difficulty_bits: template.difficulty_bits,
            fee: template.fee,
            proof_header: Some(hex_encode(header)),
            signature: stratum_mine_signature(&header),
        };
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<bool> {
        Ok(self.submit_transaction_with_outcome(transaction)?.added())
    }

    pub fn submit_transaction_with_outcome(
        &mut self,
        transaction: Transaction,
    ) -> Result<TransactionSubmitOutcome> {
        if self.has_transaction(transaction.signature()) {
            return Ok(TransactionSubmitOutcome::AlreadyKnown);
        }

        transaction.verify_signature()?;
        self.validate_transaction_terms(&transaction)?;

        if transaction_inputs_spent_by(&transaction, &self.pending) {
            return Ok(TransactionSubmitOutcome::ConflictsWithPending);
        }

        if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("mempool is full");
        }

        let mut utxos = self.utxos_after_valid_pending()?;
        if transaction_has_missing_inputs(&transaction, &utxos) {
            self.pending.push(transaction);
            return Ok(TransactionSubmitOutcome::Added);
        }
        apply_transaction(&transaction, &mut utxos)?;
        self.pending.push(transaction);
        Ok(TransactionSubmitOutcome::Added)
    }

    pub fn mine_next_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<Block> {
        let prepared = self.prepare_next_block(wallet.address(), timestamp_ms)?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn prepare_next_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        let Some((finalizer_rank, leader_ticket)) = self.finalizer_ticket_for_miner(height, miner)
        else {
            bail!("cannot mine block without a mature burn ticket");
        };
        if self.expected_leader_for_next_block().is_none() {
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
            reward: fee_reward(&transactions)?,
            vdf_rounds: self.vdf_rounds_for_finalizer_rank(finalizer_rank)?,
            vdf_seed,
            finalizer_rank,
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
            self.validate_transaction_terms(tx)?;
            apply_transaction(tx, &mut utxos)?;
        }
        if block.reward != fee_reward(&block.transactions)? {
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
        self.chain.push(block);
        let available = self.utxos.clone();
        let pending = std::mem::take(&mut self.pending);
        self.pending = pending
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && transaction_inputs_available(tx, &available)
                    && self.validate_transaction_terms(tx).is_ok()
            })
            .collect();
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
        if block.reward != fee_reward(&block.transactions)? {
            bail!("block reward is invalid");
        }
        let expected_vdf_rounds = self.vdf_rounds_for_finalizer_rank(block.finalizer_rank)?;
        if block.vdf_rounds != expected_vdf_rounds {
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
        let selected_ticket = self
            .ticket_for_finalizer_rank(block.height, block.finalizer_rank)
            .context("no selected ticket for block finalizer rank")?;
        if selected_ticket.owner != block.miner {
            bail!(
                "block finalizer {} is not selected for rank {}",
                block.miner,
                block.finalizer_rank
            );
        }
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
        let base_rounds = base_vdf_rounds_for_finalizer_rank(tip.vdf_rounds, tip.finalizer_rank);
        retarget_vdf_rounds(base_rounds, average_observed_ms)
    }

    pub fn expected_leader_for_next_block(&self) -> Option<String> {
        self.selected_ticket_for_height(self.tip().height + 1)
            .map(|ticket| ticket.owner)
    }

    pub fn finalizer_rank_for_next_block(&self, miner: &str) -> Option<u32> {
        self.finalizer_ticket_for_miner(self.tip().height + 1, miner)
            .map(|(rank, _)| rank)
    }

    fn valid_pending_transactions(&self) -> Vec<Transaction> {
        let mut utxos = self.utxos.clone();
        let mut valid = Vec::new();
        let mut remaining = self.pending.iter().collect::<Vec<_>>();

        while !remaining.is_empty() {
            let mut progressed = false;
            let mut still_pending = Vec::new();

            for tx in remaining {
                if self.validate_transaction_terms(tx).is_ok()
                    && apply_transaction(tx, &mut utxos).is_ok()
                {
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

    fn select_inputs_by_outpoint(
        &self,
        address: &str,
        amount: Amount,
        outpoints: &[OutPoint],
    ) -> Result<(Vec<UnsignedTxInput>, Amount)> {
        if outpoints.is_empty() {
            bail!("at least one UTXO must be selected");
        }
        let utxos = self.utxos_after_valid_pending()?;
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        let mut total = 0_u64;
        for outpoint in outpoints {
            if !seen.insert(outpoint.clone()) {
                bail!("selected UTXO {} is duplicated", outpoint.id());
            }
            let output = utxos
                .get(outpoint)
                .with_context(|| format!("selected UTXO {} is not spendable", outpoint.id()))?;
            if output.address != address {
                bail!("selected UTXO {} is not owned by {address}", outpoint.id());
            }
            selected.push(UnsignedTxInput {
                outpoint: outpoint.clone(),
                owner: address.to_string(),
            });
            total = total
                .checked_add(output.amount)
                .context("selected input total overflows")?;
        }
        if total < amount {
            bail!("selected UTXOs do not cover transfer amount plus fee");
        }
        Ok((selected, total))
    }

    fn validate_new_transaction(&self, transaction: &Transaction) -> Result<()> {
        self.validate_transaction_terms(transaction)?;
        let mut utxos = self.utxos_after_valid_pending()?;
        apply_transaction(transaction, &mut utxos)
    }

    fn validate_transaction_terms(&self, transaction: &Transaction) -> Result<()> {
        if let Transaction::Mine {
            output,
            anchor,
            difficulty_bits,
            fee,
            ..
        } = transaction
        {
            if output
                .amount
                .checked_add(*fee)
                .context("mine transaction output plus fee overflows")?
                != self.mine_reward
            {
                bail!("mine transaction reward is invalid");
            }
            let anchor_block = self
                .chain
                .iter()
                .find(|block| block.hash == *anchor)
                .context("mine transaction anchor is not on this chain")?;
            let anchor_age = self.tip().height.saturating_sub(anchor_block.height);
            if anchor_age > MINE_MAX_ANCHOR_AGE_BLOCKS {
                bail!("mine transaction anchor is too old");
            }
            let required_difficulty =
                self.mine_difficulty_bits_for_anchor_height(anchor_block.height);
            if *difficulty_bits != required_difficulty {
                bail!("mine transaction difficulty is invalid");
            }
        }
        Ok(())
    }

    fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut utxos)?;
        }
        Ok(utxos)
    }

    fn selected_ticket_for_height(&self, height: u64) -> Option<BurnTicket> {
        self.ticket_for_finalizer_rank(height, 0)
    }

    fn ticket_for_finalizer_rank(&self, height: u64, rank: u32) -> Option<BurnTicket> {
        ranked_tickets_for_height(self.tip(), height, &self.tickets)
            .get(rank as usize)
            .cloned()
    }

    fn finalizer_ticket_for_miner(&self, height: u64, miner: &str) -> Option<(u32, BurnTicket)> {
        ranked_tickets_for_height(self.tip(), height, &self.tickets)
            .into_iter()
            .enumerate()
            .find(|(_, ticket)| ticket.owner == miner)
            .and_then(|(rank, ticket)| {
                let rank = u32::try_from(rank).ok()?;
                Some((rank, ticket))
            })
    }

    fn vdf_rounds_for_finalizer_rank(&self, rank: u32) -> Result<u32> {
        vdf_rounds_for_finalizer_rank(self.vdf_rounds, rank)
    }

    fn mine_difficulty_bits_for_anchor_height(&self, anchor_height: u64) -> u32 {
        let mut difficulty = self.launch_profile.mine_difficulty_bits;
        let mut window_end = MINE_RETARGET_WINDOW_BLOCKS;
        while window_end <= anchor_height {
            let window_start = window_end + 1 - MINE_RETARGET_WINDOW_BLOCKS;
            let mine_actions = self
                .chain
                .iter()
                .filter(|block| window_start <= block.height && block.height <= window_end)
                .map(mine_action_count)
                .sum::<u64>();
            difficulty = retarget_mine_difficulty_bits(difficulty, mine_actions);
            window_end = window_end.saturating_add(MINE_RETARGET_WINDOW_BLOCKS);
        }
        difficulty
    }

    fn tip(&self) -> &Block {
        self.chain
            .last()
            .expect("ledger is always initialized with genesis")
    }
}

fn ranked_tickets_for_height(
    parent: &Block,
    target_height: u64,
    tickets: &[BurnTicket],
) -> Vec<BurnTicket> {
    let mut remaining = tickets
        .iter()
        .filter(|ticket| ticket_is_eligible_for_height(ticket, target_height))
        .cloned()
        .collect::<Vec<_>>();
    let mut ranked = Vec::with_capacity(remaining.len());

    for rank in 0.. {
        let Some(selected_index) =
            select_weighted_ticket_index(parent, target_height, rank, &remaining)
        else {
            break;
        };
        ranked.push(remaining.remove(selected_index));
    }

    ranked
}

fn select_weighted_ticket_index(
    parent: &Block,
    target_height: u64,
    rank: u32,
    tickets: &[BurnTicket],
) -> Option<usize> {
    let total_weight = tickets.iter().try_fold(0_u128, |total, ticket| {
        total.checked_add(u128::from(ticket.amount))
    })?;
    if total_weight == 0 {
        return None;
    }

    let draw = weighted_ticket_draw(parent, target_height, rank, total_weight);
    let mut cumulative = 0_u128;
    for (index, ticket) in tickets.iter().enumerate() {
        cumulative = cumulative.checked_add(u128::from(ticket.amount))?;
        if draw < cumulative {
            return Some(index);
        }
    }
    None
}

fn weighted_ticket_draw(parent: &Block, target_height: u64, rank: u32, total_weight: u128) -> u128 {
    let seed = if rank == 0 {
        format!(
            "iuna-ticket-draw:{}:{}:{}",
            target_height, parent.hash, parent.vdf_output
        )
    } else {
        format!(
            "iuna-ticket-draw-rank:{}:{}:{}:{}",
            target_height, rank, parent.hash, parent.vdf_output
        )
    };
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(bytes) % total_weight
}

fn vdf_rounds_for_finalizer_rank(base_rounds: u32, rank: u32) -> Result<u32> {
    base_rounds
        .checked_mul(rank.checked_add(1).context("finalizer rank overflows")?)
        .context("finalizer rank VDF rounds overflow")
}

fn base_vdf_rounds_for_finalizer_rank(vdf_rounds: u32, rank: u32) -> u32 {
    vdf_rounds / rank.saturating_add(1).max(1)
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
                "iuna-genesis-ticket:{owner}:{amount}:{}",
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
                    "iuna-genesis-bootstrap-ticket:{}:{source_id}:{height}",
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

fn mine_action_count(block: &Block) -> u64 {
    block
        .transactions
        .iter()
        .filter(|transaction| matches!(transaction, Transaction::Mine { .. }))
        .count() as u64
}

fn retarget_mine_difficulty_bits(current: u32, mine_actions: u64) -> u32 {
    let target = MINE_RETARGET_WINDOW_BLOCKS.saturating_mul(MINE_TARGET_ACTIONS_PER_BLOCK);
    if target == 0 || mine_actions == target {
        return current.clamp(MINE_MIN_DIFFICULTY_BITS, MINE_MAX_DIFFICULTY_BITS);
    }

    let step = if mine_actions > target {
        floor_log2_ratio(mine_actions, target).min(MINE_MAX_RETARGET_STEP_BITS)
    } else if mine_actions == 0 {
        MINE_MAX_RETARGET_STEP_BITS
    } else {
        floor_log2_ratio(target, mine_actions).min(MINE_MAX_RETARGET_STEP_BITS)
    };

    if step == 0 {
        return current.clamp(MINE_MIN_DIFFICULTY_BITS, MINE_MAX_DIFFICULTY_BITS);
    }
    if mine_actions > target {
        current
            .saturating_add(step)
            .clamp(MINE_MIN_DIFFICULTY_BITS, MINE_MAX_DIFFICULTY_BITS)
    } else {
        current
            .saturating_sub(step)
            .clamp(MINE_MIN_DIFFICULTY_BITS, MINE_MAX_DIFFICULTY_BITS)
    }
}

fn floor_log2_ratio(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 || numerator <= denominator {
        return 0;
    }
    let mut step = 0_u32;
    let mut threshold = denominator;
    while threshold <= numerator / 2 {
        threshold = threshold.saturating_mul(2);
        step = step.saturating_add(1);
    }
    step
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
        finalizer_rank: 0,
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
        bail!("leader proof public key does not match block finalizer");
    }
    let ticket = tickets
        .iter()
        .find(|ticket| {
            ticket.id == proof.ticket_id && ticket_is_eligible_for_height(ticket, block.height)
        })
        .context("leader ticket is not pending for this height")?;
    if ticket.owner != block.miner {
        bail!("leader ticket owner does not match block finalizer");
    }
    if ticket.eligible_from_height > block.height {
        bail!("leader ticket is not mature");
    }

    let payload = LeaderProofPayload {
        height: block.height,
        prev_hash: block.prev_hash.clone(),
        finalizer_rank: block.finalizer_rank,
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
    hex_hash(format!("iuna-vdf-child:{prev_hash}:{height}"))
}

fn apply_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    transaction.verify_signature()?;
    if let Transaction::Mine { output, .. } = transaction {
        ensure_outputs_do_not_overflow(utxos, std::slice::from_ref(output))?;
        utxos.insert(
            OutPoint {
                txid: transaction.signature().to_string(),
                index: 0,
            },
            output.clone(),
        );
        return Ok(());
    }
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
            Transaction::Transfer { .. } | Transaction::Mine { .. } => 0,
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

fn fee_reward(transactions: &[Transaction]) -> Result<Amount> {
    transactions.iter().try_fold(0_u64, |total, tx| {
        total.checked_add(tx.fee()).context("block fees overflow")
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
    if matches!(transaction, Transaction::Mine { .. }) {
        return Ok(());
    }
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
    let vdf_output = hex_hash(format!("iuna-genesis-vdf:{genesis_allocations:?}:{txs}"));
    let mut genesis = Block {
        height: 0,
        prev_hash: "0".repeat(64),
        timestamp_ms: 0,
        miner,
        finalizer_rank: 0,
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
            Transaction::Transfer { .. } | Transaction::Mine { .. } => {
                bail!("genesis only supports burn transactions")
            }
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
        txid: hex_hash(format!("iuna-genesis-allocation:{address}")),
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
            Transaction::Transfer { .. } | Transaction::Mine { .. } => None,
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
    let digest = Sha256::digest(format!("iuna-vdf-seed:{seed}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    2 + (u128::from_be_bytes(bytes) % (VDF_MODULUS - 3))
}

fn vdf_challenge_prime(seed: &str, rounds: u32, output: u128) -> u64 {
    let digest = Sha256::digest(format!("iuna-vdf-challenge:{seed}:{rounds}:{output:x}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_with_wallet_utxos(wallet: &Wallet, amounts: &[Amount]) -> Ledger {
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.utxos = amounts
            .iter()
            .enumerate()
            .map(|(index, amount)| {
                (
                    OutPoint {
                        txid: format!("test-utxo-{index}"),
                        index: 0,
                    },
                    TxOutput {
                        address: wallet.address().to_string(),
                        amount: *amount,
                    },
                )
            })
            .collect();
        ledger
    }

    fn pending_balances(ledger: &Ledger) -> BTreeMap<String, Amount> {
        balances_from_utxos(&ledger.utxos_after_valid_pending().unwrap())
    }

    fn ledger_with_allocation(wallet: &Wallet, amount: Amount) -> Ledger {
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), amount);
        Ledger::new(genesis, 1)
    }

    fn mine_burn_block_with_mines(ledger: &mut Ledger, wallet: &Wallet, mine_actions: usize) {
        let burn = ledger.build_burn(wallet, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        for _ in 0..mine_actions {
            let mine = ledger.build_mine(wallet.address()).unwrap();
            ledger.submit_transaction(mine).unwrap();
        }
        let block = ledger.mine_next_block(wallet, ledger.height() + 1).unwrap();
        ledger.apply_block(block).unwrap();
    }

    fn mine_with_output_and_fee(
        ledger: &Ledger,
        recipient: &str,
        output_amount: Amount,
        fee: Amount,
    ) -> Transaction {
        let output = TxOutput {
            address: recipient.to_string(),
            amount: output_amount,
        };
        let anchor = ledger.tip().hash.clone();
        let difficulty_bits = ledger.current_mine_difficulty_bits();
        for nonce in 0..u64::MAX {
            let salt = 1;
            let signature = mine_signature(&output, &anchor, salt, nonce, difficulty_bits, fee);
            if hash_meets_difficulty(&signature, difficulty_bits) {
                return Transaction::Mine {
                    output,
                    anchor,
                    salt,
                    nonce,
                    difficulty_bits,
                    fee,
                    proof_header: None,
                    signature,
                };
            }
        }
        panic!("expected to find mine proof");
    }

    #[test]
    fn wallet_utxos_only_include_outputs_owned_by_address() {
        let alice = Wallet::from_seed("wallet-utxos-alice");
        let bob = Wallet::from_seed("wallet-utxos-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3]);
        ledger.utxos.insert(
            OutPoint {
                txid: "bob-utxo".to_string(),
                index: 0,
            },
            TxOutput {
                address: bob.address().to_string(),
                amount: 5,
            },
        );

        let alice_utxos = ledger.utxos_for_address(alice.address());
        let total = alice_utxos
            .iter()
            .map(|(_, output)| output.amount)
            .sum::<Amount>();

        assert_eq!(alice_utxos.len(), 2);
        assert_eq!(total, ledger.balance_of(alice.address()));
        assert!(
            alice_utxos
                .iter()
                .all(|(_, output)| output.address == alice.address())
        );
    }

    #[test]
    fn transfer_combines_multiple_small_utxos_to_cover_amount_and_fee() {
        let alice = Wallet::from_seed("combine-small-utxos-alice");
        let bob = Wallet::from_seed("combine-small-utxos-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 1]);

        let tx = ledger.build_transfer(&alice, bob.address(), 2, 1).unwrap();

        let Transaction::Transfer {
            inputs,
            outputs,
            fee,
            ..
        } = &tx
        else {
            panic!("expected transfer");
        };
        assert_eq!(inputs.len(), 3);
        assert_eq!(*fee, 1);
        assert_eq!(
            outputs,
            &[TxOutput {
                address: bob.address().to_string(),
                amount: 2
            }]
        );

        ledger.submit_transaction(tx).unwrap();
        let balances = pending_balances(&ledger);
        assert_eq!(
            balances.get(alice.address()).copied().unwrap_or_default(),
            0
        );
        assert_eq!(balances.get(bob.address()).copied().unwrap_or_default(), 2);
    }

    #[test]
    fn transfer_returns_change_when_combined_utxos_exceed_payment() {
        let alice = Wallet::from_seed("combine-change-alice");
        let bob = Wallet::from_seed("combine-change-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);

        let tx = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();

        let Transaction::Transfer {
            inputs, outputs, ..
        } = &tx
        else {
            panic!("expected transfer");
        };
        assert_eq!(inputs.len(), 3);
        assert_eq!(
            outputs,
            &[
                TxOutput {
                    address: bob.address().to_string(),
                    amount: 3
                },
                TxOutput {
                    address: alice.address().to_string(),
                    amount: 1
                }
            ]
        );

        ledger.submit_transaction(tx).unwrap();
        let balances = pending_balances(&ledger);
        assert_eq!(balances.get(alice.address()).copied(), Some(1));
        assert_eq!(balances.get(bob.address()).copied(), Some(3));
    }

    #[test]
    fn transfer_can_spend_selected_utxos_when_they_cover_amount_and_fee() {
        let alice = Wallet::from_seed("selected-utxos-alice");
        let bob = Wallet::from_seed("selected-utxos-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
        let selected = vec![OutPoint {
            txid: "test-utxo-2".to_string(),
            index: 0,
        }];

        let tx = ledger
            .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
            .unwrap();

        let Transaction::Transfer {
            inputs, outputs, ..
        } = &tx
        else {
            panic!("expected transfer");
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].outpoint, selected[0]);
        assert_eq!(
            outputs,
            &[
                TxOutput {
                    address: bob.address().to_string(),
                    amount: 2
                },
                TxOutput {
                    address: alice.address().to_string(),
                    amount: 2
                }
            ]
        );

        ledger.submit_transaction(tx).unwrap();
        let balances = pending_balances(&ledger);
        assert_eq!(balances.get(bob.address()).copied(), Some(2));
        assert_eq!(balances.get(alice.address()).copied(), Some(7));
    }

    #[test]
    fn transfer_rejects_selected_utxos_that_do_not_cover_amount_plus_fee() {
        let alice = Wallet::from_seed("selected-utxos-insufficient-alice");
        let bob = Wallet::from_seed("selected-utxos-insufficient-bob");
        let ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
        let selected = vec![OutPoint {
            txid: "test-utxo-0".to_string(),
            index: 0,
        }];

        let error = ledger
            .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
            .unwrap_err();

        assert!(format!("{error:#}").contains("selected UTXOs do not cover"));
    }

    #[test]
    fn transfer_rejects_selected_utxos_owned_by_someone_else() {
        let alice = Wallet::from_seed("selected-utxos-owner-alice");
        let bob = Wallet::from_seed("selected-utxos-owner-bob");
        let carol = Wallet::from_seed("selected-utxos-owner-carol");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[5]);
        ledger.utxos.insert(
            OutPoint {
                txid: "carol-utxo".to_string(),
                index: 0,
            },
            TxOutput {
                address: carol.address().to_string(),
                amount: 5,
            },
        );
        let selected = vec![OutPoint {
            txid: "carol-utxo".to_string(),
            index: 0,
        }];

        let error = ledger
            .build_transfer_with_inputs(&alice, bob.address(), 2, 1, &selected)
            .unwrap_err();

        assert!(format!("{error:#}").contains("is not owned"));
    }

    #[test]
    fn transfer_rejects_when_combined_utxos_do_not_cover_amount_plus_fee() {
        let alice = Wallet::from_seed("combine-insufficient-alice");
        let bob = Wallet::from_seed("combine-insufficient-bob");
        let ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 1]);

        let error = ledger
            .build_transfer(&alice, bob.address(), 3, 1)
            .unwrap_err();

        assert!(format!("{error:#}").contains("insufficient funds"));
    }

    #[test]
    fn pending_change_from_combined_utxos_can_fund_next_transaction() {
        let alice = Wallet::from_seed("combine-pending-change-alice");
        let bob = Wallet::from_seed("combine-pending-change-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);

        let first = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
        let first_signature = first.signature().to_string();
        ledger.submit_transaction(first).unwrap();

        let second = ledger.build_transfer(&alice, bob.address(), 1, 0).unwrap();
        let Transaction::Transfer { inputs, .. } = &second else {
            panic!("expected transfer");
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].outpoint.txid, first_signature);
        assert_eq!(inputs[0].outpoint.index, 1);

        ledger.submit_transaction(second).unwrap();
        let balances = pending_balances(&ledger);
        assert_eq!(
            balances.get(alice.address()).copied().unwrap_or_default(),
            0
        );
        assert_eq!(balances.get(bob.address()).copied(), Some(4));
    }

    #[test]
    fn winning_burn_ticket_is_consumed_even_when_window_remains() {
        let mut tickets = vec![
            BurnTicket {
                id: "high-burn".to_string(),
                owner: "alice".to_string(),
                amount: 10_000,
                eligible_from_height: 4,
                eligible_until_height: 6,
            },
            BurnTicket {
                id: "small-burn".to_string(),
                owner: "bob".to_string(),
                amount: 1,
                eligible_from_height: 5,
                eligible_until_height: 7,
            },
        ];
        let block = Block::new(BlockDraft {
            height: 4,
            prev_hash: "0".repeat(64),
            timestamp_ms: 1,
            miner: "alice".to_string(),
            finalizer_rank: 0,
            reward: BLOCK_REWARD,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: Some(LeaderProof {
                ticket_id: "high-burn".to_string(),
                public_key: "alice".to_string(),
                signature: "signature".to_string(),
            }),
            transactions: Vec::new(),
        });

        consume_leader_ticket(&block, &mut tickets).unwrap();

        assert!(
            tickets.iter().all(|ticket| ticket.id != "high-burn"),
            "a winning burn must not remain eligible for the rest of its window"
        );
        assert!(
            tickets.iter().any(|ticket| ticket.id == "small-burn"),
            "unselected future tickets should remain pending"
        );
    }

    #[test]
    fn mine_fee_cannot_exceed_reward() {
        let alice = Wallet::from_seed("mine-fee-too-high-alice");
        let ledger = ledger_with_allocation(&alice, MICRO_IUNA);

        let error = ledger
            .build_mine_with_fee(alice.address(), MINE_REWARD + 1)
            .unwrap_err();

        assert!(format!("{error:#}").contains("fee exceeds reward"));
    }

    #[test]
    fn mine_output_plus_fee_must_equal_reward_even_with_valid_pow() {
        let alice = Wallet::from_seed("mine-invalid-split-alice");
        let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);
        let forged = mine_with_output_and_fee(&ledger, alice.address(), MINE_REWARD, 1);

        let error = ledger.submit_transaction(forged).unwrap_err();

        assert!(format!("{error:#}").contains("mine transaction reward is invalid"));
    }

    #[test]
    fn mine_fee_can_take_entire_reward_for_finalizer() {
        let alice = Wallet::from_seed("mine-full-fee-alice");
        let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);

        let mine = ledger
            .build_mine_with_fee(alice.address(), MINE_REWARD)
            .unwrap();

        assert_eq!(mine.amount(), 0);
        assert_eq!(mine.fee(), MINE_REWARD);
        assert!(ledger.submit_transaction(mine).unwrap());
    }

    #[test]
    fn block_selection_prefers_higher_fee_mine_action_when_space_is_limited() {
        let alice = Wallet::from_seed("mine-fee-priority-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);
        ledger.launch_profile.max_block_transactions = 2;

        let low_fee_mine = ledger
            .build_mine_with_fee(alice.address(), MICRO_IUNA / 100)
            .unwrap();
        let high_fee_mine = ledger
            .build_mine_with_fee(alice.address(), MICRO_IUNA / 2)
            .unwrap();
        let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(low_fee_mine.clone()).unwrap();
        ledger.submit_transaction(high_fee_mine.clone()).unwrap();
        ledger.submit_transaction(burn).unwrap();

        let block = ledger.mine_next_block(&alice, 1).unwrap();

        assert_eq!(block.transactions.len(), 2);
        assert!(block.transactions.iter().any(Transaction::is_burn));
        assert!(
            block
                .transactions
                .iter()
                .any(|tx| tx.signature() == high_fee_mine.signature())
        );
        assert!(
            block
                .transactions
                .iter()
                .all(|tx| tx.signature() != low_fee_mine.signature())
        );
        assert_eq!(block.reward, high_fee_mine.fee());
    }

    #[test]
    fn block_selection_can_include_multiple_mine_actions() {
        let alice = Wallet::from_seed("mine-multiple-actions-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

        let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let first_mine = ledger
            .build_mine_with_fee(alice.address(), MICRO_IUNA / 100)
            .unwrap();
        ledger.submit_transaction(first_mine.clone()).unwrap();
        let second_mine = ledger
            .build_mine_with_fee(alice.address(), MICRO_IUNA / 100)
            .unwrap();
        ledger.submit_transaction(second_mine.clone()).unwrap();

        assert_eq!(ledger.pending().len(), 3);
        let block = ledger.mine_next_block(&alice, 1).unwrap();

        assert_eq!(block.transactions.len(), 3);
        assert!(block.transactions.iter().any(Transaction::is_burn));
        assert!(
            block
                .transactions
                .iter()
                .any(|tx| tx.signature() == first_mine.signature())
        );
        assert!(
            block
                .transactions
                .iter()
                .any(|tx| tx.signature() == second_mine.signature())
        );
        assert_ne!(first_mine.signature(), second_mine.signature());
        assert_eq!(block.reward, first_mine.fee() + second_mine.fee());
    }

    #[test]
    fn mine_difficulty_increases_when_issuance_exceeds_target_window() {
        let alice = Wallet::from_seed("mine-difficulty-up-alice");
        let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);

        for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
            mine_burn_block_with_mines(&mut ledger, &alice, 2);
        }

        assert_eq!(
            ledger.current_mine_difficulty_bits(),
            MINE_DIFFICULTY_BITS + 1
        );
        let mine = ledger.build_mine(alice.address()).unwrap();
        let Transaction::Mine {
            difficulty_bits, ..
        } = mine
        else {
            panic!("expected mine action");
        };
        assert_eq!(difficulty_bits, MINE_DIFFICULTY_BITS + 1);
    }

    #[test]
    fn mine_difficulty_decreases_when_issuance_is_below_target_window() {
        let alice = Wallet::from_seed("mine-difficulty-down-alice");
        let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);

        for _ in 0..MINE_RETARGET_WINDOW_BLOCKS {
            mine_burn_block_with_mines(&mut ledger, &alice, 0);
        }

        assert_eq!(
            ledger.current_mine_difficulty_bits(),
            MINE_DIFFICULTY_BITS - MINE_MAX_RETARGET_STEP_BITS
        );
    }

    #[test]
    fn mine_actions_expire_when_anchor_is_too_old() {
        let alice = Wallet::from_seed("mine-anchor-expiry-alice");
        let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
        let stale_mine = ledger.build_mine(alice.address()).unwrap();

        for _ in 0..=MINE_MAX_ANCHOR_AGE_BLOCKS {
            mine_burn_block_with_mines(&mut ledger, &alice, 0);
        }

        let error = ledger.submit_transaction(stale_mine).unwrap_err();
        assert!(format!("{error:#}").contains("mine transaction anchor is too old"));
    }

    #[test]
    fn pending_mine_actions_are_removed_when_anchor_expires() {
        let alice = Wallet::from_seed("pending-mine-anchor-expiry-alice");
        let bob = Wallet::from_seed("pending-mine-anchor-expiry-bob");
        let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
        ledger.launch_profile.max_block_transactions = 1;
        let stale_mine = ledger.build_mine(bob.address()).unwrap();
        ledger.submit_transaction(stale_mine.clone()).unwrap();

        for _ in 0..=MINE_MAX_ANCHOR_AGE_BLOCKS {
            mine_burn_block_with_mines(&mut ledger, &alice, 0);
        }

        assert!(
            ledger
                .pending()
                .iter()
                .all(|tx| tx.signature() != stale_mine.signature())
        );
    }

    #[test]
    fn stratum_mine_header_proof_is_validated() {
        let alice = Wallet::from_seed("stratum-proof-alice");
        let ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
        let anchor = ledger.tip().hash.clone();
        let difficulty_bits = ledger.current_mine_difficulty_bits();
        let template = ledger
            .stratum_mine_template(alice.address(), 0, anchor, 1, difficulty_bits)
            .unwrap();

        let mut accepted = None;
        for nonce in 0_u32..50_000 {
            let result = ledger.build_stratum_mine(
                template.clone(),
                StratumMineShare {
                    extranonce2: [0, 0, 0, 0],
                    header_nonce: nonce.to_le_bytes(),
                },
            );
            if let Ok(tx) = result {
                accepted = Some(tx);
                break;
            }
        }

        let tx = accepted.expect("expected Stratum proof within search range");
        let Transaction::Mine {
            proof_header,
            signature,
            ..
        } = tx
        else {
            panic!("expected mine action");
        };
        assert_eq!(proof_header.as_deref().unwrap_or_default().len(), 160);
        assert!(hash_meets_difficulty(&signature, difficulty_bits));
    }

    #[test]
    fn stratum_mine_salt_allows_multiple_actions_for_same_anchor() {
        let alice = Wallet::from_seed("stratum-salt-alice");
        let mut ledger = ledger_with_allocation(&alice, 100 * MICRO_IUNA);
        let anchor = ledger.tip().hash.clone();
        let difficulty_bits = ledger.current_mine_difficulty_bits();

        for salt in [1, 2] {
            let template = ledger
                .stratum_mine_template(alice.address(), 0, anchor.clone(), salt, difficulty_bits)
                .unwrap();
            let mut accepted = None;
            for nonce in 0_u32..50_000 {
                let result = ledger.build_stratum_mine(
                    template.clone(),
                    StratumMineShare {
                        extranonce2: [0, 0, 0, 0],
                        header_nonce: nonce.to_le_bytes(),
                    },
                );
                if let Ok(tx) = result {
                    accepted = Some(tx);
                    break;
                }
            }
            let tx = accepted.expect("expected Stratum proof within search range");
            assert!(ledger.submit_transaction(tx).unwrap());
        }

        assert_eq!(ledger.pending().len(), 2);
        let salts = ledger
            .pending()
            .iter()
            .map(|tx| match tx {
                Transaction::Mine { salt, .. } => *salt,
                _ => panic!("expected mine action"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(salts, BTreeSet::from([1, 2]));
    }
}
