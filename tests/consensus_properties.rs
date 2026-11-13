use iuna::consensus::{INITIAL_LEDGER_SUBSIDY, MAX_TICKET_DURATION, minimum_ticket_budget};
use proptest::prelude::*;

proptest! {
    #[test]
    fn minimum_ticket_budget_is_defined_for_every_valid_duration(
        duration in 1_u64..=MAX_TICKET_DURATION,
        reward in 0_u64..=INITIAL_LEDGER_SUBSIDY,
    ) {
        let budget = minimum_ticket_budget(duration, reward).unwrap();
        prop_assert!(budget > 0);
        prop_assert!(budget <= iuna::consensus::MAX_MONEY);
    }
}
