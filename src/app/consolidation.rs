use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{NodeCore, helpers::converge_fee_by_byte};
use crate::domain::{
    Amount, Ledger, OutPoint, Transaction, TransactionV2, TxOutput, hex_encode,
    minimum_transfer_economic_size_bytes,
};

const BATCH_INPUTS: usize = 128;
const MAX_BATCHES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsolidationKind {
    Legacy,
    Hybrid,
}

#[derive(Serialize)]
pub(crate) struct ConsolidationBatch {
    pub kind: ConsolidationKind,
    pub utxos: Vec<OutPoint>,
    pub fee: Amount,
    pub amount: Amount,
    pub bytes: usize,
}

enum BuiltConsolidation {
    Legacy(Transaction),
    Hybrid(TransactionV2),
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
        let wallet = self.wallet.unlocked()?;
        let ledger = self.wallet_build_ledger()?;
        let mut confirmed = Vec::new();
        let mut available = BTreeSet::new();
        collect_consolidation_outputs(
            &ledger,
            self.wallet.address(),
            ConsolidationKind::Legacy,
            &mut confirmed,
            &mut available,
        )?;
        for address in ledger.wallet_owned_hybrid_encoded_addresses(wallet)? {
            collect_consolidation_outputs(
                &ledger,
                &address,
                ConsolidationKind::Hybrid,
                &mut confirmed,
                &mut available,
            )?;
        }
        let pending_spent = self.wallet_pending_spent_outpoints();
        available.retain(|point| !pending_spent.contains(point));
        let before = confirmed.len();
        let mut candidates = confirmed
            .into_iter()
            .filter(|(_, point, _)| available.contains(point))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(kind, point, output)| (*kind, output.amount, point.clone()));
        // Keep the largest output of each protocol available for payments and automatic burns.
        for kind in [ConsolidationKind::Legacy, ConsolidationKind::Hybrid] {
            if let Some(index) = candidates
                .iter()
                .rposition(|(candidate, _, _)| *candidate == kind)
            {
                candidates.remove(index);
            }
        }
        let mut groups =
            BTreeMap::<(ConsolidationKind, Option<OutPoint>), Vec<(OutPoint, Amount)>>::new();
        for (kind, point, output) in candidates {
            let root = if merge_roots {
                None
            } else {
                ledger.consolidation_root(&point)
            };
            groups
                .entry((kind, root))
                .or_default()
                .push((point, output.amount));
        }
        let mut plan = ConsolidationPlan {
            address: self.wallet.address().to_string(),
            before,
            after: before,
            fee: 0,
            batches: Vec::new(),
        };
        for ((kind, _), group) in &groups {
            let mut offset = 0;
            while offset + 1 < group.len() && plan.batches.len() < MAX_BATCHES {
                let mut count = BATCH_INPUTS.min(group.len() - offset);
                // The actual builder checks the active network's block-size limit.
                let built = loop {
                    if count < 2 {
                        break None;
                    }
                    let candidates = &group[offset..offset + count];
                    if !can_meet_consolidation_fee_cap(candidates, fee_per_byte) {
                        count /= 2;
                        continue;
                    }
                    let outpoints = candidates
                        .iter()
                        .map(|(point, _)| point.clone())
                        .collect::<Vec<_>>();
                    match self.build_consolidation(&ledger, &outpoints, fee_per_byte, merge_roots) {
                        Ok(value) => break Some(value),
                        Err(_) => count /= 2,
                    }
                };
                if let Some((_, batch)) = built {
                    debug_assert_eq!(batch.kind, *kind);
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
    ) -> Result<(BuiltConsolidation, ConsolidationBatch)> {
        if !(2..=BATCH_INPUTS).contains(&outpoints.len()) {
            bail!("choose between 2 and 128 outputs per batch");
        }
        let wallet = self.wallet.unlocked()?;
        let hybrid_addresses = ledger.wallet_owned_hybrid_encoded_addresses(wallet)?;
        let mut confirmed = BTreeMap::new();
        let mut available = BTreeSet::new();
        for address in std::iter::once(self.wallet.address())
            .chain(hybrid_addresses.iter().map(String::as_str))
        {
            confirmed.extend(ledger.utxos_for_address(address));
            available.extend(
                ledger
                    .available_utxos_for_address(address)?
                    .into_iter()
                    .map(|(point, _)| point),
            );
        }
        let pending_spent = self.wallet_pending_spent_outpoints();
        available.retain(|point| !pending_spent.contains(point));
        let mut seen = BTreeSet::new();
        let root = ledger.consolidation_root(&outpoints[0]);
        let mut total: Amount = 0;
        let mut kind = None;
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
            let output_kind = if output.address == self.wallet.address() {
                ConsolidationKind::Legacy
            } else if hybrid_addresses.contains(&output.address) {
                ConsolidationKind::Hybrid
            } else {
                bail!("output is not owned by this wallet");
            };
            if kind
                .replace(output_kind)
                .is_some_and(|kind| kind != output_kind)
            {
                bail!("legacy and hybrid outputs require separate consolidation batches");
            }
            total = total
                .checked_add(output.amount)
                .context("input total overflows")?;
        }
        let kind = kind.context("consolidation batch has no outputs")?;
        let (transaction, bytes, fee) = match kind {
            ConsolidationKind::Legacy => {
                let (transaction, estimate) = converge_fee_by_byte(fee_per_byte, |fee| {
                    let amount = consolidation_amount(total, fee)?;
                    ledger.build_transfer_with_inputs(
                        wallet,
                        self.wallet.address(),
                        amount,
                        fee,
                        outpoints,
                    )
                })?;
                (
                    BuiltConsolidation::Legacy(transaction),
                    estimate.bytes,
                    estimate.fee,
                )
            }
            ConsolidationKind::Hybrid => {
                let (transaction, bytes, fee) =
                    converge_v2_consolidation_fee(ledger, wallet, outpoints, total, fee_per_byte)?;
                (BuiltConsolidation::Hybrid(transaction), bytes, fee)
            }
        };
        // Never recommend or accept batches spending over 1% of their value on fees.
        if u128::from(fee) * 100 > u128::from(total) {
            bail!("batch fee exceeds 1% of its value; use a lower fee or wait");
        }
        let batch = ConsolidationBatch {
            kind,
            utxos: outpoints.to_vec(),
            fee,
            amount: total - fee,
            bytes,
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
    ) -> Result<String> {
        if address != self.wallet.address() {
            bail!("wallet changed; review a new preview");
        }
        let ledger = self.wallet_build_ledger()?;
        let (transaction, batch) =
            self.build_consolidation(&ledger, outpoints, fee_per_byte, merge_roots)?;
        if batch.fee > max_fee {
            bail!("fee exceeds the approved limit; review a new preview");
        }
        match transaction {
            BuiltConsolidation::Legacy(transaction) => {
                let signature = transaction.signature().to_string();
                self.submit_public_transaction(transaction)?;
                Ok(signature)
            }
            BuiltConsolidation::Hybrid(transaction) => {
                let domain = self.ledger.transaction_v2_domain()?;
                let transaction_id = hex_encode(transaction.transaction_id(&domain)?);
                self.submit_public_transaction_v2(transaction)?;
                Ok(transaction_id)
            }
        }
    }
}

fn collect_consolidation_outputs(
    ledger: &Ledger,
    address: &str,
    kind: ConsolidationKind,
    confirmed: &mut Vec<(ConsolidationKind, OutPoint, TxOutput)>,
    available: &mut BTreeSet<OutPoint>,
) -> Result<()> {
    confirmed.extend(
        ledger
            .utxos_for_address(address)
            .into_iter()
            .map(|(point, output)| (kind, point, output)),
    );
    available.extend(
        ledger
            .available_utxos_for_address(address)?
            .into_iter()
            .map(|(point, _)| point),
    );
    Ok(())
}

fn consolidation_amount(total: Amount, fee: Amount) -> Result<Amount> {
    total
        .checked_sub(fee)
        .filter(|amount| *amount > 0)
        .context("outputs do not cover the network fee")
}

fn converge_v2_consolidation_fee(
    ledger: &Ledger,
    wallet: &crate::domain::Wallet,
    outpoints: &[OutPoint],
    total: Amount,
    fee_per_byte: Amount,
) -> Result<(TransactionV2, usize, Amount)> {
    let domain = ledger.transaction_v2_domain()?;
    let mut fee = 1;
    for _ in 0..64 {
        let amount = consolidation_amount(total, fee)?;
        let transaction =
            ledger.build_v2_consolidation_with_inputs(wallet, amount, fee, outpoints)?;
        let bytes = transaction.encoded_size_bytes(&domain)?;
        let required_fee = fee_per_byte
            .checked_mul(bytes as Amount)
            .context("fee per byte times transaction bytes overflows")?
            .max(1);
        if fee >= required_fee {
            return Ok((transaction, bytes, fee));
        }
        fee = required_fee;
    }
    bail!("hybrid consolidation fee did not converge")
}

fn can_meet_consolidation_fee_cap(candidates: &[(OutPoint, Amount)], fee_per_byte: Amount) -> bool {
    let total = candidates
        .iter()
        .map(|(_, amount)| u128::from(*amount))
        .sum::<u128>();
    let minimum_fee = (u128::from(fee_per_byte)
        * minimum_transfer_economic_size_bytes(candidates.len()) as u128)
        .max(1);
    minimum_fee * 100 <= total
}
