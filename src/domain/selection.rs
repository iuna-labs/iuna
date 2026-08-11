use super::{BlindedTransaction, Transaction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransactionKind {
    Burn,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BlockSelection {
    pub(super) transactions: Vec<Transaction>,
    pub(super) blinded_transactions: Vec<BlindedTransaction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectableItem {
    Plain(usize, u128),
    Blinded(usize, u128),
}

pub(super) fn fee_rate_key(transaction: &Transaction) -> u128 {
    let size = transaction.economic_size_bytes();
    if size == 0 {
        return 0;
    }
    u128::from(transaction.fee()) * 1_000_000 / size as u128
}

pub(super) fn blinded_fee_rate_key(transaction: &BlindedTransaction) -> u128 {
    let size = transaction.fee_rate_size_bytes();
    if size == 0 {
        return 0;
    }
    u128::from(transaction.fee) * 1_000_000 / size as u128
}

pub(super) fn best_selectable_item(
    plain: Option<SelectableItem>,
    blinded: Option<SelectableItem>,
) -> Option<SelectableItem> {
    match (plain, blinded) {
        (
            Some(SelectableItem::Plain(_, plain_rate)),
            Some(SelectableItem::Blinded(_, blind_rate)),
        ) => {
            if blind_rate > plain_rate {
                blinded
            } else {
                plain
            }
        }
        (Some(item), None) | (None, Some(item)) => Some(item),
        (None, None) => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectableItem, best_selectable_item};

    #[test]
    fn selectable_item_prefers_blinded_only_when_fee_rate_is_higher() {
        assert_eq!(
            best_selectable_item(
                Some(SelectableItem::Plain(1, 10)),
                Some(SelectableItem::Blinded(2, 11))
            ),
            Some(SelectableItem::Blinded(2, 11))
        );
        assert_eq!(
            best_selectable_item(
                Some(SelectableItem::Plain(1, 10)),
                Some(SelectableItem::Blinded(2, 10))
            ),
            Some(SelectableItem::Plain(1, 10))
        );
    }
}
