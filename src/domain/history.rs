use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::blinded::{
    ActiveBlindedTransaction, blinded_envelope_fee_for_transaction, decrypt_blinded_transaction,
};
use super::{ChainSnapshot, RevealedBlindedTransaction, Transaction};

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
            if matches!(transaction, Transaction::Mine { .. }) {
                bail!("block {} blinded reveal is a mine action", block.height);
            }
            if blinded_envelope_fee_for_transaction(&transaction)
                != active_transaction.transaction.fee
            {
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
                    locked_outputs: Vec::new(),
                    included_height: block.height,
                    included_by: block.miner.clone(),
                },
            );
        }
    }
    Ok(revealed)
}
