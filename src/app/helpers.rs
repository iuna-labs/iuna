use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::domain::{Amount, BuiltBlindedTransaction, MINE_REWARD, OutPoint, Transaction};

use super::FeeEstimate;

pub(super) fn auto_pow_salt(wallet_address: &str, anchor: &str) -> u64 {
    let digest = Sha256::digest(format!("iuna-auto-pow:{wallet_address}:{anchor}").as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

pub(super) fn converge_fee_by_byte(
    fee_per_byte: Amount,
    mut build: impl FnMut(Amount) -> Result<BuiltBlindedTransaction>,
) -> Result<(BuiltBlindedTransaction, FeeEstimate)> {
    let mut fee = if fee_per_byte == 0 { 0 } else { 1 };
    let mut best = None;
    for _ in 0..64 {
        let built = build(fee)?;
        let bytes = built.transaction.fee_rate_size_bytes();
        let required_fee = fee_per_byte
            .checked_mul(bytes as Amount)
            .context("fee per byte times blinded transaction bytes overflows")?;
        if fee == required_fee {
            return Ok((built, FeeEstimate { bytes, fee }));
        }
        if fee > required_fee
            && best
                .as_ref()
                .is_none_or(|(_, estimate): &(BuiltBlindedTransaction, FeeEstimate)| {
                    fee < estimate.fee
                })
        {
            best = Some((built, FeeEstimate { bytes, fee }));
        }
        fee = required_fee;
    }

    let built = build(fee)?;
    let bytes = built.transaction.fee_rate_size_bytes();
    let required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times blinded transaction bytes overflows")?;
    if fee >= required_fee {
        if best
            .as_ref()
            .is_none_or(|(_, estimate): &(BuiltBlindedTransaction, FeeEstimate)| fee < estimate.fee)
        {
            best = Some((built, FeeEstimate { bytes, fee }));
        }
        if let Some(best) = best {
            return Ok(best);
        }
    }
    let built = build(required_fee)?;
    let bytes = built.transaction.fee_rate_size_bytes();
    let final_required_fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times blinded transaction bytes overflows")?;
    if required_fee < final_required_fee {
        bail!("fee per byte did not converge");
    }
    Ok((
        built,
        FeeEstimate {
            bytes,
            fee: required_fee,
        },
    ))
}

pub(super) fn transaction_output_total_for_address(
    transaction: &Transaction,
    address: &str,
) -> Amount {
    match transaction {
        Transaction::Transfer { outputs, .. } => outputs,
        Transaction::Burn { change, .. } => change,
        Transaction::Mine { recipient, .. } if recipient == address => return MINE_REWARD,
        Transaction::Mine { .. } => return 0,
    }
    .iter()
    .filter(|output| output.address == address)
    .fold(0_u64, |total, output| total.saturating_add(output.amount))
}

pub(super) fn transaction_input_total_from_outputs(
    transaction: &Transaction,
    address: &str,
    outputs: &BTreeMap<OutPoint, Amount>,
) -> Amount {
    let inputs = match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => return 0,
    };
    inputs
        .iter()
        .filter(|input| input.owner == address)
        .filter_map(|input| outputs.get(&input.outpoint))
        .fold(0_u64, |total, amount| total.saturating_add(*amount))
}

pub(super) fn transaction_input_outpoints(transaction: &Transaction) -> BTreeSet<OutPoint> {
    match transaction {
        Transaction::Transfer { inputs, .. } | Transaction::Burn { inputs, .. } => inputs,
        Transaction::Mine { .. } => return BTreeSet::new(),
    }
    .iter()
    .map(|input| input.outpoint.clone())
    .collect()
}

pub(super) fn allowed_recovery_vdf_rank_count(rank_count: usize, percent: u8) -> usize {
    if rank_count == 0 || percent == 0 {
        return 0;
    }
    rank_count
        .saturating_mul(usize::from(percent.min(100)))
        .saturating_add(99)
        / 100
}

pub(super) fn recovery_vdf_sample_percent(address: &str, tip_hash: &str) -> u8 {
    let digest = Sha256::digest(format!("iuna-recovery-vdf-sample:{tip_hash}:{address}"));
    digest[0] % 100
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use crate::domain::{BlindedReveal, BlindedTransaction, BuiltBlindedTransaction, Transaction};

    use super::converge_fee_by_byte;

    fn built_with_fee(fee: u64) -> BuiltBlindedTransaction {
        BuiltBlindedTransaction {
            payload: Transaction::Burn {
                inputs: Vec::new(),
                change: Vec::new(),
                amount: 1,
                fee,
                signature: "payload".to_string(),
            },
            transaction: BlindedTransaction {
                commitment: "0".repeat(64),
                inputs: Vec::new(),
                fee,
                encrypted_size: 1,
                expires_at_height: 1,
                nonce: "0".repeat(24),
                ciphertext: "00".to_string(),
                payload_hash: "1".repeat(64),
            },
            reveal: BlindedReveal {
                commitment: "0".repeat(64),
                key: "2".repeat(64),
            },
        }
    }

    #[test]
    fn fee_convergence_does_not_probe_zero_when_fee_rate_is_positive() {
        let (built, estimate) = converge_fee_by_byte(1, |fee| {
            if fee == 0 {
                bail!("zero fee rejected after activation");
            }
            Ok(built_with_fee(fee))
        })
        .expect("positive fee-rate convergence should skip invalid zero fee");

        assert!(estimate.fee > 0);
        assert_eq!(built.transaction.fee, estimate.fee);
    }

    #[test]
    fn fee_convergence_allows_zero_when_fee_rate_is_zero() {
        let (built, estimate) =
            converge_fee_by_byte(0, |fee| Ok(built_with_fee(fee))).expect("zero fee rate works");

        assert_eq!(estimate.fee, 0);
        assert_eq!(built.transaction.fee, 0);
    }
}
