use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::validation::{decode_canonical_hex, decode_canonical_hex_array};
use super::{
    Amount, HASH_BYTES, MINE_FINALIZER_FEE, MINE_REWARD, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, Wallet,
    canonical_transaction_size_bytes, genesis_allocation_outpoint, hash_meets_difficulty,
    hex_encode, hex_hash, mine_payload, mine_signature, stratum_mine_header_bytes,
    stratum_mine_signature,
};

pub const TRANSACTION_SIGNING_FORMAT_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TransactionSigningDomain {
    chain_id: String,
    genesis_hash: String,
}

impl TransactionSigningDomain {
    pub(super) fn new(chain_id: impl Into<String>, genesis_hash: impl Into<String>) -> Self {
        Self {
            chain_id: chain_id.into(),
            genesis_hash: genesis_hash.into(),
        }
    }

    pub(super) fn encode(&self, bytes: &mut Vec<u8>) -> Result<()> {
        bytes.extend_from_slice(b"IUNA-TX");
        bytes.extend_from_slice(&TRANSACTION_SIGNING_FORMAT_VERSION.to_be_bytes());
        encode_bytes(bytes, self.chain_id.as_bytes(), "chain ID")?;
        let genesis_hash = decode_canonical_hex_array::<HASH_BYTES>(&self.genesis_hash)
            .context("transaction signing genesis hash is invalid")?;
        encode_bytes(bytes, &genesis_hash, "genesis hash")
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MineSearchOutcome {
    pub transaction: Option<Transaction>,
    pub next_nonce: u64,
    pub attempts: u64,
}

impl Transaction {
    pub fn genesis_burn(from: impl Into<String>, amount: Amount) -> Self {
        let from = from.into();
        Self::genesis_burn_with_change(
            from,
            amount,
            Vec::new(),
            &super::LaunchProfile::default().profile_id,
        )
    }

    pub(super) fn genesis_burn_with_allocation(
        from: impl Into<String>,
        amount: Amount,
        allocation: Amount,
        chain_id: &str,
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
        Ok(Self::genesis_burn_with_change(
            from, amount, change, chain_id,
        ))
    }

    fn genesis_burn_with_change(
        from: String,
        amount: Amount,
        change: Vec<TxOutput>,
        chain_id: &str,
    ) -> Self {
        let input = TxInput {
            outpoint: genesis_allocation_outpoint(chain_id, &from),
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

    pub(super) fn verify_signature(&self, domain: &TransactionSigningDomain) -> Result<()> {
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
                let header = stratum_mine_header_bytes(
                    domain,
                    recipient,
                    anchor,
                    *salt,
                    *nonce,
                    *difficulty_bits,
                )?;
                let expected_header = hex_encode(header);
                if *proof_header != expected_header {
                    bail!("mine transaction proof header is invalid");
                }
                stratum_mine_signature(&header)
            } else {
                mine_signature(domain, recipient, anchor, *salt, *nonce, *difficulty_bits)?
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
        let public_key = decode_canonical_hex_array::<PUBLIC_KEY_BYTES>(sender)
            .with_context(|| format!("invalid public key for {sender}"))?;
        let signature = decode_canonical_hex_array::<SIGNATURE_BYTES>(self.signature())
            .context("invalid signature hex")?;
        let verifying_key =
            VerifyingKey::from_bytes(&public_key).context("invalid transaction public key")?;
        let signature = Signature::from_bytes(&signature);
        verifying_key
            .verify(&self.signing_bytes(domain)?, &signature)
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

    fn signing_bytes(&self, domain: &TransactionSigningDomain) -> Result<Vec<u8>> {
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
            .signing_bytes(domain),
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
            .signing_bytes(domain),
            Self::Mine { .. } => unreachable!("mine transactions use proof hashes"),
        }
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
    pub(super) fn sign(
        self,
        wallet: &Wallet,
        domain: &TransactionSigningDomain,
    ) -> Result<Transaction> {
        let signature = wallet.sign_bytes(&self.signing_bytes(domain)?);
        let signed_inputs = self
            .inputs()
            .iter()
            .map(|input| TxInput {
                outpoint: input.outpoint.clone(),
                owner: input.owner.clone(),
                signature: signature.clone(),
            })
            .collect::<Vec<_>>();
        Ok(match self {
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
        })
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

    pub(super) fn signing_bytes(&self, domain: &TransactionSigningDomain) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        domain.encode(&mut bytes)?;
        match self {
            Self::Transfer {
                inputs,
                outputs,
                fee,
            } => {
                bytes.push(1);
                encode_inputs(&mut bytes, inputs)?;
                encode_outputs(&mut bytes, outputs)?;
                bytes.extend_from_slice(&fee.to_be_bytes());
            }
            Self::Burn {
                inputs,
                change,
                amount,
                fee,
            } => {
                bytes.push(2);
                encode_inputs(&mut bytes, inputs)?;
                encode_outputs(&mut bytes, change)?;
                bytes.extend_from_slice(&amount.to_be_bytes());
                bytes.extend_from_slice(&fee.to_be_bytes());
            }
        }
        Ok(bytes)
    }
}

fn encode_inputs(bytes: &mut Vec<u8>, inputs: &[UnsignedTxInput]) -> Result<()> {
    encode_len(bytes, inputs.len(), "input count")?;
    for input in inputs {
        let txid = decode_canonical_hex(&input.outpoint.txid)
            .context("transaction input txid is invalid")?;
        if txid.len() != HASH_BYTES && txid.len() != SIGNATURE_BYTES {
            bail!("transaction input txid must be a hash or signature");
        }
        encode_bytes(bytes, &txid, "input txid")?;
        bytes.extend_from_slice(&input.outpoint.index.to_be_bytes());
        let owner = decode_canonical_hex_array::<PUBLIC_KEY_BYTES>(&input.owner)
            .context("transaction input owner is invalid")?;
        encode_bytes(bytes, &owner, "input owner")?;
    }
    Ok(())
}

fn encode_outputs(bytes: &mut Vec<u8>, outputs: &[TxOutput]) -> Result<()> {
    encode_len(bytes, outputs.len(), "output count")?;
    for output in outputs {
        let address = decode_canonical_hex_array::<PUBLIC_KEY_BYTES>(&output.address)
            .context("transaction output address is invalid")?;
        encode_bytes(bytes, &address, "output address")?;
        bytes.extend_from_slice(&output.amount.to_be_bytes());
    }
    Ok(())
}

fn encode_bytes(bytes: &mut Vec<u8>, value: &[u8], label: &str) -> Result<()> {
    encode_len(bytes, value.len(), label)?;
    bytes.extend_from_slice(value);
    Ok(())
}

fn encode_len(bytes: &mut Vec<u8>, len: usize, label: &str) -> Result<()> {
    let len = u32::try_from(len).with_context(|| format!("{label} exceeds u32 length"))?;
    bytes.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

pub(super) fn mine_signing_bytes(
    domain: &TransactionSigningDomain,
    recipient: &str,
    anchor: &str,
    salt: u64,
    nonce: u64,
    difficulty_bits: u32,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    domain.encode(&mut bytes)?;
    bytes.push(3);
    let recipient = decode_canonical_hex_array::<PUBLIC_KEY_BYTES>(recipient)
        .context("mine recipient is invalid")?;
    encode_bytes(&mut bytes, &recipient, "mine recipient")?;
    let anchor =
        decode_canonical_hex_array::<HASH_BYTES>(anchor).context("mine anchor is invalid")?;
    encode_bytes(&mut bytes, &anchor, "mine anchor")?;
    bytes.extend_from_slice(&salt.to_be_bytes());
    bytes.extend_from_slice(&nonce.to_be_bytes());
    bytes.extend_from_slice(&difficulty_bits.to_be_bytes());
    Ok(bytes)
}

pub(super) fn unsigned_inputs(inputs: &[TxInput]) -> Vec<UnsignedTxInput> {
    inputs.iter().map(TxInput::without_signature).collect()
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

pub(super) fn transaction_inputs_available(
    transaction: &Transaction,
    utxos: &BTreeMap<OutPoint, TxOutput>,
) -> bool {
    transaction
        .inputs()
        .iter()
        .all(|input| utxos.contains_key(&input.outpoint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_format_v1_has_a_stable_typed_binary_vector() {
        let transaction = UnsignedUtxoTransaction::Transfer {
            inputs: vec![UnsignedTxInput {
                outpoint: OutPoint {
                    txid: "11".repeat(32),
                    index: 7,
                },
                owner: "33".repeat(32),
            }],
            outputs: vec![TxOutput {
                address: "44".repeat(32),
                amount: 5,
            }],
            fee: 1,
        };
        let domain = TransactionSigningDomain::new("iuna-test-vector", "22".repeat(32));

        let encoded = hex_encode(transaction.signing_bytes(&domain).unwrap());

        assert_eq!(
            encoded,
            concat!(
                "49554e412d5458", // IUNA-TX domain tag
                "0001",           // signing format version
                "00000010",
                "69756e612d746573742d766563746f72", // chain ID
                "00000020",
                "2222222222222222222222222222222222222222222222222222222222222222", // genesis hash
                "01",                                                               // transfer type
                "00000001",                                                         // input count
                "00000020",
                "1111111111111111111111111111111111111111111111111111111111111111", // txid
                "00000007",                                                         // output index
                "00000020",
                "3333333333333333333333333333333333333333333333333333333333333333", // owner
                "00000001",                                                         // output count
                "00000020",
                "4444444444444444444444444444444444444444444444444444444444444444", // address
                "0000000000000005",                                                 // amount
                "0000000000000001",                                                 // fee
            )
        );
    }

    #[test]
    fn legacy_text_signature_is_invalid_under_format_v1() {
        let wallet = Wallet::from_seed("legacy-transaction-signature");
        let unsigned = UnsignedUtxoTransaction::Transfer {
            inputs: vec![UnsignedTxInput {
                outpoint: OutPoint {
                    txid: "11".repeat(32),
                    index: 0,
                },
                owner: wallet.address().to_string(),
            }],
            outputs: vec![TxOutput {
                address: wallet.address().to_string(),
                amount: 9,
            }],
            fee: 1,
        };
        let legacy_signature = wallet.sign_payload(&unsigned.canonical());
        let transaction = Transaction::Transfer {
            inputs: vec![TxInput {
                outpoint: OutPoint {
                    txid: "11".repeat(32),
                    index: 0,
                },
                owner: wallet.address().to_string(),
                signature: legacy_signature.clone(),
            }],
            outputs: vec![TxOutput {
                address: wallet.address().to_string(),
                amount: 9,
            }],
            fee: 1,
            signature: legacy_signature,
        };
        let domain = TransactionSigningDomain::new("iuna-mainnet-v1", "22".repeat(32));

        assert!(transaction.verify_signature(&domain).is_err());
    }
}
