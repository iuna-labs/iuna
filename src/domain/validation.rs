use anyhow::{Context, Result, bail};

use super::{
    HASH_BYTES, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, Transaction, TxInput, TxOutput, decode_hex,
    decode_hex_array, stratum::STRATUM_MINE_HEADER_BYTES,
};

pub fn validate_address(address: &str, label: &str) -> Result<()> {
    decode_hex_array::<PUBLIC_KEY_BYTES>(address)
        .with_context(|| format!("invalid {label} address"))?;
    Ok(())
}

pub(super) fn validate_hash(hash: &str, label: &str) -> Result<()> {
    decode_hex_array::<HASH_BYTES>(hash).with_context(|| format!("invalid {label}"))?;
    Ok(())
}

pub(super) fn validate_signature(signature: &str, label: &str) -> Result<()> {
    decode_hex_array::<SIGNATURE_BYTES>(signature).with_context(|| format!("invalid {label}"))?;
    Ok(())
}

pub(super) fn validate_stratum_header(header: &str) -> Result<()> {
    decode_hex_array::<STRATUM_MINE_HEADER_BYTES>(header)
        .context("invalid mine transaction proof header")?;
    Ok(())
}

pub(super) fn validate_protocol_id(value: &str, label: &str) -> Result<()> {
    let bytes = decode_hex(value).with_context(|| format!("invalid {label}"))?;
    match bytes.len() {
        HASH_BYTES | SIGNATURE_BYTES => Ok(()),
        length => bail!("invalid {label}: expected 32 or 64 bytes, got {length}"),
    }
}

pub(super) fn canonical_transaction_size_bytes(transaction: &Transaction) -> usize {
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

pub(super) fn compact_len(mut value: u128) -> usize {
    let mut bytes = 1;
    while value >= 0x80 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_transaction_size_bytes, compact_len, validate_address, validate_hash,
        validate_protocol_id, validate_signature, validate_stratum_header,
    };
    use crate::domain::{OutPoint, Transaction, TxInput, TxOutput};

    #[test]
    fn validators_accept_expected_protocol_lengths() {
        assert!(validate_address(&"0".repeat(64), "test").is_ok());
        assert!(validate_hash(&"0".repeat(64), "test").is_ok());
        assert!(validate_signature(&"0".repeat(128), "test").is_ok());
        assert!(validate_stratum_header(&"0".repeat(160)).is_ok());
        assert!(validate_protocol_id(&"0".repeat(64), "test").is_ok());
        assert!(validate_protocol_id(&"0".repeat(128), "test").is_ok());
    }

    #[test]
    fn validators_reject_wrong_lengths_and_bad_hex() {
        assert!(validate_address(&"0".repeat(62), "test").is_err());
        assert!(validate_hash(&"0".repeat(66), "test").is_err());
        assert!(validate_signature("zz", "test").is_err());
        assert!(validate_stratum_header(&"0".repeat(158)).is_err());
        assert!(validate_protocol_id(&"0".repeat(96), "test").is_err());
    }

    #[test]
    fn compact_len_uses_base_128_varint_width() {
        assert_eq!(compact_len(0), 1);
        assert_eq!(compact_len(127), 1);
        assert_eq!(compact_len(128), 2);
        assert_eq!(compact_len(16_383), 2);
        assert_eq!(compact_len(16_384), 3);
    }

    #[test]
    fn transaction_economic_size_uses_binary_ids_and_compact_integers() {
        let input = TxInput {
            outpoint: OutPoint {
                txid: "a".repeat(64),
                index: 128,
            },
            owner: "b".repeat(64),
            signature: "c".repeat(128),
        };
        let output = TxOutput {
            address: "d".repeat(64),
            amount: 128,
        };
        let tx = Transaction::Transfer {
            inputs: vec![input],
            outputs: vec![output],
            fee: 16_384,
            signature: "e".repeat(128),
        };

        assert_eq!(
            canonical_transaction_size_bytes(&tx),
            1  // transaction kind
                + 1 // input count
                + 32 // txid, counted as bytes rather than hex chars
                + 2 // output index varint
                + 32 // owner public key bytes
                + 1 // output count
                + 32 // recipient public key bytes
                + 2 // amount varint
                + 3 // fee varint
                + 64 // signature bytes
        );
        assert!(tx.serialized_size_bytes().unwrap() > canonical_transaction_size_bytes(&tx));
    }
}
