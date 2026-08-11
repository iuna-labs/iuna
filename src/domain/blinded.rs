use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::transaction::{
    BlindedTransactionPayload, canonical_inputs, signed_blinded_inputs, unsigned_inputs,
};
use super::{
    Amount, BLINDED_COMMITTER_FEE_BPS, BLINDED_FEE_BPS_DENOMINATOR, BLINDED_KEY_BYTES,
    BLINDED_NONCE_BYTES, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS, BlindedReveal, BlindedTransaction,
    OutPoint, PUBLIC_KEY_BYTES, RevealBundleSignature, SIGNATURE_BYTES, Transaction, TxInput,
    TxOutput, blinded_reveal_finalizer_fee, decode_hex, decode_hex_array,
    ensure_outputs_do_not_overflow, ensure_single_input_owner_for_inputs, hex_hash,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActiveBlindedTransaction {
    pub(super) transaction: BlindedTransaction,
    pub(super) locked_outputs: Vec<TxOutput>,
    pub(super) included_height: u64,
    pub(super) included_by: String,
}

pub(super) fn decrypt_blinded_transaction(
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
        &signed_blinded_inputs(&unsigned_inputs(&transaction.inputs), ""),
        transaction.fee,
        transaction.expires_at_height,
        &ciphertext,
    )?;
    if hex_hash(&plaintext) != transaction.payload_hash {
        bail!("blinded transaction payload hash is invalid");
    }
    let payload = serde_json::from_slice(&plaintext)
        .context("failed to decode blinded transaction payload")?;
    transaction_from_blinded_payload(payload, &transaction.inputs, transaction.fee)
}

pub(super) fn blinded_payload_from_transaction(
    transaction: &Transaction,
) -> Result<BlindedTransactionPayload> {
    match transaction {
        Transaction::Transfer {
            outputs, signature, ..
        } => Ok(BlindedTransactionPayload::Transfer {
            outputs: outputs.clone(),
            signature: signature.clone(),
        }),
        Transaction::Burn {
            change,
            amount,
            signature,
            ..
        } => Ok(BlindedTransactionPayload::Burn {
            change: change.clone(),
            amount: *amount,
            signature: signature.clone(),
        }),
        Transaction::Mine { .. } => bail!("mine actions are public and cannot be blinded"),
    }
}

pub(super) fn transaction_from_blinded_payload(
    payload: BlindedTransactionPayload,
    envelope_inputs: &[TxInput],
    fee: Amount,
) -> Result<Transaction> {
    match payload {
        BlindedTransactionPayload::Transfer { outputs, signature } => {
            let inputs = signed_blinded_inputs(&unsigned_inputs(envelope_inputs), &signature);
            Ok(Transaction::Transfer {
                inputs,
                outputs,
                fee,
                signature,
            })
        }
        BlindedTransactionPayload::Burn {
            change,
            amount,
            signature,
        } => {
            let inputs = signed_blinded_inputs(&unsigned_inputs(envelope_inputs), &signature);
            Ok(Transaction::Burn {
                inputs,
                change,
                amount,
                fee,
                signature,
            })
        }
    }
}

pub(super) fn encrypt_blinded_payload(
    key: &[u8; BLINDED_KEY_BYTES],
    nonce: &[u8; BLINDED_NONCE_BYTES],
    inputs: &[TxInput],
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
                aad: blinded_payload_aad(inputs, fee, expires_at_height).as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to encrypt blinded transaction payload"))
}

pub(super) fn decrypt_blinded_payload(
    key: &[u8; BLINDED_KEY_BYTES],
    nonce: &[u8; BLINDED_NONCE_BYTES],
    inputs: &[TxInput],
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
                aad: blinded_payload_aad(inputs, fee, expires_at_height).as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to decrypt blinded transaction payload"))
}

fn blinded_payload_aad(inputs: &[TxInput], fee: Amount, expires_at_height: u64) -> String {
    format!(
        "iuna-blinded-payload-v3:{}:{fee}:{expires_at_height}",
        canonical_inputs(&unsigned_inputs(inputs))
    )
}

pub(super) fn blinded_transaction_commitment(transaction: &BlindedTransaction) -> Result<String> {
    let mut without_commitment = transaction.clone();
    without_commitment.commitment.clear();
    Ok(hex_hash(without_commitment.canonical()))
}

pub(super) fn blinded_transaction_signing_payload(transaction: &BlindedTransaction) -> String {
    format!(
        "blinded-tx-inputs:{}:{}:{}:{}:{}:{}:{}",
        canonical_inputs(&unsigned_inputs(&transaction.inputs)),
        transaction.fee,
        transaction.encrypted_size,
        transaction.expires_at_height,
        transaction.nonce,
        transaction.ciphertext,
        transaction.payload_hash
    )
}

pub(super) fn verify_blinded_input_signatures(transaction: &BlindedTransaction) -> Result<()> {
    if transaction.inputs.is_empty() {
        return Ok(());
    }
    ensure_single_input_owner_for_inputs(&transaction.inputs)?;
    let signature = transaction.inputs[0].signature.clone();
    if !transaction
        .inputs
        .iter()
        .all(|input| input.signature == signature)
    {
        bail!("blinded transaction input signature mismatch");
    }
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.inputs {
        input.signature.clear();
    }
    let public_key = decode_hex_array::<PUBLIC_KEY_BYTES>(&transaction.inputs[0].owner)
        .context("invalid blinded transaction input owner")?;
    let signature = decode_hex_array::<SIGNATURE_BYTES>(&signature)
        .context("invalid blinded input signature")?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("invalid blinded input public key")?;
    let signature = Signature::from_bytes(&signature);
    verifying_key
        .verify(
            blinded_transaction_signing_payload(&unsigned).as_bytes(),
            &signature,
        )
        .context("blinded transaction input signature is invalid")
}

pub(super) fn credit_blinded_fee_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    active: &ActiveBlindedTransaction,
    reveal_executor: &str,
    transaction: &Transaction,
    reveal_bundle_signatures: &[RevealBundleSignature],
    available_bundle_slots: usize,
    aggregate_finalizer_fee: bool,
) -> Result<()> {
    let fee = transaction.fee();
    if fee == 0 {
        return Ok(());
    }
    let committer_fee = blinded_fee_share(fee, BLINDED_COMMITTER_FEE_BPS);
    let reveal_finalizer_fee =
        blinded_reveal_finalizer_fee(fee, reveal_bundle_signatures.len(), available_bundle_slots);
    let reveal_bundle_signer_fee = blinded_fee_share(fee, BLINDED_REVEAL_BUNDLE_SIGNER_FEE_BPS);
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
    if reveal_finalizer_fee > 0 && !aggregate_finalizer_fee {
        outputs.push((
            blinded_executor_fee_outpoint(&active.transaction.commitment),
            TxOutput {
                address: reveal_executor.to_string(),
                amount: reveal_finalizer_fee,
            },
        ));
    }
    for signature in reveal_bundle_signatures {
        if reveal_bundle_signer_fee > 0 {
            outputs.push((
                blinded_reveal_bundle_signer_fee_outpoint(
                    &active.transaction.commitment,
                    signature.slot,
                ),
                TxOutput {
                    address: signature.member.clone(),
                    amount: reveal_bundle_signer_fee,
                },
            ));
        }
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

pub(super) fn blinded_fee_share(fee: Amount, bps: u64) -> Amount {
    ((fee as u128 * bps as u128) / BLINDED_FEE_BPS_DENOMINATOR as u128) as Amount
}

pub(super) fn blinded_envelope_fee_for_transaction(transaction: &Transaction) -> Amount {
    match transaction {
        Transaction::Mine { .. } => 0,
        Transaction::Transfer { .. } | Transaction::Burn { .. } => transaction.fee(),
    }
}

pub(super) fn blinded_locked_output_total(active: &ActiveBlindedTransaction) -> Result<Amount> {
    active
        .locked_outputs
        .iter()
        .try_fold(0_u64, |total, output| {
            total
                .checked_add(output.amount)
                .context("blinded transaction locked input total overflows")
        })
}

pub(super) fn blinded_reveal_inputs_match(
    active: &ActiveBlindedTransaction,
    transaction: &Transaction,
) -> bool {
    let visible = active
        .transaction
        .inputs
        .iter()
        .map(TxInput::without_signature)
        .collect::<Vec<_>>();
    let revealed = transaction
        .inputs()
        .iter()
        .map(TxInput::without_signature)
        .collect::<Vec<_>>();
    visible == revealed
}

pub(super) fn credit_expired_blinded_outputs(
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    active: &ActiveBlindedTransaction,
) -> Result<()> {
    let Some(first_input) = active.transaction.inputs.first() else {
        return Ok(());
    };
    let input_total = blinded_locked_output_total(active)?;
    if active.transaction.fee > input_total {
        bail!("blinded transaction fee exceeds locked inputs");
    }
    let change = input_total - active.transaction.fee;
    let mut outputs = Vec::new();
    if change > 0 {
        outputs.push((
            blinded_expiry_change_outpoint(&active.transaction.commitment),
            TxOutput {
                address: first_input.owner.clone(),
                amount: change,
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

pub(super) fn blinded_committer_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 1,
    }
}

pub(super) fn blinded_executor_fee_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 2,
    }
}

pub(super) fn blinded_reveal_bundle_signer_fee_outpoint(commitment: &str, slot: u8) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: u32::MAX - 3 - u32::from(slot),
    }
}

pub(super) fn blinded_expiry_change_outpoint(commitment: &str) -> OutPoint {
    OutPoint {
        txid: commitment.to_string(),
        index: 0,
    }
}
