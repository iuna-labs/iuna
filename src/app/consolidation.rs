use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{NodeCore, helpers::converge_fee_by_byte};
use crate::domain::{Amount, Ledger, OutPoint, Transaction};

const BATCH_INPUTS: usize = 128;
const MAX_BATCHES: usize = 32;

#[derive(Serialize)]
pub(crate) struct ConsolidationBatch {
    pub utxos: Vec<OutPoint>,
    pub fee: Amount,
    pub amount: Amount,
    pub bytes: usize,
}

#[derive(Serialize)]
pub(crate) struct ConsolidationPlan {
    pub address: String,
    pub before: usize,
    pub after: usize,
    pub fee: Amount,
    pub batches: Vec<ConsolidationBatch>,
}

impl NodeCore {
    pub(crate) fn consolidation_plan(
        &self,
        fee_per_byte: Amount,
        merge_roots: bool,
    ) -> Result<ConsolidationPlan> {
        self.wallet.unlocked()?;
        let ledger = self.wallet_build_ledger()?;
        let confirmed = ledger.utxos_for_address(self.wallet.address());
        let before = confirmed.len();
        let available: BTreeSet<_> = ledger
            .available_utxos_for_address(self.wallet.address())?
            .into_iter()
            .map(|(point, _)| point)
            .collect();
        let mut candidates: Vec<_> = confirmed
            .into_iter()
            .filter(|(point, _)| available.contains(point))
            .collect();
        candidates.sort_by_key(|(point, output)| (output.amount, point.clone()));
        // Leave the largest spendable output for payments and automatic burns.
        candidates.pop();
        let mut groups = BTreeMap::<Option<OutPoint>, Vec<OutPoint>>::new();
        for (point, _) in candidates {
            let root = if merge_roots {
                None
            } else {
                ledger.consolidation_root(&point)
            };
            groups.entry(root).or_default().push(point);
        }
        let mut plan = ConsolidationPlan {
            address: self.wallet.address().to_string(),
            before,
            after: before,
            fee: 0,
            batches: Vec::new(),
        };
        for group in groups.values() {
            let mut offset = 0;
            while offset + 1 < group.len() && plan.batches.len() < MAX_BATCHES {
                let mut count = BATCH_INPUTS.min(group.len() - offset);
                // The actual builder checks the active network's block-size limit.
                let built = loop {
                    if count < 2 {
                        break None;
                    }
                    match self.build_consolidation(
                        &ledger,
                        &group[offset..offset + count],
                        fee_per_byte,
                        merge_roots,
                    ) {
                        Ok(value) => break Some(value),
                        Err(_) => count /= 2,
                    }
                };
                if let Some((_, batch)) = built {
                    plan.after -= batch.utxos.len() - 1;
                    plan.fee = plan
                        .fee
                        .checked_add(batch.fee)
                        .context("total fee overflows")?;
                    offset += batch.utxos.len();
                    plan.batches.push(batch);
                } else {
                    offset += 1;
                }
            }
        }
        Ok(plan)
    }

    fn build_consolidation(
        &self,
        ledger: &Ledger,
        outpoints: &[OutPoint],
        fee_per_byte: Amount,
        merge_roots: bool,
    ) -> Result<(Transaction, ConsolidationBatch)> {
        if !(2..=BATCH_INPUTS).contains(&outpoints.len()) {
            bail!("choose between 2 and 128 outputs per batch");
        }
        let confirmed: BTreeMap<_, _> = ledger
            .utxos_for_address(self.wallet.address())
            .into_iter()
            .collect();
        let available: BTreeSet<_> = ledger
            .available_utxos_for_address(self.wallet.address())?
            .into_iter()
            .map(|(point, _)| point)
            .collect();
        let mut seen = BTreeSet::new();
        let root = ledger.consolidation_root(&outpoints[0]);
        let mut total: Amount = 0;
        for point in outpoints {
            if !seen.insert(point) || !available.contains(point) {
                bail!("outputs changed or are reserved; review a new preview");
            }
            if !merge_roots && ledger.consolidation_root(point) != root {
                bail!("merging mining groups requires explicit consent");
            }
            let output = confirmed
                .get(point)
                .context("output is no longer confirmed in this wallet")?;
            total = total
                .checked_add(output.amount)
                .context("input total overflows")?;
        }
        let (transaction, estimate) = converge_fee_by_byte(fee_per_byte, |fee| {
            let amount = total
                .checked_sub(fee)
                .filter(|amount| *amount > 0)
                .context("outputs do not cover the network fee")?;
            ledger.build_transfer_with_inputs(
                self.wallet.unlocked()?,
                self.wallet.address(),
                amount,
                fee,
                outpoints,
            )
        })?;
        // Never recommend or accept batches spending over 1% of their value on fees.
        if u128::from(estimate.fee) * 100 > u128::from(total) {
            bail!("batch fee exceeds 1% of its value; use a lower fee or wait");
        }
        let batch = ConsolidationBatch {
            utxos: outpoints.to_vec(),
            fee: estimate.fee,
            amount: total - estimate.fee,
            bytes: estimate.bytes,
        };
        Ok((transaction, batch))
    }

    pub(crate) fn consolidate(
        &mut self,
        outpoints: &[OutPoint],
        fee_per_byte: Amount,
        max_fee: Amount,
        merge_roots: bool,
        address: &str,
    ) -> Result<Transaction> {
        if address != self.wallet.address() {
            bail!("wallet changed; review a new preview");
        }
        let ledger = self.wallet_build_ledger()?;
        let (transaction, batch) =
            self.build_consolidation(&ledger, outpoints, fee_per_byte, merge_roots)?;
        if batch.fee > max_fee {
            bail!("fee exceeds the approved limit; review a new preview");
        }
        self.submit_public_transaction(transaction)
    }
}
