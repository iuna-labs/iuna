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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{TxInput, Wallet};

    fn root(txid: char, height: u64) -> UtxoLineageRoot {
        UtxoLineageRoot {
            outpoint: OutPoint {
                txid: txid.to_string().repeat(64),
                index: 0,
            },
            height,
        }
    }

    #[test]
    fn newest_lineage_uses_height_then_deterministic_outpoint_tiebreak() {
        let old = root('f', 4);
        let same_age_low = root('1', 5);
        let same_age_high = root('2', 5);

        assert_eq!(
            newest_lineage_root(Some(old), Some(same_age_low.clone())),
            Some(same_age_low.clone())
        );
        assert_eq!(
            newest_lineage_root(Some(same_age_low), Some(same_age_high.clone())),
            Some(same_age_high)
        );
        assert_eq!(newest_lineage_root(None, None), None);
    }

    #[test]
    fn transfer_descendants_inherit_the_newest_spent_mine_root() {
        let wallet = Wallet::from_seed("lineage-merge-wallet");
        let older = root('1', 4);
        let newer = root('2', 5);
        let older_outpoint = OutPoint {
            txid: "a".repeat(64),
            index: 0,
        };
        let newer_outpoint = OutPoint {
            txid: "b".repeat(64),
            index: 0,
        };
        let transaction = Transaction::Transfer {
            inputs: vec![
                TxInput {
                    outpoint: older_outpoint.clone(),
                    owner: wallet.address().to_string(),
                    signature: "s".repeat(128),
                },
                TxInput {
                    outpoint: newer_outpoint.clone(),
                    owner: wallet.address().to_string(),
                    signature: "s".repeat(128),
                },
            ],
            outputs: vec![TxOutput {
                address: wallet.address().to_string(),
                amount: 12,
            }],
            fee: 1,
            signature: "s".repeat(128),
        };
        let mut utxos = BTreeMap::from([
            (
                older_outpoint.clone(),
                TxOutput {
                    address: wallet.address().to_string(),
                    amount: 5,
                },
            ),
            (
                newer_outpoint.clone(),
                TxOutput {
                    address: wallet.address().to_string(),
                    amount: 8,
                },
            ),
        ]);
        let mut utxo_lineage = BTreeMap::from([
            (older_outpoint.clone(), older.clone()),
            (newer_outpoint.clone(), newer.clone()),
        ]);
        let mut lineage_values = BTreeMap::from([(older.clone(), 5), (newer.clone(), 8)]);
        let mut lineage_owners = BTreeMap::from([
            (
                older.clone(),
                BTreeMap::from([(
                    wallet.address().to_string(),
                    BTreeMap::from([(older_outpoint, 5)]),
                )]),
            ),
            (
                newer.clone(),
                BTreeMap::from([(
                    wallet.address().to_string(),
                    BTreeMap::from([(newer_outpoint, 8)]),
                )]),
            ),
        ]);

        let (_, inherited) = spend_inputs_with_lineage(
            &transaction,
            &mut utxos,
            &mut utxo_lineage,
            &mut lineage_values,
            &mut lineage_owners,
        )
        .unwrap();
        assert_eq!(inherited, Some(newer.clone()));

        let descendant = OutPoint {
            txid: "c".repeat(64),
            index: 0,
        };
        insert_output_with_lineage(
            descendant.clone(),
            TxOutput {
                address: wallet.address().to_string(),
                amount: 12,
            },
            inherited,
            &mut utxos,
            &mut utxo_lineage,
            &mut lineage_values,
            &mut lineage_owners,
        )
        .unwrap();

        assert_eq!(utxo_lineage.get(&descendant), Some(&newer));
        assert_eq!(lineage_values, BTreeMap::from([(newer, 12)]));
    }

    #[test]
    fn mine_outputs_start_roots_and_unrooted_outputs_stay_unweighted() {
        let mine = Transaction::Mine {
            recipient: "recipient".to_string(),
            anchor: "a".repeat(64),
            salt: 1,
            nonce: 1,
            difficulty_bits: 10,
            proof_header: None,
            signature: "b".repeat(64),
        };
        let transfer = Transaction::Transfer {
            inputs: Vec::new(),
            outputs: Vec::new(),
            fee: 1,
            signature: "c".repeat(128),
        };

        assert_eq!(
            output_lineage_root_for_transaction(&mine, 20, None),
            Some(UtxoLineageRoot {
                outpoint: OutPoint {
                    txid: "b".repeat(64),
                    index: 0,
                },
                height: 20,
            })
        );
        assert_eq!(
            output_lineage_root_for_transaction(&transfer, 20, None),
            None
        );
    }
}
