use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{
    Amount, MINE_FINALIZER_FEE, MINE_REWARD, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, Wallet,
    canonical_transaction_size_bytes, decode_hex_array, genesis_allocation_outpoint,
    hash_meets_difficulty, hex_encode, hex_hash, mine_payload, mine_signature,
    stratum_mine_header_bytes, stratum_mine_signature,
};

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
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum BlindedTransactionPayload {
    Transfer {
        outputs: Vec<TxOutput>,
        signature: String,
    },
    Burn {
        change: Vec<TxOutput>,
        amount: Amount,
        signature: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlindedTransaction {
    pub commitment: String,
    #[serde(default)]
    pub inputs: Vec<TxInput>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealedBlindedTransaction {
    pub height: u64,
    pub commitment: String,
    pub included_by: String,
    pub transaction: Transaction,
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

    pub(super) fn genesis_burn_with_allocation(
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

    pub(super) fn verify_signature(&self) -> Result<()> {
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

    pub(super) fn inputs(&self) -> &[TxInput] {
        match self {
            Self::Transfer { inputs, .. } | Self::Burn { inputs, .. } => inputs,
            Self::Mine { .. } => &[],
        }
    }

    pub(super) fn outputs(&self) -> Vec<TxOutput> {
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
            "blinded-tx:{}:{}:{}:{}:{}:{}:{}",
            canonical_signed_inputs(&self.inputs),
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

impl TxInput {
    pub(super) fn without_signature(&self) -> UnsignedTxInput {
        UnsignedTxInput {
            outpoint: self.outpoint.clone(),
            owner: self.owner.clone(),
        }
    }
}

impl OutPoint {
    pub(super) fn id(&self) -> String {
        format!("{}:{}", self.txid, self.index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UnsignedTxInput {
    pub(super) outpoint: OutPoint,
    pub(super) owner: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum UnsignedUtxoTransaction {
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
    pub(super) fn sign(self, wallet: &Wallet) -> Transaction {
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

    pub(super) fn canonical(&self) -> String {
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

pub(super) fn unsigned_inputs(inputs: &[TxInput]) -> Vec<UnsignedTxInput> {
    inputs.iter().map(TxInput::without_signature).collect()
}

pub(super) fn signed_blinded_inputs(inputs: &[UnsignedTxInput], signature: &str) -> Vec<TxInput> {
    inputs
        .iter()
        .map(|input| TxInput {
            outpoint: input.outpoint.clone(),
            owner: input.owner.clone(),
            signature: signature.to_string(),
        })
        .collect()
}

pub(super) fn canonical_inputs(inputs: &[UnsignedTxInput]) -> String {
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

pub(super) fn canonical_signed_inputs(inputs: &[TxInput]) -> String {
    inputs
        .iter()
        .map(|input| {
            format!(
                "{}:{}:{}:{}",
                input.outpoint.txid, input.outpoint.index, input.owner, input.signature
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

pub(super) fn transaction_inputs_spent_by(
    transaction: &Transaction,
    pending: &[Transaction],
) -> bool {
    let spent = pending_spent_outpoints(pending);
    transaction
        .inputs()
        .iter()
        .any(|input| spent.contains(&input.outpoint))
}

pub(super) fn transaction_inputs_spent_by_inputs(
    inputs: &[TxInput],
    pending: &[Transaction],
) -> bool {
    let spent = pending_spent_outpoints(pending);
    inputs.iter().any(|input| spent.contains(&input.outpoint))
}

pub(super) fn blinded_transaction_inputs_spent_by(
    transaction: &BlindedTransaction,
    pending: &[BlindedTransaction],
) -> bool {
    let spent = pending
        .iter()
        .flat_map(|transaction| {
            transaction
                .inputs
                .iter()
                .map(|input| input.outpoint.clone())
        })
        .collect::<BTreeSet<_>>();
    transaction
        .inputs
        .iter()
        .any(|input| spent.contains(&input.outpoint))
}

pub(super) fn transaction_inputs_available(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs()
        .iter()
        .all(|input| utxos.contains_key(&input.outpoint))
}

pub(super) fn blinded_transaction_inputs_available(
    transaction: &BlindedTransaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs
        .iter()
        .all(|input| utxos.contains_key(&input.outpoint))
}
