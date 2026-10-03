use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{NodeCore, helpers::converge_fee_by_byte};
use crate::domain::{
    AddressNetwork, Amount, Ledger, OutPoint, SignatureScheme, Transaction, TransactionV2, TxInput,
    TxOutput, VersionedAddress, encode_versioned_address, hex_encode,
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

struct ConsolidationInputs {
    kind: ConsolidationKind,
    total: Amount,
    hybrid_inputs: Vec<(OutPoint, VersionedAddress)>,
}

struct ConsolidationInventory {
    confirmed: BTreeMap<OutPoint, (TxOutput, ConsolidationKind)>,
    available: BTreeSet<OutPoint>,
    hybrid_owners: BTreeMap<String, VersionedAddress>,
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
        let inventory = self.consolidation_inventory(&ledger, wallet)?;
        let before = inventory.confirmed.len();
        let mut candidates = inventory
            .confirmed
            .iter()
            .map(|(point, (output, kind))| (*kind, point.clone(), output.clone()))
            .filter(|(_, point, _)| inventory.available.contains(point))
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
                    match self.preview_consolidation(
                        &ledger,
                        &inventory,
                        &outpoints,
                        fee_per_byte,
                        merge_roots,
                    ) {
                        Ok(batch) => break Some(batch),
                        Err(_) => count /= 2,
                    }
                };
                if let Some(batch) = built {
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

    fn consolidation_inventory(
        &self,
        ledger: &Ledger,
        wallet: &crate::domain::Wallet,
    ) -> Result<ConsolidationInventory> {
        let network = AddressNetwork::from_profile_id(&ledger.launch_profile().profile_id);
        let hybrid_owners = ledger
            .wallet_owned_hybrid_addresses(wallet)?
            .into_iter()
            .map(|owner| Ok((encode_versioned_address(owner, network)?, owner)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut wallet_addresses = hybrid_owners.keys().cloned().collect::<BTreeSet<_>>();
        wallet_addresses.insert(self.wallet.address().to_string());

        let confirmed = ledger
            .utxos_for_addresses(&wallet_addresses)
            .into_iter()
            .map(|(point, output)| {
                let kind = if hybrid_owners.contains_key(&output.address) {
                    ConsolidationKind::Hybrid
                } else {
                    ConsolidationKind::Legacy
                };
                (point, (output, kind))
            })
            .collect();
        let mut available = ledger
            .available_utxos_for_addresses(&wallet_addresses)?
            .into_iter()
            .map(|(point, _)| point)
            .collect::<BTreeSet<_>>();
        let pending_spent = self.wallet_pending_spent_outpoints();
        available.retain(|point| !pending_spent.contains(point));
        Ok(ConsolidationInventory {
            confirmed,
            available,
            hybrid_owners,
        })
    }

    fn consolidation_inputs(
        &self,
        ledger: &Ledger,
        inventory: &ConsolidationInventory,
        outpoints: &[OutPoint],
        merge_roots: bool,
    ) -> Result<ConsolidationInputs> {
        if !(2..=BATCH_INPUTS).contains(&outpoints.len()) {
            bail!("choose between 2 and 128 outputs per batch");
        }
        let mut seen = BTreeSet::new();
        let root = ledger.consolidation_root(&outpoints[0]);
        let mut total: Amount = 0;
        let mut kind = None;
        let mut hybrid_inputs = Vec::new();
        for point in outpoints {
            if !seen.insert(point) || !inventory.available.contains(point) {
                bail!("outputs changed or are reserved; review a new preview");
            }
            if !merge_roots && ledger.consolidation_root(point) != root {
                bail!("merging mining groups requires explicit consent");
            }
            let (output, output_kind) = inventory
                .confirmed
                .get(point)
                .context("output is no longer confirmed in this wallet")?;
            if kind
                .replace(*output_kind)
                .is_some_and(|kind| kind != *output_kind)
            {
                bail!("legacy and hybrid outputs require separate consolidation batches");
            }
            if let Some(owner) = inventory.hybrid_owners.get(&output.address) {
                hybrid_inputs.push((point.clone(), *owner));
            }
            total = total
                .checked_add(output.amount)
                .context("input total overflows")?;
        }
        let kind = kind.context("consolidation batch has no outputs")?;
        Ok(ConsolidationInputs {
            kind,
            total,
            hybrid_inputs,
        })
    }

    fn preview_consolidation(
        &self,
        ledger: &Ledger,
        inventory: &ConsolidationInventory,
        outpoints: &[OutPoint],
        fee_per_byte: Amount,
        merge_roots: bool,
    ) -> Result<ConsolidationBatch> {
        let inputs = self.consolidation_inputs(ledger, inventory, outpoints, merge_roots)?;
        let wallet = self.wallet.unlocked()?;
        let (bytes, fee) = match inputs.kind {
            ConsolidationKind::Legacy => estimate_legacy_consolidation_fee(
                ledger,
                self.wallet.address(),
                outpoints,
                inputs.total,
                fee_per_byte,
            )?,
            ConsolidationKind::Hybrid => estimate_v2_consolidation_fee(
                ledger,
                wallet,
                &inputs.hybrid_inputs,
                inputs.total,
                fee_per_byte,
            )?,
        };
        consolidation_batch(inputs.kind, outpoints, inputs.total, bytes, fee)
    }

    fn build_consolidation(
        &self,
        ledger: &Ledger,
        inventory: &ConsolidationInventory,
        outpoints: &[OutPoint],
        fee_per_byte: Amount,
        merge_roots: bool,
    ) -> Result<(BuiltConsolidation, ConsolidationBatch)> {
        let inputs = self.consolidation_inputs(ledger, inventory, outpoints, merge_roots)?;
        let wallet = self.wallet.unlocked()?;
        let (transaction, bytes, fee) = match inputs.kind {
            ConsolidationKind::Legacy => {
                let (transaction, estimate) = converge_fee_by_byte(fee_per_byte, |fee| {
                    let amount = consolidation_amount(inputs.total, fee)?;
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
                let (transaction, bytes, fee) = converge_v2_consolidation_fee(
                    ledger,
                    wallet,
                    &inputs.hybrid_inputs,
                    inputs.total,
                    fee_per_byte,
                )?;
                (BuiltConsolidation::Hybrid(transaction), bytes, fee)
            }
        };
        let batch = consolidation_batch(inputs.kind, outpoints, inputs.total, bytes, fee)?;
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
        let wallet = self.wallet.unlocked()?;
        let inventory = self.consolidation_inventory(&ledger, wallet)?;
        let (transaction, batch) =
            self.build_consolidation(&ledger, &inventory, outpoints, fee_per_byte, merge_roots)?;
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

fn consolidation_amount(total: Amount, fee: Amount) -> Result<Amount> {
    total
        .checked_sub(fee)
        .filter(|amount| *amount > 0)
        .context("outputs do not cover the network fee")
}

fn consolidation_batch(
    kind: ConsolidationKind,
    outpoints: &[OutPoint],
    total: Amount,
    bytes: usize,
    fee: Amount,
) -> Result<ConsolidationBatch> {
    // Never recommend or accept batches spending over 1% of their value on fees.
    if u128::from(fee) * 100 > u128::from(total) {
        bail!("batch fee exceeds 1% of its value; use a lower fee or wait");
    }
    Ok(ConsolidationBatch {
        kind,
        utxos: outpoints.to_vec(),
        fee,
        amount: consolidation_amount(total, fee)?,
        bytes,
    })
}

fn estimate_legacy_consolidation_fee(
    ledger: &Ledger,
    address: &str,
    outpoints: &[OutPoint],
    total: Amount,
    fee_per_byte: Amount,
) -> Result<(usize, Amount)> {
    let signature = "00".repeat(SignatureScheme::Ed25519.signature_bytes());
    let mut fee = 1;
    for _ in 0..64 {
        let amount = consolidation_amount(total, fee)?;
        let transaction = Transaction::Transfer {
            inputs: outpoints
                .iter()
                .map(|outpoint| TxInput {
                    outpoint: outpoint.clone(),
                    owner: address.to_string(),
                    signature: signature.clone(),
                })
                .collect(),
            outputs: vec![TxOutput {
                address: address.to_string(),
                amount,
            }],
            fee,
            signature: signature.clone(),
        };
        let bytes = transaction.economic_size_bytes();
        let required_fee = fee_per_byte
            .checked_mul(bytes as Amount)
            .context("fee per byte times transaction bytes overflows")?
            .max(1);
        if fee >= required_fee {
            ledger.ensure_legacy_transaction_within_block_budget(&transaction)?;
            return Ok((bytes, fee));
        }
        fee = required_fee;
    }
    bail!("legacy consolidation fee did not converge")
}

fn estimate_v2_consolidation_fee(
    ledger: &Ledger,
    wallet: &crate::domain::Wallet,
    inputs: &[(OutPoint, VersionedAddress)],
    total: Amount,
    fee_per_byte: Amount,
) -> Result<(usize, Amount)> {
    let bytes = ledger.estimate_v2_consolidation_size_with_input_owners(
        wallet,
        consolidation_amount(total, 1)?,
        1,
        inputs,
    )?;
    let fee = fee_per_byte
        .checked_mul(bytes as Amount)
        .context("fee per byte times transaction bytes overflows")?
        .max(1);
    consolidation_amount(total, fee)?;
    Ok((bytes, fee))
}

fn converge_v2_consolidation_fee(
    ledger: &Ledger,
    wallet: &crate::domain::Wallet,
    inputs: &[(OutPoint, VersionedAddress)],
    total: Amount,
    fee_per_byte: Amount,
) -> Result<(TransactionV2, usize, Amount)> {
    let (estimated_bytes, fee) =
        estimate_v2_consolidation_fee(ledger, wallet, inputs, total, fee_per_byte)?;
    let amount = consolidation_amount(total, fee)?;
    let transaction =
        ledger.build_v2_consolidation_with_input_owners(wallet, amount, fee, inputs)?;
    let bytes = transaction.encoded_size_bytes(&ledger.transaction_v2_domain()?)?;
    if bytes != estimated_bytes {
        bail!("hybrid consolidation size changed after signing; review a new preview");
    }
    Ok((transaction, bytes, fee))
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
