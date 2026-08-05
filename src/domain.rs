use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use getrandom::getrandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Amount = u64;
pub const MICRO_IUNA: Amount = 1_000_000;
pub const BLOCK_REWARD: Amount = 100 * MICRO_IUNA;
pub const MINE_REWARD: Amount = MICRO_IUNA;
pub const MINE_FINALIZER_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_MINE_FEE: Amount = MINE_FINALIZER_FEE;
pub const DEFAULT_TRANSACTION_FEE: Amount = MICRO_IUNA;
pub const DEFAULT_FEE_PER_BYTE: Amount = 1;
pub const MAX_BLOCK_BYTES: usize = 100_000;
pub const VDF_TARGET_BLOCK_MS: u64 = 5 * 60 * 1_000;
pub const RECOVERY_BLOCK_DELAY_MS: u64 = VDF_TARGET_BLOCK_MS * 6;
pub const MAX_VDF_ROUNDS: u64 = i64::MAX as u64;
pub const MINE_DIFFICULTY_BITS: u32 = 12;
pub const MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS: u64 = 20;
pub const REVEAL_COMMITTEE_SIZE: usize = 3;
pub const MAX_REVEAL_BUNDLE_BYTES: usize = 10_000;
const MINE_RETARGET_WINDOW_BLOCKS: u64 = 10;
const MINE_TARGET_ACTIONS_PER_BLOCK: u64 = 1;
const MINE_MAX_RETARGET_STEP_BITS: u32 = 2;
const MINE_MIN_DIFFICULTY_BITS: u32 = 1;
const MINE_MAX_DIFFICULTY_BITS: u32 = 32;
const MINE_MAX_ANCHOR_AGE_BLOCKS: u64 = MINE_RETARGET_WINDOW_BLOCKS;
pub const MAX_PENDING_TRANSACTIONS: usize = 10_000;
const MAX_ORPHAN_TRANSACTIONS: usize = 1_024;
const MAX_BLOCK_TRANSACTIONS: usize = 1_000;
const DEFAULT_TICKET_MATURITY_DELAY: u64 = 3;
const DEFAULT_TICKET_EXPIRY_WINDOW: u64 = 3;
const MIN_VDF_ROUNDS: u64 = 1;
const VDF_RETARGET_WINDOW_BLOCKS: usize = 20;
const MAX_VDF_RETARGET_STEP_PERCENT: u128 = 2;
const VDF_RETARGET_DEADBAND_PERCENT: u128 = 10;
const MIN_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS / 4;
const MAX_VDF_RETARGET_OBSERVED_BLOCK_MS: u64 = VDF_TARGET_BLOCK_MS * 4;
const MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS: u64 = 2 * 60 * 1_000;
const BLOCK_MEDIAN_TIME_PAST_WINDOW: usize = 11;
const FORK_FINALITY_DEPTH: u64 = 6;
const VDF_MODULUS: u128 = 4_611_685_975_477_714_963;
const VDF_CHALLENGE_MIN: u64 = 1_073_741_827;
const WALLET_SEED_DOMAIN: &str = "iuna-wallet-seed";
const PUBLIC_KEY_BYTES: usize = 32;
const HASH_BYTES: usize = 32;
const SIGNATURE_BYTES: usize = 64;
const BLINDED_KEY_BYTES: usize = 32;
const BLINDED_NONCE_BYTES: usize = 12;
const STRATUM_MINE_HEADER_BYTES: usize = 80;

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
        let seed =
            decode_hex_array::<PUBLIC_KEY_BYTES>(&self.secret).expect("wallet secret is valid hex");
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

    fn reveal_bundle(&self, payload: RevealBundlePayload) -> RevealBundle {
        let signature = self.sign_payload(&payload.canonical());
        RevealBundle {
            height: payload.height,
            prev_hash: payload.prev_hash,
            slot: payload.slot,
            member: self.address.clone(),
            reveals: payload.reveals,
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
        recipient: String,
        anchor: String,
        #[serde(default)]
        salt: u64,
        nonce: u64,
        difficulty_bits: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof_header: Option<String>,
        signature: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlindedTransaction {
    pub commitment: String,
    pub fee: Amount,
    pub encrypted_size: u32,
    pub expires_at_height: u64,
    pub nonce: String,
    pub ciphertext: String,
    pub payload_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlindedReveal {
    pub commitment: String,
    pub key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltBlindedTransaction {
    pub payload: Transaction,
    pub transaction: BlindedTransaction,
    pub reveal: BlindedReveal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedBlindedTransaction {
    pub transaction: BlindedTransaction,
    pub payload: Transaction,
    pub reveal: BlindedReveal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevealedBlindedTransaction {
    pub height: u64,
    pub commitment: String,
    pub included_by: String,
    pub transaction: Transaction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveBlindedTransaction {
    transaction: BlindedTransaction,
    included_height: u64,
    included_by: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MineSearchOutcome {
    pub transaction: Option<Transaction>,
    pub next_nonce: u64,
    pub attempts: u64,
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
            Self::Mine { recipient, .. } => recipient.as_str(),
        }
    }

    pub fn to(&self) -> Option<&str> {
        match self {
            Self::Transfer { outputs, .. } => outputs.first().map(|output| output.address.as_str()),
            Self::Burn { .. } => None,
            Self::Mine { recipient, .. } => Some(recipient.as_str()),
        }
    }

    pub fn amount(&self) -> Amount {
        match self {
            Self::Transfer { outputs, .. } => {
                outputs.first().map(|output| output.amount).unwrap_or(0)
            }
            Self::Burn { amount, .. } => *amount,
            Self::Mine { .. } => MINE_REWARD,
        }
    }

    pub fn fee(&self) -> Amount {
        match self {
            Self::Transfer { fee, .. } | Self::Burn { fee, .. } => *fee,
            Self::Mine { .. } => MINE_FINALIZER_FEE,
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

    pub fn economic_size_bytes(&self) -> usize {
        canonical_transaction_size_bytes(self)
    }

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize transaction for size check")
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
                recipient,
                anchor,
                salt,
                nonce,
                difficulty_bits,
                ..
            } => mine_payload(recipient, anchor, *salt, *nonce, *difficulty_bits),
        }
    }

    fn verify_signature(&self) -> Result<()> {
        if let Self::Mine {
            recipient,
            anchor,
            salt,
            nonce,
            difficulty_bits,
            proof_header,
            signature,
        } = self
        {
            let expected = if let Some(proof_header) = proof_header {
                let header =
                    stratum_mine_header_bytes(recipient, anchor, *salt, *nonce, *difficulty_bits)?;
                let expected_header = hex_encode(header);
                if *proof_header != expected_header {
                    bail!("mine transaction proof header is invalid");
                }
                stratum_mine_signature(&header)
            } else {
                mine_signature(recipient, anchor, *salt, *nonce, *difficulty_bits)
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
        let public_key = decode_hex_array::<PUBLIC_KEY_BYTES>(sender)
            .with_context(|| format!("invalid public key for {sender}"))?;
        let signature = decode_hex_array::<SIGNATURE_BYTES>(self.signature())
            .context("invalid signature hex")?;
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

    fn outputs(&self) -> Vec<TxOutput> {
        match self {
            Self::Transfer { outputs, .. } => outputs.clone(),
            Self::Burn { change, .. } => change.clone(),
            Self::Mine { recipient, .. } => vec![TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            }],
        }
    }

    fn inputs_are_genesis_signed(&self) -> bool {
        self.inputs()
            .iter()
            .all(|input| input.signature == "genesis")
    }
}

impl BlindedTransaction {
    pub fn id(&self) -> &str {
        &self.commitment
    }

    pub fn canonical(&self) -> String {
        format!(
            "blinded-tx:{}:{}:{}:{}:{}:{}",
            self.fee,
            self.encrypted_size,
            self.expires_at_height,
            self.nonce,
            self.ciphertext,
            self.payload_hash
        )
    }

    pub fn fee_rate_size_bytes(&self) -> usize {
        self.serialized_size_bytes()
            .unwrap_or(self.encrypted_size as usize)
    }

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize blinded transaction for size check")
    }
}

impl BlindedReveal {
    pub fn canonical(&self) -> String {
        format!("blinded-reveal:{}:{}", self.commitment, self.key)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealBundle {
    pub height: u64,
    pub prev_hash: String,
    pub slot: u8,
    pub member: String,
    pub reveals: Vec<BlindedReveal>,
    pub signature: String,
}

impl RevealBundle {
    pub fn canonical_payload(&self) -> String {
        RevealBundlePayload {
            height: self.height,
            prev_hash: self.prev_hash.clone(),
            slot: self.slot,
            member: self.member.clone(),
            reveals: self.reveals.clone(),
        }
        .canonical()
    }

    pub fn canonical(&self) -> String {
        format!("{}:{}", self.canonical_payload(), self.signature)
    }

    pub fn bundle_hash(&self) -> String {
        hex_hash(self.canonical())
    }

    pub fn serialized_size_bytes(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .context("failed to serialize reveal bundle for size check")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RevealBundlePayload {
    height: u64,
    prev_hash: String,
    slot: u8,
    member: String,
    reveals: Vec<BlindedReveal>,
}

impl RevealBundlePayload {
    fn canonical(&self) -> String {
        let reveals = self
            .reveals
            .iter()
            .map(BlindedReveal::canonical)
            .collect::<Vec<_>>()
            .join("|");
        format!(
            "iuna-reveal-bundle-v1:{}:{}:{}:{}:{}",
            self.height, self.prev_hash, self.slot, self.member, reveals
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealCommitteeMember {
    pub slot: u8,
    pub rank: u32,
    pub ticket_id: String,
    pub owner: String,
    pub amount: Amount,
}

fn canonical_blinded_block_items(blinded: &str, reveals: &str, bundles: &str) -> String {
    format!("blinded-v2:{blinded}:reveals:{reveals}:bundles:{bundles}")
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
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> String {
    format!("iuna-mine:{recipient}:{anchor}:{salt}:{nonce}:{difficulty_bits}")
}

fn mine_signature(
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> String {
    hex_hash(mine_payload(
        recipient,
        anchor,
        salt,
        nonce,
        difficulty_bits,
    ))
}

pub const STRATUM_EXTRANONCE1_HEX: &str = "00000000";
pub const STRATUM_EXTRANONCE2_SIZE: usize = 4;
const STRATUM_MINE_VERSION: [u8; 4] = [1, 0, 0, 0];
const STRATUM_MINE_NTIME: [u8; 4] = [0, 0, 0, 0];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StratumMineTemplate {
    pub recipient: String,
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
    recipient: &str,
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
) -> Vec<u8> {
    format!("iuna-stratum-mine:{recipient}:{anchor}:{salt}:{difficulty_bits}:").into_bytes()
}

fn stratum_coinbase_bytes(
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Vec<u8> {
    let (extranonce2, _) = unpack_stratum_nonce(nonce);
    let mut coinbase = stratum_coinbase_prefix(recipient, anchor, salt, difficulty_bits);
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
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Result<[u8; 80]> {
    let mut header = [0_u8; 80];
    header[0..4].copy_from_slice(&STRATUM_MINE_VERSION);
    let anchor_bytes =
        decode_hex_array::<HASH_BYTES>(anchor).context("mine transaction anchor is not hex")?;
    header[4..36].copy_from_slice(&anchor_bytes);
    let merkle_root = double_sha256(&stratum_coinbase_bytes(
        recipient,
        anchor,
        salt,
        nonce,
        difficulty_bits,
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
    anchor: &str,
    salt: u64,
    difficulty_bits: u32,
) -> Result<StratumMineTemplate> {
    let recipient = recipient.into();
    validate_address(&recipient, "mine recipient")?;
    validate_hash(anchor, "mine transaction anchor")?;
    let anchor_bytes =
        decode_hex_array::<HASH_BYTES>(anchor).context("mine transaction anchor is not hex")?;
    Ok(StratumMineTemplate {
        recipient: recipient.clone(),
        anchor: anchor.to_string(),
        salt,
        difficulty_bits,
        coinbase_prefix: stratum_coinbase_prefix(&recipient, anchor, salt, difficulty_bits),
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
    pub finalizer_mode: FinalizerMode,
    #[serde(default)]
    pub finalizer_rank: u32,
    pub reward: Amount,
    pub vdf_rounds: u64,
    pub vdf_output: String,
    pub leader_proof: Option<LeaderProof>,
    #[serde(default)]
    pub blinded_transactions: Vec<BlindedTransaction>,
    #[serde(default)]
    pub reveal_bundles: Vec<RevealBundle>,
    pub transactions: Vec<Transaction>,
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
    fn new(draft: BlockDraft) -> Self {
        let mut block = Self {
            height: draft.height,
            prev_hash: draft.prev_hash,
            timestamp_ms: draft.timestamp_ms,
            miner: draft.miner,
            finalizer_mode: draft.finalizer_mode,
            finalizer_rank: draft.finalizer_rank,
            reward: draft.reward,
            vdf_rounds: draft.vdf_rounds,
            vdf_output: draft.vdf_output,
            leader_proof: draft.leader_proof,
            blinded_transactions: draft.blinded_transactions,
            reveal_bundles: draft.reveal_bundles,
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
        let bundle_hashes = self.reveal_bundle_hashes();
        match self.finalizer_mode {
            FinalizerMode::Ticket => {
                vdf_seed_for_child(&self.prev_hash, self.height, &bundle_hashes)
            }
            FinalizerMode::Recovery => recovery_vdf_seed_for_child(
                &self.prev_hash,
                self.height,
                self.timestamp_ms,
                &bundle_hashes,
            ),
        }
    }

    fn content_hash(&self) -> String {
        let txs = self
            .transactions
            .iter()
            .map(Transaction::canonical)
            .collect::<Vec<_>>()
            .join("|");
        let blinded = self
            .blinded_transactions
            .iter()
            .map(BlindedTransaction::canonical)
            .collect::<Vec<_>>()
            .join("|");
        let reveals = self
            .all_blinded_reveals()
            .iter()
            .map(|reveal| reveal.canonical())
            .collect::<Vec<_>>()
            .join("|");
        let bundles = self
            .reveal_bundles
            .iter()
            .map(RevealBundle::canonical)
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
        if !self.blinded_transactions.is_empty() || !self.reveal_bundles.is_empty() {
            return hex_hash(format!(
                "block-content-v3:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.timestamp_ms,
                self.miner,
                self.finalizer_rank,
                self.reward,
                self.vdf_rounds,
                leader_proof,
                txs,
                canonical_blinded_block_items(&blinded, &reveals, &bundles)
            ));
        }
        hex_hash(format!(
            "{}:{}",
            self.legacy_content_hash_prefix(&leader_proof),
            txs
        ))
    }

    fn legacy_content_hash_prefix(&self, leader_proof: &str) -> String {
        if self.finalizer_mode == FinalizerMode::Recovery {
            format!(
                "block-content-recovery-v1:{}:{}:{}:{}:{}:{}:{}",
                self.height,
                self.prev_hash,
                self.timestamp_ms,
                self.miner,
                self.reward,
                self.vdf_rounds,
                leader_proof
            )
        } else if self.finalizer_rank == 0 {
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
            finalizer_mode_rank: self.finalizer_mode.fork_choice_rank(),
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

    pub fn all_blinded_reveals(&self) -> Vec<&BlindedReveal> {
        let mut seen = BTreeSet::new();
        self.reveal_bundles
            .iter()
            .flat_map(|bundle| bundle.reveals.iter())
            .filter(|reveal| seen.insert(reveal.commitment.clone()))
            .collect()
    }

    pub fn reveal_bundle_hashes(&self) -> [String; REVEAL_COMMITTEE_SIZE] {
        reveal_bundle_hashes(&self.reveal_bundles)
    }

    pub fn included_reveal_bundle_count(&self) -> usize {
        self.reveal_bundles.len()
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
    height: u64,
    prev_hash: String,
    timestamp_ms: u64,
    miner: String,
    finalizer_mode: FinalizerMode,
    finalizer_rank: u32,
    reward: Amount,
    vdf_rounds: u64,
    vdf_seed: String,
    leader_ticket: Option<BurnTicket>,
    blinded_transactions: Vec<BlindedTransaction>,
    reveal_bundles: Vec<RevealBundle>,
    transactions: Vec<Transaction>,
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
        Block::new(BlockDraft {
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
            blinded_transactions: self.blinded_transactions,
            reveal_bundles: self.reveal_bundles,
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
    finalizer_mode: FinalizerMode,
    finalizer_rank: u32,
    reward: Amount,
    vdf_rounds: u64,
    vdf_output: String,
    leader_proof: Option<LeaderProof>,
    blinded_transactions: Vec<BlindedTransaction>,
    reveal_bundles: Vec<RevealBundle>,
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
    pub vdf_rounds: u64,
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
    finalizer_mode_rank: u8,
    finalizer_rank: u32,
    proof_rank: String,
}

impl Ord for LeaderScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.finalizer_mode_rank
            .cmp(&other.finalizer_mode_rank)
            .then_with(|| self.finalizer_rank.cmp(&other.finalizer_rank))
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BlockSelection {
    transactions: Vec<Transaction>,
    blinded_transactions: Vec<BlindedTransaction>,
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
    orphans: Vec<Transaction>,
    pending_blinded: Vec<BlindedTransaction>,
    pending_reveals: Vec<BlindedReveal>,
    active_blinded: BTreeMap<String, ActiveBlindedTransaction>,
    mine_reward: Amount,
    initial_vdf_rounds: u64,
    vdf_rounds: u64,
    launch_profile: LaunchProfile,
}

impl Ledger {
    pub fn new(genesis_allocations: BTreeMap<String, Amount>, vdf_rounds: u64) -> Self {
        Self::new_with_genesis_transactions(genesis_allocations, Vec::new(), vdf_rounds)
            .expect("empty genesis transactions are valid")
    }

    pub fn new_with_genesis_burns(
        genesis_allocations: BTreeMap<String, Amount>,
        genesis_burns: Vec<GenesisBurn>,
        vdf_rounds: u64,
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
        vdf_rounds: u64,
    ) -> Result<Self> {
        validate_genesis_allocations(&genesis_allocations)?;
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
            orphans: Vec::new(),
            pending_blinded: Vec::new(),
            pending_reveals: Vec::new(),
            active_blinded: BTreeMap::new(),
            mine_reward: MINE_REWARD,
            initial_vdf_rounds: vdf_rounds,
            vdf_rounds,
            launch_profile,
        })
    }

    pub fn from_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, unix_now_ms())
    }

    pub fn from_persisted_snapshot(snapshot: ChainSnapshot) -> Result<Self> {
        Self::from_snapshot_at(snapshot, u64::MAX)
    }

    pub(crate) fn from_snapshot_at(snapshot: ChainSnapshot, now_ms: u64) -> Result<Self> {
        Self::from_snapshot_with_vdf_policy(snapshot, true, now_ms)
    }

    fn from_snapshot_with_vdf_policy(
        snapshot: ChainSnapshot,
        verify_vdf: bool,
        now_ms: u64,
    ) -> Result<Self> {
        let ChainSnapshot {
            genesis_allocations,
            vdf_rounds,
            launch_profile,
            blocks,
        } = snapshot;

        if blocks.is_empty() {
            bail!("chain snapshot is empty");
        }

        validate_genesis_allocations(&genesis_allocations)?;
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
            orphans: Vec::new(),
            pending_blinded: Vec::new(),
            pending_reveals: Vec::new(),
            active_blinded: BTreeMap::new(),
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
                ledger.apply_block_at(block, now_ms)?;
            } else {
                ledger.apply_preverified_block_at(block, now_ms)?;
            }
        }
        Ok(ledger)
    }

    pub fn extend_from_snapshot(&mut self, snapshot: ChainSnapshot) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, true, unix_now_ms())
    }

    pub(crate) fn extend_from_preverified_snapshot_at(
        &mut self,
        snapshot: ChainSnapshot,
        now_ms: u64,
    ) -> Result<bool> {
        self.extend_from_snapshot_with_vdf_policy(snapshot, false, now_ms)
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
        now_ms: u64,
    ) -> Result<bool> {
        self.validate_snapshot_identity(&snapshot)?;
        let candidate = Self::from_snapshot_with_vdf_policy(snapshot, verify_vdf, now_ms)?;
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
        carry_forward.extend(self.orphans.clone());
        let mut carry_forward_blinded = self.pending_blinded.clone();
        let mut carry_forward_reveals = self.pending_reveals.clone();
        for block in self
            .chain
            .iter()
            .skip(fork_point.first_diverging_height() as usize)
        {
            carry_forward.extend(block.transactions.clone());
            carry_forward_blinded.extend(block.blinded_transactions.clone());
            carry_forward_reveals.extend(block.all_blinded_reveals().into_iter().cloned());
        }

        let mined_signatures = candidate
            .chain
            .iter()
            .flat_map(|block| block.transactions.iter())
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let mined_blinded_commitments = candidate
            .chain
            .iter()
            .flat_map(|block| block.blinded_transactions.iter())
            .map(|transaction| transaction.commitment.clone())
            .collect::<BTreeSet<_>>();
        let mined_reveal_commitments = candidate
            .chain
            .iter()
            .flat_map(|block| block.all_blinded_reveals())
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();

        for transaction in carry_forward {
            if !mined_signatures.contains(transaction.signature()) {
                let _ = candidate.submit_transaction(transaction);
            }
        }
        for transaction in carry_forward_blinded {
            if !mined_blinded_commitments.contains(&transaction.commitment) {
                let _ = candidate.submit_blinded_transaction(transaction);
            }
        }
        for reveal in carry_forward_reveals {
            if !mined_reveal_commitments.contains(&reveal.commitment) {
                let _ = candidate.submit_blinded_reveal(reveal);
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
            pending_transactions: self.pending.len()
                + self.pending_blinded.len()
                + self.pending_reveals.len(),
        }
    }

    pub fn chain(&self) -> &[Block] {
        &self.chain
    }

    pub fn burn_leader_ranks_for_block(&self, height: u64) -> Result<Vec<BurnLeaderRank>> {
        if height == 0 {
            return Ok(Vec::new());
        }
        let parent_index = height.checked_sub(1).context("block height underflows")? as usize;
        let parent = self
            .chain
            .get(parent_index)
            .with_context(|| format!("missing parent block for height {height}"))?;
        let mut tickets = genesis_tickets(
            &self.genesis_allocations,
            &self.chain[0],
            &self.launch_profile,
        )?;
        let mut active_blinded = BTreeMap::<String, ActiveBlindedTransaction>::new();
        for block in self
            .chain
            .iter()
            .skip(1)
            .take_while(|block| block.height < height)
        {
            apply_finalizer_ticket_effects(block, &mut tickets)?;
            tickets.extend(tickets_created_by_block(block, &self.launch_profile)?);
            let mut revealed_transactions = Vec::new();
            for reveal in block.all_blinded_reveals() {
                let active = active_blinded.get(&reveal.commitment).with_context(|| {
                    format!(
                        "block {} reveals unknown blinded transaction {}",
                        block.height, reveal.commitment
                    )
                })?;
                let transaction = decrypt_blinded_transaction(&active.transaction, reveal)?;
                if transaction.fee() != active.transaction.fee {
                    bail!(
                        "block {} blinded reveal fee does not match envelope",
                        block.height
                    );
                }
                revealed_transactions.push(transaction);
                active_blinded.remove(&reveal.commitment);
            }
            tickets.extend(tickets_created_by_transactions(
                block.height,
                &revealed_transactions,
                &self.launch_profile,
            )?);
            active_blinded.retain(|_, active| block.height < active.transaction.expires_at_height);
            for transaction in &block.blinded_transactions {
                active_blinded.insert(
                    transaction.commitment.clone(),
                    ActiveBlindedTransaction {
                        transaction: transaction.clone(),
                        included_height: block.height,
                        included_by: block.miner.clone(),
                    },
                );
            }
        }
        Ok(ranked_tickets_for_height(parent, height, &tickets)
            .into_iter()
            .enumerate()
            .map(|(rank, ticket)| BurnLeaderRank {
                rank: rank as u32,
                ticket_id: ticket.id,
                owner: ticket.owner,
                amount: ticket.amount,
                eligible_from_height: ticket.eligible_from_height,
                eligible_until_height: ticket.eligible_until_height,
            })
            .collect())
    }

    pub fn reveal_committee_for_next_block(&self) -> Vec<RevealCommitteeMember> {
        self.reveal_committee_for_height(self.tip().height + 1)
    }

    pub fn reveal_committee_for_height(&self, height: u64) -> Vec<RevealCommitteeMember> {
        let ranked = ranked_tickets_for_height(self.tip(), height, &self.tickets);
        let committee_start = ranked.len().saturating_sub(REVEAL_COMMITTEE_SIZE);
        ranked
            .into_iter()
            .enumerate()
            .skip(committee_start)
            .enumerate()
            .filter_map(|(slot, (rank, ticket))| {
                Some(RevealCommitteeMember {
                    slot: u8::try_from(slot).ok()?,
                    rank: u32::try_from(rank).ok()?,
                    ticket_id: ticket.id,
                    owner: ticket.owner,
                    amount: ticket.amount,
                })
            })
            .collect()
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

    pub fn pending_blinded_transactions(&self) -> &[BlindedTransaction] {
        &self.pending_blinded
    }

    pub fn pending_blinded_reveals(&self) -> &[BlindedReveal] {
        &self.pending_reveals
    }

    pub fn build_reveal_bundle(&self, wallet: &Wallet) -> Result<Option<RevealBundle>> {
        let height = self.tip().height + 1;
        let prev_hash = self.tip().hash.clone();
        let Some(member) = self
            .reveal_committee_for_next_block()
            .into_iter()
            .find(|member| member.owner == wallet.address())
        else {
            return Ok(None);
        };
        let mut reveals = self.valid_pending_blinded_reveals();
        reveals.sort_by(|left, right| {
            self.reveal_fee_order_key(right)
                .cmp(&self.reveal_fee_order_key(left))
                .then_with(|| left.commitment.cmp(&right.commitment))
        });

        let mut selected = Vec::new();
        for reveal in reveals {
            let mut candidate = selected.clone();
            candidate.push(reveal);
            let bundle = wallet.reveal_bundle(RevealBundlePayload {
                height,
                prev_hash: prev_hash.clone(),
                slot: member.slot,
                member: wallet.address().to_string(),
                reveals: candidate.clone(),
            });
            if bundle.serialized_size_bytes()? <= MAX_REVEAL_BUNDLE_BYTES {
                selected = candidate;
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        Ok(Some(wallet.reveal_bundle(RevealBundlePayload {
            height,
            prev_hash,
            slot: member.slot,
            member: wallet.address().to_string(),
            reveals: selected,
        })))
    }

    pub fn validate_next_block_reveal_bundles(
        &self,
        bundles: Vec<RevealBundle>,
    ) -> Result<Vec<RevealBundle>> {
        let expected_height = self.tip().height + 1;
        let expected_prev_hash = self.tip().hash.clone();
        self.validate_reveal_bundles_for_block(expected_height, &expected_prev_hash, bundles)
    }

    fn validate_reveal_bundles_for_block(
        &self,
        expected_height: u64,
        expected_prev_hash: &str,
        mut bundles: Vec<RevealBundle>,
    ) -> Result<Vec<RevealBundle>> {
        if bundles.len() > REVEAL_COMMITTEE_SIZE {
            bail!("block has too many reveal bundles");
        }
        if bundles.windows(2).any(|pair| pair[0].slot >= pair[1].slot) {
            bail!("reveal bundles are not in slot order");
        }
        bundles.sort_by_key(|bundle| bundle.slot);
        let committee = self
            .reveal_committee_for_height(expected_height)
            .into_iter()
            .map(|member| (member.slot, member))
            .collect::<BTreeMap<_, _>>();
        let mut seen_slots = BTreeSet::new();
        let mut seen_members = BTreeSet::new();
        for bundle in &bundles {
            if bundle.height != expected_height {
                bail!("reveal bundle height is invalid");
            }
            if bundle.prev_hash != expected_prev_hash {
                bail!("reveal bundle parent hash is invalid");
            }
            if usize::from(bundle.slot) >= REVEAL_COMMITTEE_SIZE {
                bail!("reveal bundle slot is invalid");
            }
            if !seen_slots.insert(bundle.slot) {
                bail!("duplicate reveal bundle slot");
            }
            if !seen_members.insert(bundle.member.clone()) {
                bail!("duplicate reveal bundle member");
            }
            let member = committee
                .get(&bundle.slot)
                .context("reveal bundle slot is not assigned")?;
            if bundle.member != member.owner {
                bail!("reveal bundle member is not assigned to slot");
            }
            if bundle.serialized_size_bytes()? > MAX_REVEAL_BUNDLE_BYTES {
                bail!("reveal bundle exceeds max size");
            }
            verify_address_signature(
                &bundle.member,
                &bundle.canonical_payload(),
                &bundle.signature,
                "reveal bundle",
            )?;
            let mut seen_bundle_reveals = BTreeSet::new();
            let mut previous_key: Option<((u128, Amount), String)> = None;
            for reveal in &bundle.reveals {
                if !seen_bundle_reveals.insert(reveal.commitment.clone()) {
                    bail!("duplicate blinded reveal in reveal bundle");
                }
                self.pending_reveal_transaction(reveal)?;
                let key = (self.reveal_fee_order_key(reveal), reveal.commitment.clone());
                if let Some((previous_fee_key, previous_commitment)) = &previous_key {
                    if key.0 > *previous_fee_key
                        || key.0 == *previous_fee_key && key.1 < *previous_commitment
                    {
                        bail!("reveal bundle is not fee ordered");
                    }
                }
                previous_key = Some(key);
            }
        }
        Ok(bundles)
    }

    pub fn orphan_transactions(&self) -> &[Transaction] {
        &self.orphans
    }

    pub fn transaction_by_signature(&self, signature: &str) -> Option<Transaction> {
        self.pending
            .iter()
            .chain(self.orphans.iter())
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

    pub fn has_blinded_transaction(&self, commitment: &str) -> bool {
        self.pending_blinded
            .iter()
            .any(|transaction| transaction.commitment == commitment)
            || self.active_blinded.contains_key(commitment)
            || self.chain.iter().any(|block| {
                block
                    .blinded_transactions
                    .iter()
                    .any(|tx| tx.commitment == commitment)
            })
    }

    pub fn has_unrevealed_blinded_transaction(&self, commitment: &str) -> bool {
        self.pending_blinded
            .iter()
            .any(|transaction| transaction.commitment == commitment)
            || self.active_blinded.contains_key(commitment)
    }

    pub fn has_active_blinded_transaction(&self, commitment: &str) -> bool {
        self.active_blinded.contains_key(commitment)
    }

    pub fn has_blinded_reveal(&self, commitment: &str) -> bool {
        self.pending_reveals
            .iter()
            .any(|reveal| reveal.commitment == commitment)
            || self.chain.iter().any(|block| {
                block
                    .all_blinded_reveals()
                    .iter()
                    .any(|reveal| reveal.commitment == commitment)
            })
    }

    pub fn vdf_rounds(&self) -> u64 {
        self.vdf_rounds
    }

    pub fn launch_profile(&self) -> &LaunchProfile {
        &self.launch_profile
    }

    pub fn current_mine_difficulty_bits(&self) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(self.tip().height)
    }

    pub fn mine_difficulty_bits_at_height(&self, height: u64) -> u32 {
        self.mine_difficulty_bits_for_anchor_height(height.min(self.tip().height))
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
            .utxos_after_spendable_pending()?
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
        let to = to.into();
        validate_address(&to, "transfer recipient")?;
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let (inputs, input_total) = self.select_inputs(wallet.address(), required)?;
        let mut outputs = vec![TxOutput {
            address: to,
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
        let to = to.into();
        validate_address(&to, "transfer recipient")?;
        let required = amount
            .checked_add(fee)
            .context("transfer amount plus fee overflows")?;
        let (inputs, input_total) =
            self.select_inputs_by_outpoint(wallet.address(), required, outpoints)?;
        let mut outputs = vec![TxOutput {
            address: to,
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

    pub fn build_blinded_burn(
        &self,
        wallet: &Wallet,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        let transaction = self.build_burn(wallet, amount, fee)?;
        self.blind_transaction(transaction, fee, expires_at_height)
    }

    pub fn build_blinded_transfer(
        &self,
        wallet: &Wallet,
        to: impl Into<String>,
        amount: Amount,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        let transaction = self.build_transfer(wallet, to, amount, fee)?;
        self.blind_transaction(transaction, fee, expires_at_height)
    }

    pub fn build_blinded_transaction(
        &self,
        transaction: Transaction,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        let fee = transaction.fee();
        self.blind_transaction(transaction, fee, expires_at_height)
    }

    fn blind_transaction(
        &self,
        transaction: Transaction,
        fee: Amount,
        expires_at_height: u64,
    ) -> Result<BuiltBlindedTransaction> {
        if expires_at_height <= self.height() {
            bail!("blinded transaction expiry must be in the future");
        }
        if expires_at_height
            > self
                .height()
                .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
        {
            bail!("blinded transaction expiry is too far in the future");
        }
        if fee != transaction.fee() {
            bail!("blinded transaction fee must match plaintext transaction fee");
        }
        let plaintext = serde_json::to_vec(&transaction)
            .context("failed to serialize transaction for blinded payload")?;
        let payload_hash = hex_hash(&plaintext);
        let payload = transaction;
        let mut key = [0_u8; BLINDED_KEY_BYTES];
        let mut nonce = [0_u8; BLINDED_NONCE_BYTES];
        getrandom(&mut key)
            .map_err(|error| anyhow!("failed to generate blinded transaction key: {error}"))?;
        getrandom(&mut nonce)
            .map_err(|error| anyhow!("failed to generate blinded transaction nonce: {error}"))?;
        let ciphertext = encrypt_blinded_payload(&key, &nonce, fee, expires_at_height, &plaintext)?;
        let encrypted_size = u32::try_from(ciphertext.len())
            .context("blinded transaction ciphertext is too large")?;
        let transaction = BlindedTransaction {
            commitment: String::new(),
            fee,
            encrypted_size,
            expires_at_height,
            nonce: hex_encode(nonce),
            ciphertext: hex_encode(&ciphertext),
            payload_hash,
        };
        let commitment = blinded_transaction_commitment(&transaction)?;
        let transaction = BlindedTransaction {
            commitment: commitment.clone(),
            ..transaction
        };
        self.validate_blinded_transaction(&transaction)?;
        Ok(BuiltBlindedTransaction {
            payload,
            transaction,
            reveal: BlindedReveal {
                commitment,
                key: hex_encode(key),
            },
        })
    }

    pub fn build_mine(&self, recipient: impl Into<String>) -> Result<Transaction> {
        let recipient = recipient.into();
        validate_address(&recipient, "mine recipient")?;
        let anchor = self.tip().hash.clone();
        let salt = 1;
        let difficulty_bits = self.current_mine_difficulty_bits();
        for nonce in 0..u64::MAX {
            let signature = mine_signature(&recipient, &anchor, salt, nonce, difficulty_bits);
            if !hash_meets_difficulty(&signature, difficulty_bits) {
                continue;
            }
            let transaction = Transaction::Mine {
                recipient: recipient.clone(),
                anchor: anchor.clone(),
                salt,
                nonce,
                difficulty_bits,
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

    pub fn search_mine(
        &self,
        recipient: impl Into<String>,
        salt: u64,
        start_nonce: u64,
        max_attempts: u64,
    ) -> Result<MineSearchOutcome> {
        let recipient = recipient.into();
        validate_address(&recipient, "mine recipient")?;
        let anchor = self.tip().hash.clone();
        let difficulty_bits = self.current_mine_difficulty_bits();
        let mut attempts = 0_u64;
        let mut nonce = start_nonce;
        while attempts < max_attempts {
            let signature = mine_signature(&recipient, &anchor, salt, nonce, difficulty_bits);
            attempts = attempts.saturating_add(1);
            let next_nonce = nonce.checked_add(1).unwrap_or(0);
            if hash_meets_difficulty(&signature, difficulty_bits) {
                let transaction = Transaction::Mine {
                    recipient: recipient.clone(),
                    anchor: anchor.clone(),
                    salt,
                    nonce,
                    difficulty_bits,
                    proof_header: None,
                    signature,
                };
                if !self.has_transaction(transaction.signature()) {
                    self.validate_new_transaction(&transaction)?;
                    return Ok(MineSearchOutcome {
                        transaction: Some(transaction),
                        next_nonce,
                        attempts,
                    });
                }
            }
            nonce = next_nonce;
        }
        Ok(MineSearchOutcome {
            transaction: None,
            next_nonce: nonce,
            attempts,
        })
    }

    pub fn stratum_mine_template(
        &self,
        recipient: impl Into<String>,
        anchor: impl AsRef<str>,
        salt: u64,
        difficulty_bits: u32,
    ) -> Result<StratumMineTemplate> {
        stratum_mine_template(recipient, anchor.as_ref(), salt, difficulty_bits)
    }

    pub fn build_stratum_mine(
        &self,
        template: StratumMineTemplate,
        share: StratumMineShare,
    ) -> Result<Transaction> {
        let nonce = pack_stratum_nonce(share.extranonce2, share.header_nonce);
        let header = stratum_mine_header_bytes(
            &template.recipient,
            &template.anchor,
            template.salt,
            nonce,
            template.difficulty_bits,
        )?;
        let transaction = Transaction::Mine {
            recipient: template.recipient,
            anchor: template.anchor,
            salt: template.salt,
            nonce,
            difficulty_bits: template.difficulty_bits,
            proof_header: Some(hex_encode(header)),
            signature: stratum_mine_signature(&header),
        };
        self.validate_new_transaction(&transaction)?;
        Ok(transaction)
    }

    pub fn submit_transaction(&mut self, transaction: Transaction) -> Result<bool> {
        Ok(self.submit_transaction_with_outcome(transaction)?.added())
    }

    pub fn submit_blinded_transaction(&mut self, transaction: BlindedTransaction) -> Result<bool> {
        if self.has_blinded_transaction(&transaction.commitment) {
            return Ok(false);
        }
        self.validate_blinded_transaction(&transaction)?;
        if self.pending_blinded.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("blinded mempool is full");
        }
        self.pending_blinded.push(transaction);
        Ok(true)
    }

    pub fn submit_blinded_reveal(&mut self, reveal: BlindedReveal) -> Result<bool> {
        if self.has_blinded_reveal(&reveal.commitment) {
            return Ok(false);
        }
        self.validate_blinded_reveal_terms(&reveal)?;
        if self.pending_reveals.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("blinded reveal pool is full");
        }
        self.pending_reveals.push(reveal);
        Ok(true)
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
        if transaction_inputs_spent_by(&transaction, &self.orphans) {
            return Ok(TransactionSubmitOutcome::ConflictsWithPending);
        }

        if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
            bail!("mempool is full");
        }

        let mut utxos = self.utxos_after_valid_pending()?;
        if transaction_has_missing_inputs(&transaction, &utxos) {
            if self.orphans.len() >= MAX_ORPHAN_TRANSACTIONS {
                bail!("orphan transaction pool is full");
            }
            self.orphans.push(transaction);
            return Ok(TransactionSubmitOutcome::Added);
        }
        apply_transaction(&transaction, &mut utxos)?;
        self.pending.push(transaction);
        self.promote_orphan_transactions()?;
        Ok(TransactionSubmitOutcome::Added)
    }

    pub fn mine_next_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<Block> {
        let prepared = self.prepare_next_block(wallet.address(), timestamp_ms)?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn mine_recovery_block(&self, wallet: &Wallet, timestamp_ms: u64) -> Result<Block> {
        let prepared = self.prepare_recovery_block(wallet.address(), timestamp_ms)?;
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        Ok(prepared.finish(wallet, vdf_output))
    }

    pub fn prepare_next_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.prepare_next_block_with_reveal_bundles(miner, timestamp_ms, Vec::new())
    }

    pub fn prepare_next_block_with_reveal_bundles(
        &self,
        miner: &str,
        timestamp_ms: u64,
        reveal_bundles: Vec<RevealBundle>,
    ) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        let Some((finalizer_rank, leader_ticket)) = self.finalizer_ticket_for_miner(height, miner)
        else {
            bail!("cannot mine block without a mature burn ticket");
        };
        if self.expected_leader_for_next_block().is_none() {
            bail!("no selected leader for block {height}");
        }

        let reveal_bundles = self.validate_next_block_reveal_bundles(reveal_bundles)?;
        let selection = self.select_block_transactions()?;
        ensure_block_has_burn(&selection.transactions)?;

        let tip = self.tip();
        let prev_hash = tip.hash.clone();
        let timestamp_ms = timestamp_ms.max(ticket_block_min_timestamp(tip, finalizer_rank)?);
        let bundle_hashes = reveal_bundle_hashes(&reveal_bundles);
        let vdf_seed = vdf_seed_for_child(&prev_hash, height, &bundle_hashes);
        Ok(PreparedBlock {
            height,
            prev_hash,
            timestamp_ms,
            miner: miner.to_string(),
            finalizer_mode: FinalizerMode::Ticket,
            reward: fee_reward(&selection.transactions)?,
            vdf_rounds: self.vdf_rounds_for_finalizer_rank(finalizer_rank)?,
            vdf_seed,
            finalizer_rank,
            leader_ticket: Some(leader_ticket),
            blinded_transactions: selection.blinded_transactions,
            reveal_bundles,
            transactions: selection.transactions,
        })
    }

    pub fn recovery_block_available_at(&self, timestamp_ms: u64) -> bool {
        timestamp_ms >= self.recovery_block_min_timestamp()
    }

    pub fn recovery_block_min_timestamp(&self) -> u64 {
        self.tip()
            .timestamp_ms
            .saturating_add(RECOVERY_BLOCK_DELAY_MS)
    }

    pub fn prepare_recovery_block(&self, miner: &str, timestamp_ms: u64) -> Result<PreparedBlock> {
        self.prepare_recovery_block_with_reveal_bundles(miner, timestamp_ms, Vec::new())
    }

    pub fn prepare_recovery_block_with_reveal_bundles(
        &self,
        miner: &str,
        timestamp_ms: u64,
        reveal_bundles: Vec<RevealBundle>,
    ) -> Result<PreparedBlock> {
        let height = self.tip().height + 1;
        let min_timestamp = self.recovery_block_min_timestamp();
        if timestamp_ms < min_timestamp {
            bail!("recovery block is not available before timestamp {min_timestamp}");
        }

        let reveal_bundles = self.validate_next_block_reveal_bundles(reveal_bundles)?;
        let selection = self.select_recovery_block_transactions(miner)?;
        ensure_block_has_burn(&selection.transactions)?;
        ensure_block_has_burn_from(&selection.transactions, miner)?;

        let tip = self.tip();
        let prev_hash = tip.hash.clone();
        let timestamp_ms = timestamp_ms.max(tip.timestamp_ms + 1);
        let bundle_hashes = reveal_bundle_hashes(&reveal_bundles);
        let vdf_seed =
            recovery_vdf_seed_for_child(&prev_hash, height, timestamp_ms, &bundle_hashes);
        Ok(PreparedBlock {
            height,
            prev_hash,
            timestamp_ms,
            miner: miner.to_string(),
            finalizer_mode: FinalizerMode::Recovery,
            finalizer_rank: 0,
            reward: fee_reward(&selection.transactions)?,
            vdf_rounds: self.recovery_vdf_rounds()?,
            vdf_seed,
            leader_ticket: None,
            blinded_transactions: selection.blinded_transactions,
            reveal_bundles,
            transactions: selection.transactions,
        })
    }

    pub fn apply_block(&mut self, block: Block) -> Result<()> {
        self.apply_block_at(block, unix_now_ms())
    }

    pub(crate) fn apply_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        self.apply_block_with_vdf_policy(block, true, now_ms)
    }

    pub(crate) fn block_requires_vdf_verification_at(
        &self,
        block: &Block,
        now_ms: u64,
    ) -> Result<bool> {
        self.precheck_block_without_vdf_at(block, now_ms)
    }

    pub fn apply_locally_mined_block(&mut self, block: Block) -> Result<()> {
        self.apply_preverified_block(block)
    }

    pub(crate) fn apply_preverified_block(&mut self, block: Block) -> Result<()> {
        self.apply_preverified_block_at(block, unix_now_ms())
    }

    pub(crate) fn apply_preverified_block_at(&mut self, block: Block, now_ms: u64) -> Result<()> {
        self.apply_block_with_vdf_policy(block, false, now_ms)
    }

    fn apply_block_with_vdf_policy(
        &mut self,
        block: Block,
        should_verify_vdf: bool,
        now_ms: u64,
    ) -> Result<()> {
        if !self.precheck_block_without_vdf_at(&block, now_ms)? {
            return Ok(());
        }

        if should_verify_vdf && !verify_vdf(&block.vdf_seed(), block.vdf_rounds, &block.vdf_output)
        {
            bail!("block VDF output is invalid");
        }

        let mut utxos = self.utxos.clone();
        let mut signatures = BTreeSet::new();
        let mut revealed_transactions = Vec::new();
        for tx in &block.transactions {
            if !signatures.insert(tx.signature()) {
                bail!("duplicate transaction in block");
            }
            self.validate_transaction_terms(tx)?;
            apply_transaction(tx, &mut utxos)?;
        }
        let mut revealed_commitments = BTreeSet::new();
        let included_reveal_bundle_count = block.included_reveal_bundle_count();
        for reveal in block.all_blinded_reveals() {
            if !revealed_commitments.insert(reveal.commitment.clone()) {
                bail!("duplicate blinded reveal in block");
            }
            let active = self
                .active_blinded
                .get(&reveal.commitment)
                .context("blinded reveal does not reference an active blinded transaction")?;
            let tx = self.decrypt_active_blinded(active, reveal)?;
            apply_transaction(&tx, &mut utxos)?;
            credit_blinded_fee_outputs(
                &mut utxos,
                active,
                &block.miner,
                &tx,
                included_reveal_bundle_count,
            )?;
            revealed_transactions.push(tx);
        }
        if block.reward != fee_reward(&block.transactions)? {
            bail!("block reward is invalid");
        }
        let mut tickets = self.tickets.clone();
        apply_finalizer_ticket_effects(&block, &mut tickets)?;
        credit_reward_output(&mut utxos, &block)?;
        tickets.extend(tickets_created_by_block(&block, &self.launch_profile)?);
        tickets.extend(tickets_created_by_transactions(
            block.height,
            &revealed_transactions,
            &self.launch_profile,
        )?);

        let mined_signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<BTreeSet<_>>();
        let included_blinded = block
            .blinded_transactions
            .iter()
            .map(|transaction| transaction.commitment.clone())
            .collect::<BTreeSet<_>>();
        let revealed_blinded = block
            .all_blinded_reveals()
            .into_iter()
            .map(|reveal| reveal.commitment.clone())
            .collect::<BTreeSet<_>>();
        self.utxos = utxos;
        self.tickets = tickets;
        self.chain.push(block);
        let new_height = self.height();
        let tip_miner = self.tip().miner.clone();
        let tip_blinded_transactions = self.tip().blinded_transactions.clone();
        self.active_blinded.retain(|commitment, active| {
            !revealed_blinded.contains(commitment)
                && new_height < active.transaction.expires_at_height
        });
        for transaction in tip_blinded_transactions {
            self.active_blinded.insert(
                transaction.commitment.clone(),
                ActiveBlindedTransaction {
                    transaction,
                    included_height: new_height,
                    included_by: tip_miner.clone(),
                },
            );
        }
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
        let orphans = std::mem::take(&mut self.orphans);
        self.orphans = orphans
            .into_iter()
            .filter(|tx| {
                !mined_signatures.contains(tx.signature())
                    && self.validate_transaction_terms(tx).is_ok()
            })
            .collect();
        let pending_blinded = std::mem::take(&mut self.pending_blinded);
        self.pending_blinded = pending_blinded
            .into_iter()
            .filter(|transaction| {
                !included_blinded.contains(&transaction.commitment)
                    && new_height < transaction.expires_at_height
                    && self.validate_blinded_transaction(transaction).is_ok()
            })
            .collect();
        let pending_reveals = std::mem::take(&mut self.pending_reveals);
        self.pending_reveals = pending_reveals
            .into_iter()
            .filter(|reveal| {
                !revealed_blinded.contains(&reveal.commitment)
                    && self.pending_reveal_transaction(reveal).is_ok()
            })
            .collect();
        self.promote_orphan_transactions()?;
        self.vdf_rounds = self.next_vdf_rounds_after_tip();
        Ok(())
    }

    fn precheck_block_without_vdf_at(&self, block: &Block, now_ms: u64) -> Result<bool> {
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
        let expected_vdf_rounds = self.expected_vdf_rounds_for_block(block)?;
        if block.vdf_rounds != expected_vdf_rounds {
            bail!("block VDF rounds are invalid");
        }
        if block.timestamp_ms <= self.tip().timestamp_ms {
            bail!("block timestamp must increase");
        }
        if block.finalizer_mode == FinalizerMode::Ticket {
            let min_timestamp = ticket_block_min_timestamp(self.tip(), block.finalizer_rank)?;
            if block.timestamp_ms < min_timestamp {
                bail!(
                    "block timestamp is before finalizer rank {} time slot {min_timestamp}",
                    block.finalizer_rank
                );
            }
        }
        let median_time_past = self.median_time_past();
        if block.timestamp_ms <= median_time_past {
            bail!("block timestamp must exceed median time past");
        }
        let max_future_timestamp = now_ms.saturating_add(MAX_BLOCK_TIMESTAMP_FUTURE_DRIFT_MS);
        if block.timestamp_ms > max_future_timestamp {
            bail!("block timestamp is too far in the future");
        }
        if block.transactions.len() > self.launch_profile.max_block_transactions {
            bail!("block has too many transactions");
        }
        let block_item_count = block.transactions.len()
            + block.blinded_transactions.len()
            + block.all_blinded_reveals().len();
        if block_item_count > self.launch_profile.max_block_transactions {
            bail!("block has too many transaction items");
        }
        if block.serialized_size_bytes()? > self.launch_profile.max_block_bytes {
            bail!("block exceeds max block size");
        }
        ensure_block_has_burn(&block.transactions)?;
        self.validate_reveal_bundles_for_block(
            block.height,
            &block.prev_hash,
            block.reveal_bundles.clone(),
        )?;
        validate_block_blinded_items(block, self)?;
        match block.finalizer_mode {
            FinalizerMode::Ticket => {
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
            }
            FinalizerMode::Recovery => {
                ensure_valid_recovery_block(block, self.tip())?;
            }
        }

        Ok(true)
    }

    fn median_time_past(&self) -> u64 {
        let mut timestamps = self
            .chain
            .iter()
            .rev()
            .take(BLOCK_MEDIAN_TIME_PAST_WINDOW)
            .map(|block| block.timestamp_ms)
            .collect::<Vec<_>>();
        timestamps.sort_unstable();
        timestamps[timestamps.len() / 2]
    }

    fn next_vdf_rounds_after_tip(&self) -> u64 {
        let Some(tip) = self.chain.last() else {
            return self.vdf_rounds;
        };
        if tip.height < 2 {
            return self.vdf_rounds;
        }

        let mut total_observed_ms = 0_u128;
        let mut observed_blocks = 0_u128;
        for pair in self
            .chain
            .windows(2)
            .rev()
            .filter(|pair| pair[0].height > 0)
            .take(VDF_RETARGET_WINDOW_BLOCKS)
        {
            let Some(observed_ms) = vdf_retarget_observed_block_ms(&pair[0], &pair[1]) else {
                continue;
            };
            total_observed_ms += u128::from(observed_ms);
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

    fn select_block_transactions(&self) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(None)
    }

    fn select_recovery_block_transactions(&self, miner: &str) -> Result<BlockSelection> {
        self.select_block_transactions_with_required_burn_owner(Some(miner))
    }

    fn select_block_transactions_with_required_burn_owner(
        &self,
        required_burn_owner: Option<&str>,
    ) -> Result<BlockSelection> {
        let mut utxos = self.utxos.clone();
        let mut remaining = self.valid_pending_transactions();
        let mut remaining_blinded = self.valid_pending_blinded_transactions();
        let mut selected = Vec::new();
        let mut selected_blinded = Vec::new();

        let needs_first_burn = !selected.iter().any(Transaction::is_burn);
        let needs_owner_burn = required_burn_owner.is_some_and(|owner| {
            !selected
                .iter()
                .any(|transaction| transaction.is_burn() && transaction.sender() == owner)
        });
        if needs_first_burn || needs_owner_burn {
            let first_burn_index = if let Some(owner) = required_burn_owner {
                best_selectable_burn_from_index(&remaining, &utxos, owner)
            } else {
                best_selectable_transaction_index(&remaining, &utxos, Some(TransactionKind::Burn))
            };
            if let Some(index) = first_burn_index {
                let tx = remaining.remove(index);
                let mut candidate = BlockSelection {
                    transactions: selected.clone(),
                    blinded_transactions: selected_blinded.clone(),
                };
                candidate.transactions.push(tx.clone());
                if estimated_block_selection_size_bytes(&candidate, required_burn_owner.is_some())?
                    <= self.launch_profile.max_block_bytes
                {
                    apply_transaction(&tx, &mut utxos)?;
                    selected.push(tx);
                }
            }
        }

        while selected.len() < self.launch_profile.max_block_transactions {
            let selected_count = selected.len() + selected_blinded.len();
            if selected_count >= self.launch_profile.max_block_transactions {
                break;
            }

            let best_plain = best_selectable_transaction_index(&remaining, &utxos, None)
                .map(|index| SelectableItem::Plain(index, fee_rate_key(&remaining[index])));
            let best_blinded = best_selectable_blinded_index(&remaining_blinded).map(|index| {
                SelectableItem::Blinded(index, blinded_fee_rate_key(&remaining_blinded[index]))
            });
            let Some(item) = best_selectable_item(best_plain, best_blinded) else {
                break;
            };

            match item {
                SelectableItem::Plain(index, _) => {
                    let tx = remaining.remove(index);
                    let mut candidate = BlockSelection {
                        transactions: selected.clone(),
                        blinded_transactions: selected_blinded.clone(),
                    };
                    candidate.transactions.push(tx.clone());
                    if estimated_block_selection_size_bytes(
                        &candidate,
                        required_burn_owner.is_some(),
                    )? <= self.launch_profile.max_block_bytes
                    {
                        apply_transaction(&tx, &mut utxos)?;
                        selected.push(tx);
                    }
                }
                SelectableItem::Blinded(index, _) => {
                    let transaction = remaining_blinded.remove(index);
                    let mut candidate = BlockSelection {
                        transactions: selected.clone(),
                        blinded_transactions: selected_blinded.clone(),
                    };
                    candidate.blinded_transactions.push(transaction.clone());
                    if estimated_block_selection_size_bytes(
                        &candidate,
                        required_burn_owner.is_some(),
                    )? <= self.launch_profile.max_block_bytes
                    {
                        selected_blinded.push(transaction);
                    }
                }
            }
        }
        Ok(BlockSelection {
            transactions: selected,
            blinded_transactions: selected_blinded,
        })
    }

    fn select_inputs(
        &self,
        address: &str,
        amount: Amount,
    ) -> Result<(Vec<UnsignedTxInput>, Amount)> {
        let utxos = self.utxos_after_spendable_pending()?;
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
        let utxos = self.utxos_after_spendable_pending()?;
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
        let mut utxos = self.utxos_after_spendable_pending()?;
        apply_transaction(transaction, &mut utxos)
    }

    fn promote_orphan_transactions(&mut self) -> Result<()> {
        loop {
            if self.pending.len() >= MAX_PENDING_TRANSACTIONS {
                return Ok(());
            }
            let mut promoted_index = None;
            let mut utxos = self.utxos_after_valid_pending()?;
            for (index, transaction) in self.orphans.iter().enumerate() {
                if transaction_inputs_spent_by(transaction, &self.pending) {
                    continue;
                }
                if transaction_has_missing_inputs(transaction, &utxos) {
                    continue;
                }
                if self.validate_transaction_terms(transaction).is_ok()
                    && apply_transaction(transaction, &mut utxos).is_ok()
                {
                    promoted_index = Some(index);
                    break;
                }
            }

            let Some(index) = promoted_index else {
                return Ok(());
            };
            self.pending.push(self.orphans.remove(index));
        }
    }

    fn validate_transaction_terms(&self, transaction: &Transaction) -> Result<()> {
        match transaction {
            Transaction::Transfer {
                inputs,
                outputs,
                signature,
                ..
            } => {
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(outputs)?;
                validate_signature(signature, "transaction signature")?;
            }
            Transaction::Burn {
                inputs,
                change,
                signature,
                ..
            } => {
                validate_transaction_inputs(inputs)?;
                validate_transaction_outputs(change)?;
                validate_signature(signature, "transaction signature")?;
            }
            Transaction::Mine {
                recipient,
                anchor,
                difficulty_bits,
                proof_header,
                signature,
                ..
            } => {
                validate_address(recipient, "mine recipient")?;
                validate_hash(anchor, "mine transaction anchor")?;
                validate_hash(signature, "mine transaction proof hash")?;
                if let Some(proof_header) = proof_header {
                    validate_stratum_header(proof_header)?;
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
        }
        Ok(())
    }

    fn validate_blinded_transaction(&self, transaction: &BlindedTransaction) -> Result<()> {
        validate_hash(&transaction.commitment, "blinded transaction commitment")?;
        validate_hash(
            &transaction.payload_hash,
            "blinded transaction payload hash",
        )?;
        decode_hex_array::<BLINDED_NONCE_BYTES>(&transaction.nonce)
            .context("invalid blinded transaction nonce")?;
        let ciphertext = decode_hex(&transaction.ciphertext)
            .context("invalid blinded transaction ciphertext")?;
        if ciphertext.is_empty() {
            bail!("blinded transaction ciphertext is empty");
        }
        if ciphertext.len() != transaction.encrypted_size as usize {
            bail!("blinded transaction encrypted size is invalid");
        }
        if transaction.expires_at_height <= self.height() {
            bail!("blinded transaction is expired");
        }
        if transaction.expires_at_height
            > self
                .height()
                .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS)
        {
            bail!("blinded transaction expiry is too far in the future");
        }
        let expected = blinded_transaction_commitment(transaction)?;
        if transaction.commitment != expected {
            bail!("blinded transaction commitment is invalid");
        }
        Ok(())
    }

    fn validate_blinded_reveal_terms(&self, reveal: &BlindedReveal) -> Result<()> {
        validate_hash(&reveal.commitment, "blinded reveal commitment")?;
        decode_hex_array::<BLINDED_KEY_BYTES>(&reveal.key).context("invalid blinded reveal key")?;
        Ok(())
    }

    fn valid_pending_blinded_transactions(&self) -> Vec<BlindedTransaction> {
        let next_height = self.height().saturating_add(1);
        self.pending_blinded
            .iter()
            .filter(|transaction| {
                transaction.expires_at_height > next_height
                    && self.validate_blinded_transaction(transaction).is_ok()
            })
            .cloned()
            .collect()
    }

    fn valid_pending_blinded_reveals(&self) -> Vec<BlindedReveal> {
        self.pending_reveals
            .iter()
            .filter(|reveal| self.pending_reveal_transaction(reveal).is_ok())
            .cloned()
            .collect()
    }

    fn reveal_fee_order_key(&self, reveal: &BlindedReveal) -> (u128, Amount) {
        let Some(active) = self.active_blinded.get(&reveal.commitment) else {
            return (0, 0);
        };
        let size = active.transaction.fee_rate_size_bytes();
        let rate = if size == 0 {
            0
        } else {
            u128::from(active.transaction.fee) * 1_000_000 / size as u128
        };
        (rate, active.transaction.fee)
    }

    fn pending_reveal_transaction(&self, reveal: &BlindedReveal) -> Result<Transaction> {
        self.validate_blinded_reveal_terms(reveal)?;
        let active = self
            .active_blinded
            .get(&reveal.commitment)
            .context("blinded reveal does not reference an active blinded transaction")?;
        self.decrypt_active_blinded(active, reveal)
    }

    fn decrypt_active_blinded(
        &self,
        active: &ActiveBlindedTransaction,
        reveal: &BlindedReveal,
    ) -> Result<Transaction> {
        if self.height() >= active.transaction.expires_at_height {
            bail!("blinded transaction reveal is expired");
        }
        let transaction = decrypt_blinded_transaction(&active.transaction, reveal)?;
        if transaction.fee() != active.transaction.fee {
            bail!("blinded transaction reveal fee does not match envelope");
        }
        self.validate_transaction_terms(&transaction)?;
        Ok(transaction)
    }

    fn utxos_after_valid_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            apply_transaction(&pending, &mut utxos)?;
        }
        Ok(utxos)
    }

    fn utxos_after_spendable_pending(&self) -> Result<BTreeMap<OutPoint, TxOutput>> {
        let mut utxos = self.utxos.clone();
        for pending in self.valid_pending_transactions() {
            if matches!(pending, Transaction::Mine { .. }) {
                continue;
            }
            let mut candidate = utxos.clone();
            if apply_transaction(&pending, &mut candidate).is_ok() {
                utxos = candidate;
            }
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

    fn vdf_rounds_for_finalizer_rank(&self, rank: u32) -> Result<u64> {
        vdf_rounds_for_finalizer_rank(self.vdf_rounds, rank)
    }

    fn recovery_vdf_rounds(&self) -> Result<u64> {
        vdf_rounds_for_finalizer_rank(self.vdf_rounds, 0)
    }

    fn expected_vdf_rounds_for_block(&self, block: &Block) -> Result<u64> {
        match block.finalizer_mode {
            FinalizerMode::Ticket => self.vdf_rounds_for_finalizer_rank(block.finalizer_rank),
            FinalizerMode::Recovery => self.recovery_vdf_rounds(),
        }
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

fn vdf_rounds_for_finalizer_rank(base_rounds: u64, rank: u32) -> Result<u64> {
    let rounds = base_rounds
        .checked_mul(u64::from(
            rank.checked_add(1).context("finalizer rank overflows")?,
        ))
        .context("finalizer rank VDF rounds overflow")?;
    if rounds > MAX_VDF_ROUNDS {
        bail!("finalizer rank VDF rounds exceed maximum");
    }
    Ok(rounds)
}

fn finalizer_rank_slot_delay_ms(rank: u32) -> Result<u64> {
    VDF_TARGET_BLOCK_MS
        .checked_mul(u64::from(rank))
        .context("finalizer rank time slot overflow")
}

fn ticket_block_min_timestamp(parent: &Block, rank: u32) -> Result<u64> {
    if rank == 0 {
        return parent
            .timestamp_ms
            .checked_add(1)
            .context("finalizer rank minimum timestamp overflow");
    }

    parent
        .timestamp_ms
        .checked_add(finalizer_rank_slot_delay_ms(rank)?)
        .context("finalizer rank minimum timestamp overflow")
}

fn base_vdf_rounds_for_finalizer_rank(vdf_rounds: u64, rank: u32) -> u64 {
    vdf_rounds / u64::from(rank.saturating_add(1).max(1))
}

fn tickets_created_by_block(block: &Block, profile: &LaunchProfile) -> Result<Vec<BurnTicket>> {
    tickets_created_by_transactions(block.height, &block.transactions, profile)
}

fn tickets_created_by_transactions(
    block_height: u64,
    transactions: &[Transaction],
    profile: &LaunchProfile,
) -> Result<Vec<BurnTicket>> {
    if profile.ticket_expiry_window_heights == 0 {
        bail!("ticket expiry window must be at least one height");
    }
    let mut tickets = Vec::new();
    for tx in transactions {
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
        let target_height = block_height
            .checked_add(profile.ticket_maturity_delay_heights)
            .with_context(|| format!("ticket target height overflow at block {block_height}"))?;
        let eligible_until_height = target_height
            .checked_add(profile.ticket_expiry_window_heights - 1)
            .with_context(|| format!("ticket expiry height overflow at block {block_height}"))?;
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

fn apply_finalizer_ticket_effects(block: &Block, tickets: &mut Vec<BurnTicket>) -> Result<()> {
    match block.finalizer_mode {
        FinalizerMode::Ticket => consume_leader_ticket(block, tickets),
        FinalizerMode::Recovery => {
            tickets.retain(|ticket| {
                !ticket_is_eligible_for_height(ticket, block.height)
                    && ticket.eligible_until_height > block.height
            });
            Ok(())
        }
    }
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

fn ensure_block_has_burn_from(transactions: &[Transaction], miner: &str) -> Result<()> {
    if !transactions
        .iter()
        .any(|transaction| transaction.is_burn() && transaction.sender() == miner)
    {
        bail!("recovery block must include a burn from the finalizer");
    }
    Ok(())
}

fn ensure_valid_recovery_block(block: &Block, parent: &Block) -> Result<()> {
    if block.finalizer_rank != 0 {
        bail!("recovery block finalizer rank must be 0");
    }
    if block.leader_proof.is_some() {
        bail!("recovery block must not carry a leader proof");
    }
    let min_timestamp = parent.timestamp_ms.saturating_add(RECOVERY_BLOCK_DELAY_MS);
    if block.timestamp_ms < min_timestamp {
        bail!("recovery block is not available before timestamp {min_timestamp}");
    }
    ensure_block_has_burn_from(&block.transactions, &block.miner)
}

fn fee_rate_key(transaction: &Transaction) -> u128 {
    let size = transaction.economic_size_bytes();
    if size == 0 {
        return 0;
    }
    u128::from(transaction.fee()) * 1_000_000 / size as u128
}

fn blinded_fee_rate_key(transaction: &BlindedTransaction) -> u128 {
    let size = transaction.fee_rate_size_bytes();
    if size == 0 {
        return 0;
    }
    u128::from(transaction.fee) * 1_000_000 / size as u128
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectableItem {
    Plain(usize, u128),
    Blinded(usize, u128),
}

fn best_selectable_item(
    plain: Option<SelectableItem>,
    blinded: Option<SelectableItem>,
) -> Option<SelectableItem> {
    match (plain, blinded) {
        (
            Some(SelectableItem::Plain(_, plain_rate)),
            Some(SelectableItem::Blinded(_, blind_rate)),
        ) => {
            if blind_rate > plain_rate {
                blinded
            } else {
                plain
            }
        }
        (Some(item), None) | (None, Some(item)) => Some(item),
        (None, None) => None,
        _ => None,
    }
}

fn best_selectable_blinded_index(transactions: &[BlindedTransaction]) -> Option<usize> {
    transactions
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            blinded_fee_rate_key(left)
                .cmp(&blinded_fee_rate_key(right))
                .then_with(|| left.fee.cmp(&right.fee))
                .then_with(|| right.commitment.cmp(&left.commitment))
        })
        .map(|(index, _)| index)
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

fn best_selectable_burn_from_index(
    transactions: &[Transaction],
    utxos: &BTreeMap<OutPoint, TxOutput>,
    owner: &str,
) -> Option<usize> {
    transactions
        .iter()
        .enumerate()
        .filter(|(_, tx)| tx.is_burn() && tx.sender() == owner)
        .filter(|(_, tx)| {
            let mut utxos = utxos.clone();
            apply_transaction(tx, &mut utxos).is_ok()
        })
        .max_by(|(_, left), (_, right)| {
            fee_rate_key(left)
                .cmp(&fee_rate_key(right))
                .then_with(|| left.fee().cmp(&right.fee()))
                .then_with(|| right.signature().cmp(left.signature()))
        })
        .map(|(index, _)| index)
}

fn validate_genesis_allocations(genesis_allocations: &BTreeMap<String, Amount>) -> Result<()> {
    for address in genesis_allocations.keys() {
        validate_address(address, "genesis allocation")?;
    }
    Ok(())
}

fn validate_transaction_inputs(inputs: &[TxInput]) -> Result<()> {
    for input in inputs {
        validate_protocol_id(&input.outpoint.txid, "input outpoint txid")?;
        validate_address(&input.owner, "input owner")?;
        validate_signature(&input.signature, "input signature")?;
    }
    Ok(())
}

fn validate_transaction_outputs(outputs: &[TxOutput]) -> Result<()> {
    for output in outputs {
        validate_address(&output.address, "output recipient")?;
    }
    Ok(())
}

fn validate_genesis_burn_transaction(transaction: &Transaction) -> Result<()> {
    let Transaction::Burn {
        inputs,
        change,
        fee,
        signature,
        ..
    } = transaction
    else {
        bail!("genesis only supports burn transactions");
    };
    if *fee != 0 {
        bail!("genesis burn fee must be zero");
    }
    validate_hash(signature, "genesis burn signature")?;
    validate_transaction_outputs(change)?;
    for input in inputs {
        validate_hash(&input.outpoint.txid, "genesis burn input outpoint txid")?;
        validate_address(&input.owner, "genesis burn input owner")?;
        if input.signature != "genesis" {
            bail!("genesis burn input signature is invalid");
        }
    }
    Ok(())
}

fn validate_address(address: &str, label: &str) -> Result<()> {
    decode_hex_array::<PUBLIC_KEY_BYTES>(address)
        .with_context(|| format!("invalid {label} address"))?;
    Ok(())
}

fn validate_hash(hash: &str, label: &str) -> Result<()> {
    decode_hex_array::<HASH_BYTES>(hash).with_context(|| format!("invalid {label}"))?;
    Ok(())
}

fn validate_signature(signature: &str, label: &str) -> Result<()> {
    decode_hex_array::<SIGNATURE_BYTES>(signature).with_context(|| format!("invalid {label}"))?;
    Ok(())
}

fn validate_stratum_header(header: &str) -> Result<()> {
    decode_hex_array::<STRATUM_MINE_HEADER_BYTES>(header)
        .context("invalid mine transaction proof header")?;
    Ok(())
}

fn validate_protocol_id(value: &str, label: &str) -> Result<()> {
    let bytes = decode_hex(value).with_context(|| format!("invalid {label}"))?;
    match bytes.len() {
        HASH_BYTES | SIGNATURE_BYTES => Ok(()),
        length => bail!("invalid {label}: expected 32 or 64 bytes, got {length}"),
    }
}

fn canonical_transaction_size_bytes(transaction: &Transaction) -> usize {
    match transaction {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } => {
            1 + compact_len(inputs.len() as u128)
                + compact_inputs_size_bytes(inputs)
                + compact_len(outputs.len() as u128)
                + compact_outputs_size_bytes(outputs)
                + compact_len(u128::from(*fee))
                + signature_size_bytes(signature)
        }
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } => {
            1 + compact_len(inputs.len() as u128)
                + compact_inputs_size_bytes(inputs)
                + compact_len(change.len() as u128)
                + compact_outputs_size_bytes(change)
                + compact_len(u128::from(*amount))
                + compact_len(u128::from(*fee))
                + signature_size_bytes(signature)
        }
        Transaction::Mine {
            recipient,
            anchor,
            salt,
            nonce,
            difficulty_bits,
            proof_header,
            signature,
        } => {
            1 + address_size_bytes(recipient)
                + hash_size_bytes(anchor)
                + compact_len(u128::from(*salt))
                + compact_len(u128::from(*nonce))
                + compact_len(u128::from(*difficulty_bits))
                + 1
                + proof_header
                    .as_ref()
                    .map(|header| stratum_header_size_bytes(header))
                    .unwrap_or(0)
                + hash_size_bytes(signature)
        }
    }
}

fn compact_inputs_size_bytes(inputs: &[TxInput]) -> usize {
    inputs
        .iter()
        .map(|input| {
            protocol_id_size_bytes(&input.outpoint.txid)
                + compact_len(u128::from(input.outpoint.index))
                + address_size_bytes(&input.owner)
        })
        .sum()
}

fn compact_outputs_size_bytes(outputs: &[TxOutput]) -> usize {
    outputs.iter().map(compact_output_size_bytes).sum()
}

fn compact_output_size_bytes(output: &TxOutput) -> usize {
    address_size_bytes(&output.address) + compact_len(u128::from(output.amount))
}

fn address_size_bytes(address: &str) -> usize {
    debug_assert!(validate_address(address, "debug address").is_ok());
    PUBLIC_KEY_BYTES
}

fn hash_size_bytes(hash: &str) -> usize {
    debug_assert!(validate_hash(hash, "debug hash").is_ok());
    HASH_BYTES
}

fn signature_size_bytes(signature: &str) -> usize {
    debug_assert!(validate_signature(signature, "debug signature").is_ok());
    SIGNATURE_BYTES
}

fn stratum_header_size_bytes(header: &str) -> usize {
    debug_assert!(validate_stratum_header(header).is_ok());
    STRATUM_MINE_HEADER_BYTES
}

fn protocol_id_size_bytes(value: &str) -> usize {
    match decode_hex(value).map(|bytes| bytes.len()) {
        Ok(HASH_BYTES) => HASH_BYTES,
        Ok(SIGNATURE_BYTES) => SIGNATURE_BYTES,
        _ => SIGNATURE_BYTES,
    }
}

fn compact_len(mut value: u128) -> usize {
    let mut bytes = 1;
    while value >= 0x80 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

fn estimated_block_selection_size_bytes(
    selection: &BlockSelection,
    recovery: bool,
) -> Result<usize> {
    let block = Block {
        height: u64::MAX,
        prev_hash: "f".repeat(64),
        timestamp_ms: u64::MAX,
        miner: "f".repeat(64),
        finalizer_mode: if recovery {
            FinalizerMode::Recovery
        } else {
            FinalizerMode::Ticket
        },
        finalizer_rank: 0,
        reward: u64::MAX,
        vdf_rounds: u64::MAX,
        vdf_output: "f".repeat(64),
        leader_proof: (!recovery).then(|| LeaderProof {
            ticket_id: "f".repeat(64),
            public_key: "f".repeat(64),
            signature: "f".repeat(128),
        }),
        blinded_transactions: selection.blinded_transactions.clone(),
        reveal_bundles: Vec::new(),
        transactions: selection.transactions.clone(),
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
    verify_address_signature(
        &proof.public_key,
        &payload.canonical(),
        &proof.signature,
        "leader",
    )
}

fn verify_address_signature(
    address: &str,
    payload: &str,
    signature: &str,
    label: &str,
) -> Result<()> {
    let public_key = decode_hex_array::<PUBLIC_KEY_BYTES>(address)
        .with_context(|| format!("invalid {label} public key {address}"))?;
    let signature = decode_hex_array::<SIGNATURE_BYTES>(signature)
        .with_context(|| format!("invalid {label} signature hex"))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .with_context(|| format!("invalid {label} public key"))?;
    let signature = Signature::from_bytes(&signature);
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .with_context(|| format!("{label} signature is invalid"))
}

pub fn default_reveal_bundle_hash(slot: usize) -> String {
    hex_hash(format!("iuna-default-reveal-bundle-v1:{slot}"))
}

fn reveal_bundle_hashes(bundles: &[RevealBundle]) -> [String; REVEAL_COMMITTEE_SIZE] {
    std::array::from_fn(|slot| {
        bundles
            .iter()
            .find(|bundle| usize::from(bundle.slot) == slot)
            .map(RevealBundle::bundle_hash)
            .unwrap_or_else(|| default_reveal_bundle_hash(slot))
    })
}

fn canonical_reveal_bundle_hashes(bundle_hashes: &[String; REVEAL_COMMITTEE_SIZE]) -> String {
    bundle_hashes.join("|")
}

fn vdf_seed_for_child(
    prev_hash: &str,
    height: u64,
    bundle_hashes: &[String; REVEAL_COMMITTEE_SIZE],
) -> String {
    hex_hash(format!(
        "iuna-vdf-child:{prev_hash}:{height}:{}",
        canonical_reveal_bundle_hashes(bundle_hashes)
    ))
}

fn recovery_vdf_seed_for_child(
    prev_hash: &str,
    height: u64,
    timestamp_ms: u64,
    bundle_hashes: &[String; REVEAL_COMMITTEE_SIZE],
) -> String {
    hex_hash(format!(
        "iuna-recovery-vdf-child:{prev_hash}:{height}:{timestamp_ms}:{}",
        canonical_reveal_bundle_hashes(bundle_hashes)
    ))
}

fn apply_transaction(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
) -> Result<()> {
    transaction.verify_signature()?;
    match transaction {
        Transaction::Mine { recipient, .. } => {
            let output = TxOutput {
                address: recipient.clone(),
                amount: MINE_REWARD,
            };
            ensure_outputs_do_not_overflow(utxos, std::slice::from_ref(&output))?;
            utxos.insert(
                OutPoint {
                    txid: transaction.signature().to_string(),
                    index: 0,
                },
                output,
            );
            return Ok(());
        }
        Transaction::Transfer { .. } | Transaction::Burn { .. } => {}
    }
    ensure_single_input_owner(transaction)?;
    let input_total = spend_inputs(transaction, utxos)?;
    let outputs = transaction.outputs();
    let output_total = outputs.iter().try_fold(0_u64, |total, output| {
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
    ensure_outputs_do_not_overflow(utxos, &outputs)?;
    for (index, output) in outputs.iter().enumerate() {
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

fn validate_block_blinded_items(block: &Block, ledger: &Ledger) -> Result<()> {
    let mut commitments = BTreeSet::new();
    for transaction in &block.blinded_transactions {
        if !commitments.insert(transaction.commitment.clone()) {
            bail!("duplicate blinded transaction in block");
        }
        ledger.validate_blinded_transaction(transaction)?;
        if transaction.expires_at_height <= block.height {
            bail!("blinded transaction is expired for block height");
        }
        if ledger.active_blinded.contains_key(&transaction.commitment) {
            bail!("blinded transaction is already active");
        }
        if ledger.chain.iter().any(|block| {
            block
                .blinded_transactions
                .iter()
                .any(|existing| existing.commitment == transaction.commitment)
        }) {
            bail!("blinded transaction is already on chain");
        }
    }

    let mut reveals = BTreeSet::new();
    for reveal in block.all_blinded_reveals() {
        if !reveals.insert(reveal.commitment.clone()) {
            bail!("duplicate blinded reveal in block");
        }
        if ledger.chain.iter().any(|block| {
            block
                .all_blinded_reveals()
                .iter()
                .any(|existing| existing.commitment == reveal.commitment)
        }) {
            bail!("blinded reveal is already on chain");
        }
        ledger.pending_reveal_transaction(reveal)?;
    }
    Ok(())
}

fn decrypt_blinded_transaction(
    transaction: &BlindedTransaction,
    reveal: &BlindedReveal,
) -> Result<Transaction> {
    if reveal.commitment != transaction.commitment {
        bail!("blinded reveal commitment does not match transaction");
    }
    let key =
        decode_hex_array::<BLINDED_KEY_BYTES>(&reveal.key).context("invalid blinded reveal key")?;
    let nonce = decode_hex_array::<BLINDED_NONCE_BYTES>(&transaction.nonce)
        .context("invalid blinded transaction nonce")?;
    let ciphertext =
        decode_hex(&transaction.ciphertext).context("invalid blinded transaction ciphertext")?;
    let plaintext = decrypt_blinded_payload(
        &key,
        &nonce,
        transaction.fee,
        transaction.expires_at_height,
        &ciphertext,
    )?;
    if hex_hash(&plaintext) != transaction.payload_hash {
        bail!("blinded transaction payload hash is invalid");
    }
    serde_json::from_slice(&plaintext).context("failed to decode blinded transaction payload")
}

fn encrypt_blinded_payload(
    key: &[u8; BLINDED_KEY_BYTES],
    nonce: &[u8; BLINDED_NONCE_BYTES],
    fee: Amount,
    expires_at_height: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad: blinded_payload_aad(fee, expires_at_height).as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to encrypt blinded transaction payload"))
}

fn decrypt_blinded_payload(
    key: &[u8; BLINDED_KEY_BYTES],
    nonce: &[u8; BLINDED_NONCE_BYTES],
    fee: Amount,
    expires_at_height: u64,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            chacha20poly1305::aead::Payload {
                msg: ciphertext,
                aad: blinded_payload_aad(fee, expires_at_height).as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to decrypt blinded transaction payload"))
}

fn blinded_payload_aad(fee: Amount, expires_at_height: u64) -> String {
    format!("iuna-blinded-payload-v1:{fee}:{expires_at_height}")
}

fn blinded_transaction_commitment(transaction: &BlindedTransaction) -> Result<String> {
    let mut without_commitment = transaction.clone();
    without_commitment.commitment.clear();
    Ok(hex_hash(without_commitment.canonical()))
}

fn credit_blinded_fee_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    active: &ActiveBlindedTransaction,
    reveal_executor: &str,
    transaction: &Transaction,
    included_reveal_bundle_count: usize,
) -> Result<()> {
    let fee = transaction.fee();
    if fee == 0 {
        return Ok(());
    }
    let committer_fee = fee / 2;
    let executor_full_fee = fee - committer_fee;
    let executor_fee = executor_full_fee.saturating_mul(included_reveal_bundle_count as u64)
        / REVEAL_COMMITTEE_SIZE as u64;
    let mut outputs = Vec::new();
    if committer_fee > 0 {
        outputs.push((
            blinded_committer_fee_outpoint(&active.transaction.commitment),
            TxOutput {
                address: active.included_by.clone(),
                amount: committer_fee,
            },
        ));
    }
    if executor_fee > 0 {
        outputs.push((
            blinded_executor_fee_outpoint(&active.transaction.commitment),
            TxOutput {
                address: reveal_executor.to_string(),
                amount: executor_fee,
            },
        ));
    }
    let tx_outputs = outputs
        .iter()
        .map(|(_, output)| output.clone())
        .collect::<Vec<_>>();
    ensure_outputs_do_not_overflow(utxos, &tx_outputs)?;
    for (outpoint, output) in outputs {
        utxos.insert(outpoint, output);
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
        finalizer_mode: FinalizerMode::Ticket,
        finalizer_rank: 0,
        reward,
        vdf_rounds: 0,
        vdf_output,
        leader_proof: None,
        blinded_transactions: Vec::new(),
        reveal_bundles: Vec::new(),
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
            Transaction::Burn { .. } => {
                validate_genesis_burn_transaction(transaction)?;
                apply_transaction(transaction, &mut utxos)?;
            }
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

fn blinded_committer_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

fn blinded_executor_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 2,
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
    if !block.blinded_transactions.is_empty() || !block.reveal_bundles.is_empty() {
        bail!("genesis block must not carry blinded transactions");
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

pub fn run_vdf(seed: &str, rounds: u64) -> String {
    let x = vdf_seed_element(seed);
    let mut y = x;
    for _ in 0..rounds {
        y = mul_mod(y, y);
    }

    let challenge = vdf_challenge_prime(seed, rounds, y);
    let proof = vdf_proof(x, rounds, challenge);
    encode_vdf_solution(y, proof)
}

pub fn verify_vdf(seed: &str, rounds: u64, solution: &str) -> bool {
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

pub fn revealed_blinded_transactions(
    snapshot: &ChainSnapshot,
) -> Result<Vec<RevealedBlindedTransaction>> {
    let mut active = BTreeMap::<String, ActiveBlindedTransaction>::new();
    let mut revealed = Vec::new();
    for block in &snapshot.blocks {
        for reveal in block.all_blinded_reveals() {
            let active_transaction = active.get(&reveal.commitment).with_context(|| {
                format!(
                    "block {} reveals unknown blinded transaction {}",
                    block.height, reveal.commitment
                )
            })?;
            let transaction = decrypt_blinded_transaction(&active_transaction.transaction, reveal)?;
            if transaction.fee() != active_transaction.transaction.fee {
                bail!(
                    "block {} blinded reveal fee does not match envelope",
                    block.height
                );
            }
            revealed.push(RevealedBlindedTransaction {
                height: block.height,
                commitment: reveal.commitment.clone(),
                included_by: active_transaction.included_by.clone(),
                transaction,
            });
            active.remove(&reveal.commitment);
        }
        active.retain(|_, active_transaction| {
            block.height < active_transaction.transaction.expires_at_height
        });
        for transaction in &block.blinded_transactions {
            active.insert(
                transaction.commitment.clone(),
                ActiveBlindedTransaction {
                    transaction: transaction.clone(),
                    included_height: block.height,
                    included_by: block.miner.clone(),
                },
            );
        }
    }
    Ok(revealed)
}

fn vdf_seed_element(seed: &str) -> u128 {
    let digest = Sha256::digest(format!("iuna-vdf-seed:{seed}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    2 + (u128::from_be_bytes(bytes) % (VDF_MODULUS - 3))
}

fn vdf_challenge_prime(seed: &str, rounds: u64, output: u128) -> u64 {
    let digest = Sha256::digest(format!("iuna-vdf-challenge:{seed}:{rounds}:{output:x}"));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let candidate = VDF_CHALLENGE_MIN + (u64::from_be_bytes(bytes) % VDF_CHALLENGE_MIN);
    next_odd_prime(candidate | 1)
}

fn vdf_proof(x: u128, rounds: u64, challenge: u64) -> u128 {
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

fn pow_mod_small(base: u64, exponent: u64, modulus: u64) -> u64 {
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

fn retarget_vdf_rounds(current_rounds: u64, observed_block_ms: u64) -> u64 {
    let current = u128::from(current_rounds);
    let observed = u128::from(observed_block_ms.max(1));
    let target = u128::from(VDF_TARGET_BLOCK_MS);
    let deadband = target * VDF_RETARGET_DEADBAND_PERCENT / 100;
    if observed >= target.saturating_sub(deadband) && observed <= target.saturating_add(deadband) {
        return current_rounds;
    }

    let raw_adjusted = current * target / observed;
    let max_step = (current * MAX_VDF_RETARGET_STEP_PERCENT / 100).max(1);
    let min_next = current
        .saturating_sub(max_step)
        .max(u128::from(MIN_VDF_ROUNDS));
    let max_next = current
        .saturating_add(max_step)
        .min(u128::from(MAX_VDF_ROUNDS));
    raw_adjusted.clamp(min_next, max_next) as u64
}

fn clamped_vdf_retarget_observed_block_ms(observed_block_ms: u64) -> u64 {
    observed_block_ms.clamp(
        MIN_VDF_RETARGET_OBSERVED_BLOCK_MS,
        MAX_VDF_RETARGET_OBSERVED_BLOCK_MS,
    )
}

fn vdf_retarget_observed_block_ms(parent: &Block, child: &Block) -> Option<u64> {
    if child.finalizer_mode != FinalizerMode::Ticket || child.finalizer_rank != 0 {
        return None;
    }

    Some(clamped_vdf_retarget_observed_block_ms(
        child.timestamp_ms - parent.timestamp_ms,
    ))
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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

    #[test]
    fn target_block_time_is_five_minutes() {
        assert_eq!(VDF_TARGET_BLOCK_MS, 5 * 60 * 1_000);
    }

    fn test_utxo_outpoint(index: usize) -> OutPoint {
        OutPoint {
            txid: format!("{index:064x}"),
            index: 0,
        }
    }

    fn named_test_outpoint(name: &str) -> OutPoint {
        OutPoint {
            txid: hex_hash(format!("test-utxo:{name}")),
            index: 0,
        }
    }

    fn ledger_with_wallet_utxos(wallet: &Wallet, amounts: &[Amount]) -> Ledger {
        let mut ledger = Ledger::new(BTreeMap::new(), 1);
        ledger.utxos = amounts
            .iter()
            .enumerate()
            .map(|(index, amount)| {
                (
                    test_utxo_outpoint(index),
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

    fn apply_preverified_burn_block_at(
        ledger: &mut Ledger,
        wallet: &Wallet,
        timestamp_ms: u64,
    ) -> Block {
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let work = ledger
            .prepare_next_block(wallet.address(), timestamp_ms)
            .unwrap();
        let block = work.finish(wallet, "preverified-vdf".to_string());
        ledger
            .apply_preverified_block_at(block.clone(), u64::MAX)
            .unwrap();
        block
    }

    fn vdf_retarget_sample_block(
        timestamp_ms: u64,
        finalizer_mode: FinalizerMode,
        finalizer_rank: u32,
    ) -> Block {
        Block {
            height: 1,
            prev_hash: String::new(),
            timestamp_ms,
            miner: String::new(),
            finalizer_mode,
            finalizer_rank,
            reward: 0,
            vdf_rounds: 1,
            vdf_output: String::new(),
            leader_proof: None,
            blinded_transactions: Vec::new(),
            reveal_bundles: Vec::new(),
            transactions: Vec::new(),
            hash: String::new(),
        }
    }

    const TEST_BURN_AMOUNT: Amount = MICRO_IUNA / 10;

    fn unsigned_mine(ledger: &Ledger, recipient: &str) -> Transaction {
        let anchor = ledger.tip().hash.clone();
        let difficulty_bits = ledger.current_mine_difficulty_bits();
        for nonce in 0..u64::MAX {
            let salt = 1;
            let signature = mine_signature(recipient, &anchor, salt, nonce, difficulty_bits);
            if hash_meets_difficulty(&signature, difficulty_bits) {
                return Transaction::Mine {
                    recipient: recipient.to_string(),
                    anchor,
                    salt,
                    nonce,
                    difficulty_bits,
                    proof_header: None,
                    signature,
                };
            }
        }
        panic!("expected to find mine proof");
    }

    fn wallet_for_address<'a>(wallets: &'a [Wallet], address: &str) -> &'a Wallet {
        wallets
            .iter()
            .find(|wallet| wallet.address() == address)
            .unwrap_or_else(|| panic!("missing wallet for address {address}"))
    }

    fn ledger_with_finalizers(
        finalizers: &[Wallet],
        extra_allocations: &[(&Wallet, Amount)],
    ) -> Ledger {
        let mut allocations = BTreeMap::new();
        for wallet in finalizers {
            allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        }
        for (wallet, amount) in extra_allocations {
            allocations.insert(wallet.address().to_string(), *amount);
        }
        Ledger::new_with_genesis_burns(
            allocations,
            finalizers
                .iter()
                .map(|wallet| GenesisBurn::new(wallet.address(), MICRO_IUNA))
                .collect(),
            1,
        )
        .unwrap()
    }

    fn mine_preverified_as_next_leader(
        ledger: &mut Ledger,
        wallets: &[Wallet],
        timestamp_ms: u64,
    ) -> Block {
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(wallets, &leader);
        let prepared = ledger
            .prepare_next_block(wallet.address(), timestamp_ms)
            .unwrap();
        let block = prepared.finish(wallet, "preverified-vdf".to_string());
        ledger
            .apply_preverified_block_at(block.clone(), u64::MAX)
            .unwrap();
        block
    }

    fn mine_preverified_as_next_leader_with_reveal_bundles(
        ledger: &mut Ledger,
        wallets: &[Wallet],
        timestamp_ms: u64,
    ) -> Block {
        let bundles = ledger
            .reveal_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                let wallet = wallet_for_address(wallets, &member.owner);
                ledger.build_reveal_bundle(wallet).unwrap()
            })
            .collect::<Vec<_>>();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(wallets, &leader);
        let prepared = ledger
            .prepare_next_block_with_reveal_bundles(wallet.address(), timestamp_ms, bundles)
            .unwrap();
        let block = prepared.finish(wallet, "preverified-vdf".to_string());
        ledger
            .apply_preverified_block_at(block.clone(), u64::MAX)
            .unwrap();
        block
    }

    fn queue_next_leader_burn(ledger: &mut Ledger, wallets: &[Wallet]) {
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(wallets, &leader);
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
    }

    fn transfer_with_extra_zero_outputs(
        ledger: &Ledger,
        wallet: &Wallet,
        to: &str,
        amount: Amount,
        fee: Amount,
        extra_outputs: usize,
    ) -> Transaction {
        let required = amount.checked_add(fee).unwrap();
        let (inputs, input_total) = ledger.select_inputs(wallet.address(), required).unwrap();
        let mut outputs = vec![TxOutput {
            address: to.to_string(),
            amount,
        }];
        outputs.extend((0..extra_outputs).map(|_| TxOutput {
            address: to.to_string(),
            amount: 0,
        }));
        let change = input_total - required;
        if change > 0 {
            outputs.push(TxOutput {
                address: wallet.address().to_string(),
                amount: change,
            });
        }
        UnsignedUtxoTransaction::Transfer {
            inputs,
            outputs,
            fee,
        }
        .sign(wallet)
    }

    #[test]
    fn wallet_utxos_only_include_outputs_owned_by_address() {
        let alice = Wallet::from_seed("wallet-utxos-alice");
        let bob = Wallet::from_seed("wallet-utxos-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3]);
        ledger.utxos.insert(
            named_test_outpoint("bob"),
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
    fn transaction_economic_size_uses_compact_canonical_fields() {
        let alice = Wallet::from_seed("economic-size-alice");
        let bob = Wallet::from_seed("economic-size-bob");
        let ledger = ledger_with_wallet_utxos(&alice, &[1, 1, 2]);
        let selected = vec![
            test_utxo_outpoint(0),
            test_utxo_outpoint(1),
            test_utxo_outpoint(2),
        ];

        let tx = ledger
            .build_transfer_with_inputs(&alice, bob.address(), 3, 0, &selected)
            .unwrap();

        assert!(tx.economic_size_bytes() < tx.serialized_size_bytes().unwrap());
        assert_eq!(
            tx.economic_size_bytes(),
            1 + 1 + (3 * (32 + 1 + 32)) + 1 + (2 * (32 + 1)) + 1 + 64
        );
    }

    #[test]
    fn blinded_fee_rate_size_uses_visible_envelope_bytes() {
        let alice = Wallet::from_seed("blinded-fee-size-alice");
        let ledger = ledger_with_wallet_utxos(&alice, &[10]);
        let built = ledger
            .build_blinded_burn(&alice, 1, 2, ledger.height() + 4)
            .unwrap();

        assert_eq!(
            built.transaction.fee_rate_size_bytes(),
            built.transaction.serialized_size_bytes().unwrap()
        );
        assert!(
            built.transaction.fee_rate_size_bytes() > built.transaction.encrypted_size as usize
        );
    }

    #[test]
    fn blinded_fee_split_rounds_remainder_to_reveal_executor() {
        let committer = Wallet::from_seed("blinded-split-committer");
        let executor = Wallet::from_seed("blinded-split-executor");
        let commitment = "01".repeat(32);
        let active = ActiveBlindedTransaction {
            transaction: BlindedTransaction {
                commitment: commitment.clone(),
                fee: 1,
                encrypted_size: 1,
                expires_at_height: 2,
                nonce: "02".repeat(BLINDED_NONCE_BYTES),
                ciphertext: "03".to_string(),
                payload_hash: "04".repeat(32),
            },
            included_height: 1,
            included_by: committer.address().to_string(),
        };
        let transaction = Transaction::Transfer {
            inputs: Vec::new(),
            outputs: Vec::new(),
            fee: 1,
            signature: String::new(),
        };
        let mut utxos = BTreeMap::new();

        credit_blinded_fee_outputs(
            &mut utxos,
            &active,
            executor.address(),
            &transaction,
            REVEAL_COMMITTEE_SIZE,
        )
        .unwrap();

        assert!(!utxos.contains_key(&blinded_committer_fee_outpoint(&commitment)));
        assert_eq!(
            utxos.get(&blinded_executor_fee_outpoint(&commitment)),
            Some(&TxOutput {
                address: executor.address().to_string(),
                amount: 1,
            })
        );
    }

    #[test]
    fn blinded_executor_fee_scales_with_included_reveal_bundles() {
        let committer = Wallet::from_seed("blinded-scale-committer");
        let executor = Wallet::from_seed("blinded-scale-executor");
        let commitment = "05".repeat(32);
        let active = ActiveBlindedTransaction {
            transaction: BlindedTransaction {
                commitment: commitment.clone(),
                fee: 7,
                encrypted_size: 1,
                expires_at_height: 2,
                nonce: "02".repeat(BLINDED_NONCE_BYTES),
                ciphertext: "03".to_string(),
                payload_hash: "04".repeat(32),
            },
            included_height: 1,
            included_by: committer.address().to_string(),
        };
        let transaction = Transaction::Transfer {
            inputs: Vec::new(),
            outputs: Vec::new(),
            fee: 7,
            signature: String::new(),
        };
        let mut utxos = BTreeMap::new();

        credit_blinded_fee_outputs(&mut utxos, &active, executor.address(), &transaction, 1)
            .unwrap();

        assert_eq!(
            utxos.get(&blinded_committer_fee_outpoint(&commitment)),
            Some(&TxOutput {
                address: committer.address().to_string(),
                amount: 3,
            })
        );
        assert_eq!(
            utxos.get(&blinded_executor_fee_outpoint(&commitment)),
            Some(&TxOutput {
                address: executor.address().to_string(),
                amount: 1,
            })
        );
    }

    #[test]
    fn transfer_rejects_invalid_recipient_address() {
        let alice = Wallet::from_seed("invalid-transfer-recipient-alice");
        let ledger = ledger_with_wallet_utxos(&alice, &[10]);

        let error = ledger.build_transfer(&alice, "aa", 1, 0).unwrap_err();

        assert!(format!("{error:#}").contains("invalid transfer recipient address"));
    }

    #[test]
    fn mine_rejects_invalid_recipient_address_before_pow() {
        let ledger = Ledger::new(BTreeMap::new(), 1);

        let error = ledger.build_mine("aa").unwrap_err();

        assert!(format!("{error:#}").contains("invalid mine recipient address"));
    }

    #[test]
    fn mine_search_respects_nonce_attempt_limit() {
        let alice = Wallet::from_seed("bounded-mine-search-alice");
        let ledger = Ledger::new(BTreeMap::new(), 1);

        let outcome = ledger.search_mine(alice.address(), 1, 0, 0).unwrap();

        assert!(outcome.transaction.is_none());
        assert_eq!(outcome.next_nonce, 0);
        assert_eq!(outcome.attempts, 0);
    }

    #[test]
    fn mempool_rejects_invalid_input_outpoint_id() {
        let alice = Wallet::from_seed("invalid-outpoint-alice");
        let bob = Wallet::from_seed("invalid-outpoint-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[10]);
        let unsigned = UnsignedUtxoTransaction::Transfer {
            inputs: vec![UnsignedTxInput {
                outpoint: OutPoint {
                    txid: "aa".to_string(),
                    index: 0,
                },
                owner: alice.address().to_string(),
            }],
            outputs: vec![TxOutput {
                address: bob.address().to_string(),
                amount: 1,
            }],
            fee: 0,
        };
        let transaction = unsigned.sign(&alice);

        let error = ledger.submit_transaction(transaction).unwrap_err();

        assert!(format!("{error:#}").contains("invalid input outpoint txid"));
        assert!(ledger.pending().is_empty());
    }

    #[test]
    fn missing_input_transaction_goes_to_orphan_pool_not_pending_mempool() {
        let alice = Wallet::from_seed("missing-input-orphan-alice");
        let bob = Wallet::from_seed("missing-input-orphan-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[10]);
        let transaction = UnsignedUtxoTransaction::Transfer {
            inputs: vec![UnsignedTxInput {
                outpoint: OutPoint {
                    txid: hex_hash("missing-input-orphan"),
                    index: 0,
                },
                owner: alice.address().to_string(),
            }],
            outputs: vec![TxOutput {
                address: bob.address().to_string(),
                amount: 1,
            }],
            fee: 0,
        }
        .sign(&alice);

        let outcome = ledger.submit_transaction_with_outcome(transaction).unwrap();

        assert_eq!(outcome, TransactionSubmitOutcome::Added);
        assert!(ledger.pending().is_empty());
        assert_eq!(ledger.orphan_transactions().len(), 1);
    }

    #[test]
    fn vdf_retarget_observed_block_time_is_clamped() {
        assert_eq!(
            clamped_vdf_retarget_observed_block_ms(1),
            MIN_VDF_RETARGET_OBSERVED_BLOCK_MS
        );
        assert_eq!(
            clamped_vdf_retarget_observed_block_ms(VDF_TARGET_BLOCK_MS),
            VDF_TARGET_BLOCK_MS
        );
        assert_eq!(
            clamped_vdf_retarget_observed_block_ms(u64::MAX),
            MAX_VDF_RETARGET_OBSERVED_BLOCK_MS
        );
    }

    #[test]
    fn vdf_retarget_observed_block_time_ignores_ticket_fallback_ranks() {
        let parent = vdf_retarget_sample_block(0, FinalizerMode::Ticket, 0);
        let primary_child =
            vdf_retarget_sample_block(VDF_TARGET_BLOCK_MS, FinalizerMode::Ticket, 0);
        let rank_one_child =
            vdf_retarget_sample_block(VDF_TARGET_BLOCK_MS, FinalizerMode::Ticket, 1);
        let rank_two_child =
            vdf_retarget_sample_block(VDF_TARGET_BLOCK_MS * 2, FinalizerMode::Ticket, 2);

        assert_eq!(
            vdf_retarget_observed_block_ms(&parent, &primary_child),
            Some(VDF_TARGET_BLOCK_MS)
        );
        assert_eq!(
            vdf_retarget_observed_block_ms(&parent, &rank_one_child),
            None
        );
        assert_eq!(
            vdf_retarget_observed_block_ms(&parent, &rank_two_child),
            None
        );
    }

    #[test]
    fn vdf_retarget_observed_block_time_ignores_recovery_blocks() {
        let parent = vdf_retarget_sample_block(0, FinalizerMode::Ticket, 0);
        let recovery_child =
            vdf_retarget_sample_block(RECOVERY_BLOCK_DELAY_MS, FinalizerMode::Recovery, 0);

        assert_eq!(
            vdf_retarget_observed_block_ms(&parent, &recovery_child),
            None
        );
    }

    #[test]
    fn vdf_retarget_keeps_rounds_inside_deadband() {
        let current = 1_000;
        let low_deadband_edge =
            VDF_TARGET_BLOCK_MS - VDF_TARGET_BLOCK_MS * VDF_RETARGET_DEADBAND_PERCENT as u64 / 100;
        let high_deadband_edge =
            VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS * VDF_RETARGET_DEADBAND_PERCENT as u64 / 100;

        assert_eq!(retarget_vdf_rounds(current, low_deadband_edge), current);
        assert_eq!(retarget_vdf_rounds(current, VDF_TARGET_BLOCK_MS), current);
        assert_eq!(retarget_vdf_rounds(current, high_deadband_edge), current);
    }

    #[test]
    fn vdf_retarget_limits_each_step_to_two_percent() {
        let current = 1_000;

        assert_eq!(
            retarget_vdf_rounds(current, MIN_VDF_RETARGET_OBSERVED_BLOCK_MS),
            1_020
        );
        assert_eq!(
            retarget_vdf_rounds(current, MAX_VDF_RETARGET_OBSERVED_BLOCK_MS),
            980
        );
    }

    #[test]
    fn vdf_rounds_retarget_below_legacy_u32_limit_after_slow_blocks() {
        let wallet = Wallet::from_seed("vdf-rounds-slow-above-u32");
        let initial_rounds = u64::from(u32::MAX);
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            initial_rounds,
        )
        .unwrap();

        let block1 = apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
        assert_eq!(block1.vdf_rounds, initial_rounds);
        assert_eq!(ledger.vdf_rounds(), initial_rounds);

        let block2 = apply_preverified_burn_block_at(
            &mut ledger,
            &wallet,
            VDF_TARGET_BLOCK_MS + VDF_TARGET_BLOCK_MS * 2,
        );
        assert_eq!(block2.vdf_rounds, initial_rounds);

        assert!(
            ledger.vdf_rounds() < initial_rounds,
            "slow blocks should retarget below the legacy u32 VDF rounds ceiling"
        );
    }

    #[test]
    fn fallback_block_is_excluded_from_vdf_retarget_observations() {
        let alice = Wallet::from_seed("fallback-retarget-alice");
        let bob = Wallet::from_seed("fallback-retarget-bob");
        let wallets = [&alice, &bob];
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 1_000);
        genesis.insert(bob.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![
                GenesisBurn::new(alice.address(), 1),
                GenesisBurn::new(bob.address(), 1),
            ],
            100,
        )
        .unwrap();

        let primary = ledger.expected_leader_for_next_block().unwrap();
        let primary_wallet = wallets
            .into_iter()
            .find(|wallet| wallet.address() == primary)
            .unwrap();
        apply_preverified_burn_block_at(&mut ledger, primary_wallet, VDF_TARGET_BLOCK_MS);
        assert_eq!(ledger.vdf_rounds(), 100);

        let primary = ledger.expected_leader_for_next_block().unwrap();
        let fallback = wallets
            .into_iter()
            .find(|wallet| wallet.address() != primary)
            .unwrap();
        let timestamp_ms = ledger.tip().timestamp_ms + 1;
        let block = apply_preverified_burn_block_at(&mut ledger, fallback, timestamp_ms);

        assert_eq!(block.finalizer_rank, 1);
        assert_eq!(block.vdf_rounds, 200);
        assert_eq!(ledger.vdf_rounds(), 100);
    }

    #[test]
    fn recovery_block_is_excluded_from_vdf_retarget_observations() {
        let alice = Wallet::from_seed("recovery-retarget-alice");
        let bob = Wallet::from_seed("recovery-retarget-bob");
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 1_000);
        genesis.insert(bob.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![GenesisBurn::new(alice.address(), 1)],
            100,
        )
        .unwrap();

        apply_preverified_burn_block_at(&mut ledger, &alice, VDF_TARGET_BLOCK_MS);
        assert_eq!(ledger.vdf_rounds(), 100);

        let burn = ledger.build_burn(&bob, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger
            .mine_recovery_block(&bob, VDF_TARGET_BLOCK_MS + RECOVERY_BLOCK_DELAY_MS)
            .unwrap();
        assert_eq!(block.finalizer_mode, FinalizerMode::Recovery);
        ledger.apply_block(block).unwrap();

        assert_eq!(ledger.vdf_rounds(), 100);
    }

    #[test]
    fn generated_vdf_retarget_decreases_after_slow_blocks_above_legacy_limit() {
        let legacy_limit = u64::from(u32::MAX);
        let slow_observed_ms = [
            VDF_TARGET_BLOCK_MS * 6 / 5,
            VDF_TARGET_BLOCK_MS * 2,
            VDF_TARGET_BLOCK_MS * 3,
            MAX_VDF_RETARGET_OBSERVED_BLOCK_MS,
        ];

        for seed in 0..16_u64 {
            let wallet = Wallet::from_seed(&format!("generated-vdf-retarget-{seed}"));
            let initial_rounds = legacy_limit + 1 + seed * 1_000_003;
            let mut allocations = BTreeMap::new();
            allocations.insert(wallet.address().to_string(), 1_000);
            let mut ledger = Ledger::new_with_genesis_burns(
                allocations,
                vec![GenesisBurn::new(wallet.address(), 1)],
                initial_rounds,
            )
            .unwrap();
            let observed_ms = slow_observed_ms[seed as usize % slow_observed_ms.len()];

            apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
            let second = apply_preverified_burn_block_at(
                &mut ledger,
                &wallet,
                VDF_TARGET_BLOCK_MS + observed_ms,
            );

            assert_eq!(second.vdf_rounds, initial_rounds);
            assert!(
                ledger.vdf_rounds() < initial_rounds,
                "seed {seed} with observed {observed_ms}ms should lower VDF rounds from {initial_rounds}, got {}",
                ledger.vdf_rounds()
            );

            let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
            ledger.submit_transaction(burn).unwrap();
            let next_work = ledger
                .prepare_next_block(wallet.address(), second.timestamp_ms + VDF_TARGET_BLOCK_MS)
                .unwrap();
            assert_eq!(next_work.vdf_rounds(), ledger.vdf_rounds());
        }
    }

    #[test]
    fn block_timestamp_future_check_uses_supplied_network_time() {
        let wallet = Wallet::from_seed("adjusted-time-domain");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let mut ledger = Ledger::new(allocations, 1);

        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        assert!(ledger.submit_transaction(burn).unwrap());
        let block = ledger.mine_next_block(&wallet, 10 * 60 * 1_000).unwrap();

        let error = ledger.apply_block_at(block, 1_000).unwrap_err();

        assert!(format!("{error:#}").contains("too far in the future"));
    }

    #[test]
    fn ticket_block_timestamp_uses_finalizer_rank_time_slot() {
        let alice = Wallet::from_seed("rank-slot-alice");
        let bob = Wallet::from_seed("rank-slot-bob");
        let wallets = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 1_000);
        allocations.insert(bob.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![
                GenesisBurn::new(alice.address(), 1),
                GenesisBurn::new(bob.address(), 1),
            ],
            100,
        )
        .unwrap();

        let primary =
            wallet_for_address(&wallets, &ledger.expected_leader_for_next_block().unwrap());
        let burn = ledger.build_burn(primary, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let work = ledger.prepare_next_block(primary.address(), 1).unwrap();
        assert_eq!(work.timestamp_ms(), 1);
        let block = work.finish(primary, "preverified-vdf".to_string());
        ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

        let fallback = wallets
            .iter()
            .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
            .expect("expected rank 1 fallback");
        let burn = ledger.build_burn(fallback, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let parent_timestamp = ledger.tip().timestamp_ms;
        let work = ledger
            .prepare_next_block(fallback.address(), parent_timestamp + 1)
            .unwrap();

        assert_eq!(work.timestamp_ms(), parent_timestamp + VDF_TARGET_BLOCK_MS);
        assert_eq!(work.vdf_rounds(), ledger.vdf_rounds() * 2);
    }

    #[test]
    fn late_ticket_vdf_completion_is_visible_to_retarget() {
        let wallet = Wallet::from_seed("late-ticket-vdf-wallet");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            100,
        )
        .unwrap();

        apply_preverified_burn_block_at(&mut ledger, &wallet, VDF_TARGET_BLOCK_MS);
        assert_eq!(ledger.vdf_rounds(), 100);

        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let work = ledger
            .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
            .unwrap();
        let scheduled_timestamp = work.timestamp_ms();
        let late_timestamp = ledger.tip().timestamp_ms + VDF_TARGET_BLOCK_MS * 3;
        assert!(late_timestamp > scheduled_timestamp);

        let block = work.finish_at(&wallet, "preverified-vdf".to_string(), late_timestamp);
        assert_eq!(block.timestamp_ms, late_timestamp);
        ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

        assert!(
            ledger.vdf_rounds() < 100,
            "late VDF completion should lower future VDF rounds"
        );
    }

    #[test]
    fn block_before_finalizer_rank_time_slot_is_rejected() {
        let alice = Wallet::from_seed("rank-slot-reject-alice");
        let bob = Wallet::from_seed("rank-slot-reject-bob");
        let wallets = [alice.clone(), bob.clone()];
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 1_000);
        allocations.insert(bob.address().to_string(), 1_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![
                GenesisBurn::new(alice.address(), 1),
                GenesisBurn::new(bob.address(), 1),
            ],
            100,
        )
        .unwrap();

        let primary =
            wallet_for_address(&wallets, &ledger.expected_leader_for_next_block().unwrap());
        let burn = ledger.build_burn(primary, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let work = ledger.prepare_next_block(primary.address(), 1).unwrap();
        let block = work.finish(primary, "preverified-vdf".to_string());
        ledger.apply_preverified_block_at(block, u64::MAX).unwrap();

        let fallback = wallets
            .iter()
            .find(|wallet| ledger.finalizer_rank_for_next_block(wallet.address()) == Some(1))
            .expect("expected rank 1 fallback");
        let burn = ledger.build_burn(fallback, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let parent_timestamp = ledger.tip().timestamp_ms;
        let work = ledger
            .prepare_next_block(fallback.address(), parent_timestamp + 1)
            .unwrap();
        let mut block = work.finish(fallback, "preverified-vdf".to_string());
        block.timestamp_ms = parent_timestamp + VDF_TARGET_BLOCK_MS - 1;
        block.hash = block.compute_hash();

        let error = ledger
            .apply_preverified_block_at(block, u64::MAX)
            .unwrap_err();

        assert!(format!("{error:#}").contains("before finalizer rank 1 time slot"));
    }

    #[test]
    fn miner_skips_oversized_pending_transaction_and_keeps_fitting_fee_transaction() {
        let alice = Wallet::from_seed("oversized-select-alice");
        let bob = Wallet::from_seed("oversized-select-bob");
        let carol = Wallet::from_seed("oversized-select-carol");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 1);
        allocations.insert(bob.address().to_string(), 300_000);
        allocations.insert(carol.address().to_string(), 300_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(alice.address(), 1)],
            10,
        )
        .unwrap();
        let burn = ledger.build_burn(&alice, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let oversized =
            transfer_with_extra_zero_outputs(&ledger, &bob, alice.address(), 1, 100_000, 4_000);
        let fitting = ledger
            .build_transfer(&carol, alice.address(), 1, 5)
            .unwrap();
        assert!(oversized.serialized_size_bytes().unwrap() > MAX_BLOCK_BYTES);
        ledger.submit_transaction(oversized.clone()).unwrap();
        ledger.submit_transaction(fitting.clone()).unwrap();

        let block = ledger.mine_next_block(&alice, 1).unwrap();
        let signatures = block
            .transactions
            .iter()
            .map(|tx| tx.signature().to_string())
            .collect::<Vec<_>>();

        assert!(!signatures.contains(&oversized.signature().to_string()));
        assert!(signatures.contains(&fitting.signature().to_string()));
        assert!(block.serialized_size_bytes().unwrap() <= MAX_BLOCK_BYTES);
    }

    #[test]
    fn transfer_can_spend_selected_utxos_when_they_cover_amount_and_fee() {
        let alice = Wallet::from_seed("selected-utxos-alice");
        let bob = Wallet::from_seed("selected-utxos-bob");
        let mut ledger = ledger_with_wallet_utxos(&alice, &[2, 3, 5]);
        let selected = vec![test_utxo_outpoint(2)];

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
        let selected = vec![test_utxo_outpoint(0)];

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
            named_test_outpoint("carol"),
            TxOutput {
                address: carol.address().to_string(),
                amount: 5,
            },
        );
        let selected = vec![named_test_outpoint("carol")];

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
            finalizer_mode: FinalizerMode::Ticket,
            finalizer_rank: 0,
            reward: BLOCK_REWARD,
            vdf_rounds: 1,
            vdf_output: "vdf".to_string(),
            leader_proof: Some(LeaderProof {
                ticket_id: "high-burn".to_string(),
                public_key: "alice".to_string(),
                signature: "signature".to_string(),
            }),
            blinded_transactions: Vec::new(),
            reveal_bundles: Vec::new(),
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
    fn burn_leader_ranks_for_block_reconstructs_historical_ticket_order() {
        let alice = Wallet::from_seed("burn-rank-alice");
        let bob = Wallet::from_seed("burn-rank-bob");
        let mut allocations = BTreeMap::new();
        allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
        allocations.insert(bob.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![
                GenesisBurn::new(alice.address(), MICRO_IUNA),
                GenesisBurn::new(bob.address(), MICRO_IUNA),
            ],
            1,
        )
        .unwrap();

        let ranks = ledger.burn_leader_ranks_for_block(1).unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();

        assert_eq!(ranks.len(), 2);
        assert_eq!(ranks[0].rank, 0);
        assert_eq!(ranks[0].owner, leader);
        assert!(ranks.iter().all(|rank| rank.amount == MICRO_IUNA));
        assert_eq!(ledger.burn_leader_ranks_for_block(0).unwrap(), Vec::new());
    }

    #[test]
    fn mine_recipient_is_bound_to_proof_hash() {
        let alice = Wallet::from_seed("mine-proof-alice");
        let bob = Wallet::from_seed("mine-proof-bob");
        let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);
        let mut forged = unsigned_mine(&ledger, alice.address());
        if let Transaction::Mine { recipient, .. } = &mut forged {
            *recipient = bob.address().to_string();
        }

        let error = ledger.submit_transaction(forged).unwrap_err();

        assert!(format!("{error:#}").contains("proof hash is invalid"));
    }

    #[test]
    fn mine_action_uses_fixed_reward_and_fixed_finalizer_fee() {
        let alice = Wallet::from_seed("mine-fixed-reward-alice");
        let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);

        let mine = ledger.build_mine(alice.address()).unwrap();

        assert_eq!(mine.amount(), MINE_REWARD);
        assert_eq!(mine.fee(), MINE_FINALIZER_FEE);
        assert!(ledger.submit_transaction(mine).unwrap());
    }

    #[test]
    fn burn_fee_goes_to_block_finalizer() {
        let alice = Wallet::from_seed("burn-fee-finalizer-alice");
        let mut ledger = ledger_with_allocation(&alice, MICRO_IUNA);

        let burn_fee = 12;
        let burn = ledger
            .build_burn(&alice, TEST_BURN_AMOUNT, burn_fee)
            .unwrap();
        ledger.submit_transaction(burn).unwrap();
        let prepared = ledger.prepare_next_block(alice.address(), 1).unwrap();

        assert_eq!(prepared.reward, burn_fee);
    }

    #[test]
    fn blinded_burn_commits_ciphertext_and_reveal_executes_later() {
        let alice = Wallet::from_seed("blinded-burn-finalizer-alice");
        let bob = Wallet::from_seed("blinded-burn-finalizer-bob");
        let carol = Wallet::from_seed("blinded-burn-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let fee = 7;
        let burn_amount = 3;
        let before_carol = ledger.balance_of(carol.address());

        let blinded = ledger
            .build_blinded_burn(&carol, burn_amount, fee, ledger.height() + 4)
            .unwrap();
        assert!(!blinded.transaction.ciphertext.contains("burn"));
        assert!(!blinded.transaction.ciphertext.contains(carol.address()));
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);

        let commit_block = mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
        let inclusion_finalizer = commit_block.miner.clone();
        assert_eq!(
            commit_block
                .transactions
                .iter()
                .filter(|transaction| transaction.is_burn())
                .count(),
            1
        );
        assert_eq!(commit_block.blinded_transactions, vec![blinded.transaction]);
        assert_eq!(commit_block.reward, 0);
        let before_inclusion_finalizer = ledger.balance_of(&inclusion_finalizer);

        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        let reveal_block =
            mine_preverified_as_next_leader_with_reveal_bundles(&mut ledger, &finalizers, 2);
        let reveal_executor = reveal_block.miner.clone();

        assert_eq!(reveal_block.all_blinded_reveals().len(), 1);
        assert_eq!(
            ledger.balance_of(carol.address()),
            before_carol - burn_amount - fee
        );
        assert!(ledger.tickets.iter().any(|ticket| {
            ticket.owner == carol.address()
                && ticket.amount == burn_amount
                && ticket.eligible_from_height
                    == reveal_block.height + ledger.launch_profile.ticket_maturity_delay_heights
        }));
        let reveal_plaintext_burn_spent_by_inclusion_finalizer = reveal_block
            .transactions
            .iter()
            .filter(|transaction| {
                transaction.is_burn() && transaction.sender() == inclusion_finalizer.as_str()
            })
            .fold(0_u64, |total, transaction| {
                total + transaction.amount() + transaction.fee()
            });
        let committer_fee = fee / 2;
        let executor_fee = (fee - committer_fee)
            * reveal_block.included_reveal_bundle_count() as u64
            / REVEAL_COMMITTEE_SIZE as u64;
        assert_eq!(
            ledger
                .utxos
                .get(&blinded_committer_fee_outpoint(
                    &commit_block.blinded_transactions[0].commitment
                ))
                .unwrap(),
            &TxOutput {
                address: inclusion_finalizer.clone(),
                amount: committer_fee,
            }
        );
        assert_eq!(
            ledger
                .utxos
                .get(&blinded_executor_fee_outpoint(
                    &commit_block.blinded_transactions[0].commitment
                ))
                .unwrap(),
            &TxOutput {
                address: reveal_executor.clone(),
                amount: executor_fee,
            }
        );
        let inclusion_finalizer_fee = if inclusion_finalizer == reveal_executor {
            committer_fee + executor_fee
        } else {
            committer_fee
        };
        assert_eq!(
            ledger.balance_of(&inclusion_finalizer),
            before_inclusion_finalizer + inclusion_finalizer_fee
                - reveal_plaintext_burn_spent_by_inclusion_finalizer
        );
    }

    #[test]
    fn reveal_bundle_hashes_are_bound_to_next_block_vdf_seed() {
        let alice = Wallet::from_seed("bundle-seed-alice");
        let bob = Wallet::from_seed("bundle-seed-bob");
        let carol = Wallet::from_seed("bundle-seed-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let leader_wallet = wallet_for_address(&finalizers, &leader);
        let bundles = ledger
            .reveal_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                let wallet = wallet_for_address(&finalizers, &member.owner);
                ledger.build_reveal_bundle(wallet).unwrap()
            })
            .collect::<Vec<_>>();
        if bundles.len() > 1 {
            let mut reversed = bundles.clone();
            reversed.reverse();
            let error = ledger
                .validate_next_block_reveal_bundles(reversed)
                .unwrap_err();
            assert!(format!("{error:#}").contains("reveal bundles are not in slot order"));
        }

        let without_bundles = ledger
            .prepare_next_block(leader_wallet.address(), ledger.tip().timestamp_ms + 1)
            .unwrap();
        let with_bundles = ledger
            .prepare_next_block_with_reveal_bundles(
                leader_wallet.address(),
                ledger.tip().timestamp_ms + 1,
                bundles,
            )
            .unwrap();

        assert_ne!(without_bundles.vdf_seed(), with_bundles.vdf_seed());
    }

    #[test]
    fn reveal_bundle_validation_rejects_wrong_signature_and_slot() {
        let alice = Wallet::from_seed("bundle-invalid-alice");
        let bob = Wallet::from_seed("bundle-invalid-bob");
        let carol = Wallet::from_seed("bundle-invalid-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        let member = ledger.reveal_committee_for_next_block()[0].clone();
        let wallet = wallet_for_address(&finalizers, &member.owner);
        let bundle = ledger.build_reveal_bundle(wallet).unwrap().unwrap();

        let mut wrong_signature = bundle.clone();
        wrong_signature.signature = "00".repeat(SIGNATURE_BYTES);
        let error = ledger
            .validate_next_block_reveal_bundles(vec![wrong_signature])
            .unwrap_err();
        assert!(format!("{error:#}").contains("reveal bundle signature is invalid"));

        let mut wrong_slot = bundle;
        wrong_slot.slot = REVEAL_COMMITTEE_SIZE as u8 - 1;
        let error = ledger
            .validate_next_block_reveal_bundles(vec![wrong_slot])
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("reveal bundle slot is not assigned")
                || format!("{error:#}").contains("reveal bundle member is not assigned to slot")
        );
    }

    #[test]
    fn blinded_reveal_with_wrong_key_is_rejected_in_block() {
        let alice = Wallet::from_seed("blinded-wrong-key-finalizer-alice");
        let bob = Wallet::from_seed("blinded-wrong-key-finalizer-bob");
        let carol = Wallet::from_seed("blinded-wrong-key-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(filler_burn).unwrap();
        let mut prepared = ledger
            .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
            .unwrap();
        let committee_member = ledger.reveal_committee_for_next_block()[0].clone();
        let committee_wallet = wallet_for_address(&finalizers, &committee_member.owner);
        let wrong_reveal = BlindedReveal {
            commitment: blinded.transaction.commitment,
            key: "00".repeat(BLINDED_KEY_BYTES),
        };
        prepared
            .reveal_bundles
            .push(committee_wallet.reveal_bundle(RevealBundlePayload {
                height: prepared.height,
                prev_hash: prepared.prev_hash.clone(),
                slot: committee_member.slot,
                member: committee_wallet.address().to_string(),
                reveals: vec![wrong_reveal],
            }));
        let block = prepared.finish(wallet, "preverified-vdf".to_string());

        let error = ledger
            .apply_preverified_block_at(block, u64::MAX)
            .unwrap_err();

        assert!(format!("{error:#}").contains("failed to decrypt blinded transaction payload"));
    }

    #[test]
    fn expired_blinded_reveal_is_not_selected() {
        let alice = Wallet::from_seed("blinded-expire-finalizer-alice");
        let bob = Wallet::from_seed("blinded-expire-finalizer-bob");
        let carol = Wallet::from_seed("blinded-expire-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 2)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);

        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(filler_burn).unwrap();
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 2);

        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        assert!(ledger.valid_pending_blinded_reveals().is_empty());
    }

    #[test]
    fn recovery_block_includes_pending_blinded_transactions_when_space_allows() {
        let alice = Wallet::from_seed("recovery-blinded-commit-alice");
        let bob = Wallet::from_seed("recovery-blinded-commit-bob");
        let carol = Wallet::from_seed("recovery-blinded-commit-carol");
        let mut ledger = ledger_with_finalizers(
            &[alice],
            &[(&bob, 10 * MICRO_IUNA), (&carol, 10 * MICRO_IUNA)],
        );
        let blinded = ledger
            .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(recovery_burn).unwrap();

        let block = ledger
            .mine_recovery_block(&bob, RECOVERY_BLOCK_DELAY_MS)
            .unwrap();

        assert!(
            block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.transaction.commitment)
        );
    }

    #[test]
    fn recovery_block_size_selection_uses_recovery_skeleton() {
        let alice = Wallet::from_seed("recovery-size-commit-alice");
        let bob = Wallet::from_seed("recovery-size-commit-bob");
        let carol = Wallet::from_seed("recovery-size-commit-carol");
        let mut ledger = ledger_with_finalizers(
            &[alice],
            &[(&bob, 10 * MICRO_IUNA), (&carol, 10 * MICRO_IUNA)],
        );
        let blinded = ledger
            .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(recovery_burn.clone()).unwrap();
        let recovery_selection = BlockSelection {
            transactions: vec![recovery_burn],
            blinded_transactions: vec![blinded.transaction.clone()],
        };
        let recovery_estimate =
            estimated_block_selection_size_bytes(&recovery_selection, true).unwrap();
        let ticket_estimate =
            estimated_block_selection_size_bytes(&recovery_selection, false).unwrap();
        assert!(recovery_estimate < ticket_estimate);

        ledger.launch_profile.max_block_bytes = recovery_estimate;
        let tight_block = ledger
            .mine_recovery_block(&bob, RECOVERY_BLOCK_DELAY_MS)
            .unwrap();

        assert!(
            tight_block
                .blinded_transactions
                .iter()
                .any(|transaction| transaction.commitment == blinded.transaction.commitment)
        );
    }

    #[test]
    fn recovery_block_includes_pending_blinded_reveals_when_space_allows() {
        let alice = Wallet::from_seed("recovery-blinded-reveal-alice");
        let bob = Wallet::from_seed("recovery-blinded-reveal-bob");
        let carol = Wallet::from_seed("recovery-blinded-reveal-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, MICRO_IUNA, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
        ledger
            .submit_blinded_reveal(blinded.reveal.clone())
            .unwrap();
        let recovery_burn = ledger.build_burn(&bob, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(recovery_burn).unwrap();

        let bundles = ledger
            .reveal_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                let wallet = wallet_for_address(&finalizers, &member.owner);
                ledger.build_reveal_bundle(wallet).unwrap()
            })
            .collect::<Vec<_>>();
        let prepared = ledger
            .prepare_recovery_block_with_reveal_bundles(
                bob.address(),
                ledger.recovery_block_min_timestamp(),
                bundles,
            )
            .unwrap();
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        let block = prepared.finish(&bob, vdf_output);

        assert!(
            block
                .all_blinded_reveals()
                .iter()
                .any(|reveal| reveal.commitment == blinded.transaction.commitment)
        );
    }

    #[test]
    fn blinded_transaction_expiring_at_next_height_is_not_selected() {
        let alice = Wallet::from_seed("blinded-next-expire-finalizer-alice");
        let bob = Wallet::from_seed("blinded-next-expire-finalizer-bob");
        let carol = Wallet::from_seed("blinded-next-expire-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 1)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction)
            .unwrap();

        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let error = ledger.prepare_next_block(wallet.address(), 1).unwrap_err();

        assert!(format!("{error:#}").contains("block must include at least one burn transaction"));
    }

    #[test]
    fn blinded_transaction_expiry_cannot_exceed_protocol_window() {
        let alice = Wallet::from_seed("blinded-window-finalizer-alice");
        let bob = Wallet::from_seed("blinded-window-finalizer-bob");
        let carol = Wallet::from_seed("blinded-window-carol");
        let finalizers = [alice, bob];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let max_expiry = ledger
            .height()
            .saturating_add(MAX_BLINDED_TRANSACTION_EXPIRY_HEIGHTS);

        ledger.build_blinded_burn(&carol, 3, 7, max_expiry).unwrap();

        let error = ledger
            .build_blinded_burn(&carol, 3, 7, max_expiry + 1)
            .unwrap_err();
        assert!(format!("{error:#}").contains("expiry is too far in the future"));

        let mut forged = ledger
            .build_blinded_burn(&carol, 3, 7, max_expiry)
            .unwrap()
            .transaction;
        forged.expires_at_height = max_expiry + 1;
        forged.commitment = blinded_transaction_commitment(&forged).unwrap();
        let error = ledger.submit_blinded_transaction(forged).unwrap_err();
        assert!(format!("{error:#}").contains("expiry is too far in the future"));
    }

    #[test]
    fn blinded_transaction_does_not_satisfy_plaintext_burn_requirement() {
        let alice = Wallet::from_seed("blinded-no-burn-finalizer-alice");
        let bob = Wallet::from_seed("blinded-no-burn-finalizer-bob");
        let carol = Wallet::from_seed("blinded-no-burn-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction)
            .unwrap();

        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let error = ledger.prepare_next_block(wallet.address(), 1).unwrap_err();

        assert!(format!("{error:#}").contains("block must include at least one burn transaction"));
    }

    #[test]
    fn revealed_blinded_transaction_cannot_be_included_again() {
        let alice = Wallet::from_seed("blinded-duplicate-finalizer-alice");
        let bob = Wallet::from_seed("blinded-duplicate-finalizer-bob");
        let carol = Wallet::from_seed("blinded-duplicate-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut ledger = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 6)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader(&mut ledger, &finalizers, 1);
        ledger
            .submit_blinded_reveal(blinded.reveal.clone())
            .unwrap();
        queue_next_leader_burn(&mut ledger, &finalizers);
        mine_preverified_as_next_leader_with_reveal_bundles(&mut ledger, &finalizers, 2);

        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallet_for_address(&finalizers, &leader);
        let filler_burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(filler_burn).unwrap();
        let mut prepared = ledger
            .prepare_next_block(wallet.address(), ledger.tip().timestamp_ms + 1)
            .unwrap();
        prepared
            .blinded_transactions
            .push(blinded.transaction.clone());
        let block = prepared.finish(wallet, "preverified-vdf".to_string());

        let error = ledger
            .apply_preverified_block_at(block, u64::MAX)
            .unwrap_err();

        assert!(format!("{error:#}").contains("blinded transaction is already on chain"));
    }

    #[test]
    fn abandoned_fork_blinded_transactions_return_to_mempool() {
        let alice = Wallet::from_seed("blinded-reorg-finalizer-alice");
        let bob = Wallet::from_seed("blinded-reorg-finalizer-bob");
        let carol = Wallet::from_seed("blinded-reorg-carol");
        let finalizers = [alice.clone(), bob.clone()];
        let mut local = ledger_with_finalizers(&finalizers, &[(&carol, 10 * MICRO_IUNA)]);
        let mut remote = local.clone();
        let blinded = local
            .build_blinded_burn(&carol, 3, 7, local.height() + 8)
            .unwrap();
        local
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        queue_next_leader_burn(&mut local, &finalizers);
        mine_preverified_as_next_leader(&mut local, &finalizers, 1);

        for timestamp_ms in [1, 2] {
            let leader = remote.expected_leader_for_next_block().unwrap();
            let wallet = wallet_for_address(&finalizers, &leader);
            let burn = remote.build_burn(wallet, 1, 0).unwrap();
            remote.submit_transaction(burn).unwrap();
            mine_preverified_as_next_leader(&mut remote, &finalizers, timestamp_ms);
        }

        assert!(
            local
                .extend_from_preverified_snapshot_at(remote.snapshot(), u64::MAX)
                .unwrap()
        );
        assert!(local.has_blinded_transaction(&blinded.transaction.commitment));
        assert_eq!(
            local.pending_blinded_transactions(),
            std::slice::from_ref(&blinded.transaction)
        );
    }

    #[test]
    fn block_selection_includes_mine_action_after_required_block_burn() {
        let alice = Wallet::from_seed("mine-fixed-reward-select-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

        let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let mine = ledger.build_mine(alice.address()).unwrap();
        ledger.submit_transaction(mine.clone()).unwrap();

        let block = ledger.mine_next_block(&alice, 1).unwrap();

        assert_eq!(
            block.transactions.iter().filter(|tx| tx.is_burn()).count(),
            1
        );
        assert!(
            block
                .transactions
                .iter()
                .any(|tx| tx.signature() == mine.signature())
        );
        assert_eq!(block.reward, MINE_FINALIZER_FEE);
    }

    #[test]
    fn block_selection_can_skip_mine_action_when_space_is_limited() {
        let alice = Wallet::from_seed("mine-space-limit-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);
        ledger.launch_profile.max_block_transactions = 2;

        let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let first_mine = ledger.build_mine(alice.address()).unwrap();
        ledger.submit_transaction(first_mine).unwrap();
        let second_mine = ledger.build_mine(alice.address()).unwrap();
        ledger.submit_transaction(second_mine).unwrap();

        let block = ledger.mine_next_block(&alice, 1).unwrap();

        assert_eq!(block.transactions.len(), 2);
        assert!(block.transactions.iter().any(Transaction::is_burn));
        assert_eq!(block.reward, MINE_FINALIZER_FEE);
    }

    #[test]
    fn pending_mine_outputs_are_not_spendable_until_confirmed() {
        let alice = Wallet::from_seed("pending-mine-spend-alice");
        let bob = Wallet::from_seed("pending-mine-spend-bob");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

        let mine = ledger.build_mine(alice.address()).unwrap();
        let mine_outpoint = OutPoint {
            txid: mine.signature().to_string(),
            index: 0,
        };
        ledger.submit_transaction(mine.clone()).unwrap();

        assert!(
            !ledger
                .available_utxos_for_address(alice.address())
                .unwrap()
                .iter()
                .any(|(outpoint, _)| outpoint == &mine_outpoint)
        );
        let pending_error = ledger
            .build_transfer_with_inputs(
                &alice,
                bob.address(),
                TEST_BURN_AMOUNT,
                0,
                std::slice::from_ref(&mine_outpoint),
            )
            .unwrap_err();
        assert!(format!("{pending_error:#}").contains("not spendable"));

        let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&alice, 1).unwrap();
        assert!(
            block
                .transactions
                .iter()
                .any(|tx| tx.signature() == mine.signature())
        );
        ledger.apply_locally_mined_block(block).unwrap();

        assert!(
            ledger
                .available_utxos_for_address(alice.address())
                .unwrap()
                .iter()
                .any(|(outpoint, _)| outpoint == &mine_outpoint)
        );
        ledger
            .build_transfer_with_inputs(
                &alice,
                bob.address(),
                TEST_BURN_AMOUNT,
                0,
                std::slice::from_ref(&mine_outpoint),
            )
            .unwrap();
    }

    #[test]
    fn burns_built_after_pending_mine_do_not_spend_pending_mine_output() {
        let alice = Wallet::from_seed("pending-mine-burn-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

        let mine = ledger.build_mine(alice.address()).unwrap();
        let mine_outpoint = OutPoint {
            txid: mine.signature().to_string(),
            index: 0,
        };
        ledger.submit_transaction(mine).unwrap();

        let burn = ledger.build_burn(&alice, TEST_BURN_AMOUNT, 0).unwrap();

        let Transaction::Burn { inputs, .. } = &burn else {
            panic!("expected burn transaction");
        };
        assert!(!inputs.iter().any(|input| input.outpoint == mine_outpoint));
    }

    #[test]
    fn block_selection_can_include_multiple_mine_actions() {
        let alice = Wallet::from_seed("mine-multiple-actions-alice");
        let mut ledger = ledger_with_allocation(&alice, 10 * MICRO_IUNA);

        let burn = ledger.build_burn(&alice, MICRO_IUNA, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let first_mine = ledger.build_mine(alice.address()).unwrap();
        ledger.submit_transaction(first_mine.clone()).unwrap();
        let second_mine = ledger.build_mine(alice.address()).unwrap();
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
            .stratum_mine_template(alice.address(), anchor, 1, difficulty_bits)
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
                .stratum_mine_template(alice.address(), anchor.clone(), salt, difficulty_bits)
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
