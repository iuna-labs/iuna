use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::{Amount, OutPoint, Transaction, TxOutput};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct UtxoLineageRoot {
    pub(super) outpoint: OutPoint,
    pub(super) height: u64,
}

pub(super) type LineageOwnerValues =
    BTreeMap<UtxoLineageRoot, BTreeMap<String, BTreeMap<OutPoint, Amount>>>;

pub(super) fn spend_inputs_with_lineage(
    transaction: &Transaction,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    utxo_lineage: &mut BTreeMap<OutPoint, UtxoLineageRoot>,
    lineage_values: &mut BTreeMap<UtxoLineageRoot, Amount>,
    lineage_owners: &mut LineageOwnerValues,
) -> Result<(Amount, Option<UtxoLineageRoot>)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    let mut inherited_root = None;
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
        if let Some(root) = utxo_lineage.remove(&input.outpoint) {
            subtract_lineage_value(lineage_values, &root, output.amount)?;
            subtract_lineage_owner_value(lineage_owners, &root, &output.address, &input.outpoint)?;
            inherited_root = newest_lineage_root(inherited_root, Some(root));
        }
    }
    Ok((total, inherited_root))
}

pub(super) fn insert_output_with_lineage(
    outpoint: OutPoint,
    output: TxOutput,
    root: Option<UtxoLineageRoot>,
    utxos: &mut BTreeMap<OutPoint, TxOutput>,
    utxo_lineage: &mut BTreeMap<OutPoint, UtxoLineageRoot>,
    lineage_values: &mut BTreeMap<UtxoLineageRoot, Amount>,
    lineage_owners: &mut LineageOwnerValues,
) -> Result<()> {
    if utxos.insert(outpoint.clone(), output.clone()).is_some() {
        bail!("created output replaces existing UTXO {}", outpoint.id());
    }
    if let Some(root) = root {
        utxo_lineage.insert(outpoint.clone(), root.clone());
        let value = lineage_values.entry(root.clone()).or_insert(0);
        *value = value
            .checked_add(output.amount)
            .context("lineage value overflows")?;
        lineage_owners
            .entry(root)
            .or_default()
            .entry(output.address)
            .or_default()
            .insert(outpoint, output.amount);
    }
    Ok(())
}

pub(super) fn newest_lineage_root(
    left: Option<UtxoLineageRoot>,
    right: Option<UtxoLineageRoot>,
) -> Option<UtxoLineageRoot> {
    match (left, right) {
        (None, None) => None,
        (Some(root), None) | (None, Some(root)) => Some(root),
        (Some(left), Some(right)) => {
            if (right.height, &right.outpoint) > (left.height, &left.outpoint) {
                Some(right)
            } else {
                Some(left)
            }
        }
    }
}

pub(super) fn output_lineage_root_for_transaction(
    transaction: &Transaction,
    block_height: u64,
    inherited_root: Option<UtxoLineageRoot>,
) -> Option<UtxoLineageRoot> {
    match transaction {
        Transaction::Mine { .. } => Some(UtxoLineageRoot {
            outpoint: OutPoint {
                txid: transaction.signature().to_string(),
                index: 0,
            },
            height: block_height,
        }),
        Transaction::Transfer { .. } | Transaction::Burn { .. } => inherited_root,
    }
}

fn subtract_lineage_value(
    lineage_values: &mut BTreeMap<UtxoLineageRoot, Amount>,
    root: &UtxoLineageRoot,
    amount: Amount,
) -> Result<()> {
    let value = lineage_values
        .get_mut(root)
        .context("lineage index is missing spent root")?;
    *value = value
        .checked_sub(amount)
        .context("lineage value underflows")?;
    if *value == 0 {
        lineage_values.remove(root);
    }
    Ok(())
}

fn subtract_lineage_owner_value(
    lineage_owners: &mut LineageOwnerValues,
    root: &UtxoLineageRoot,
    owner: &str,
    outpoint: &OutPoint,
) -> Result<()> {
    let owners = lineage_owners
        .get_mut(root)
        .context("lineage owner index is missing spent root")?;
    let outputs = owners
        .get_mut(owner)
        .context("lineage owner index is missing spent owner")?;
    outputs
        .remove(outpoint)
        .context("lineage owner index is missing spent output")?;
    if outputs.is_empty() {
        owners.remove(owner);
    }
    if owners.is_empty() {
        lineage_owners.remove(root);
    }
    Ok(())
}
