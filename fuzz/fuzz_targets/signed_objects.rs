//! The public wallet API and relay accept these objects as JSON. Hashing, sizing and signing must
//! hold for any shape serde lets through, because they run before consensus validation.
#![no_main]

use std::sync::LazyLock;

use iuna::consensus::{Chain, TicketTransaction, Transaction, validate_pending_transaction};
use libfuzzer_sys::fuzz_target;

static GENESIS: LazyLock<Chain> = LazyLock::new(|| Chain::from_genesis().expect("pinned genesis"));

fuzz_target!(|bytes: &[u8]| {
    let genesis = &*GENESIS;
    if let Ok(transaction) = serde_json::from_slice::<Transaction>(bytes) {
        let _ = (transaction.id(), transaction.canonical_size());
        let _ = transaction.signing_digest(0, &vec![1; transaction.inputs.len()]);
        let _ = validate_pending_transaction(genesis.tip(), &transaction);
    }
    if let Ok(ticket) = serde_json::from_slice::<TicketTransaction>(bytes) {
        let _ = (ticket.id(), ticket.canonical_size());
        let _ = ticket.signing_digest(0, &vec![1; ticket.inputs.len()]);
        let _ = iuna::consensus::validate_pending_ticket(genesis.tip(), &ticket);
    }
});
