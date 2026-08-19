use super::Transaction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransactionKind {
    Burn,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BlockSelection {
    pub(super) transactions: Vec<Transaction>,
}

pub(super) fn fee_rate_key(transaction: &Transaction) -> u128 {
    let size = transaction.economic_size_bytes();
    if size == 0 {
        return 0;
    }
    u128::from(transaction.fee()) * 1_000_000 / size as u128
}
