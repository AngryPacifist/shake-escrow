use trident_fuzz::fuzzing::*;

/// Storage for all account addresses used in fuzz testing.
///
/// This struct serves as a centralized repository for account addresses,
/// enabling their reuse across different instruction flows and test scenarios.
///
/// Docs: https://ackee.xyz/trident/docs/latest/trident-api-macro/trident-types/fuzz-accounts/
#[derive(Default)]
pub struct AccountAddresses {
    pub acceptor: AddressStorage,

    pub config: AddressStorage,

    pub wager: AddressStorage,

    pub side_a: AddressStorage,

    pub side_b: AddressStorage,

    pub side_a_token: AddressStorage,

    pub side_b_token: AddressStorage,

    pub counter_a: AddressStorage,

    pub counter_b: AddressStorage,

    pub fee_token: AddressStorage,

    pub vault: AddressStorage,

    pub rent_collector: AddressStorage,

    pub token_program: AddressStorage,

    pub event_authority: AddressStorage,

    pub program: AddressStorage,

    pub participant: AddressStorage,

    pub cranker: AddressStorage,

    pub payer: AddressStorage,

    pub mint: AddressStorage,

    pub associated_token_program: AddressStorage,

    pub system_program: AddressStorage,

    pub authority: AddressStorage,

    pub program_data: AddressStorage,

    pub side: AddressStorage,

    pub side_token: AddressStorage,

    pub counter: AddressStorage,

    pub resolver: AddressStorage,

    pub winner: AddressStorage,

    pub winner_token: AddressStorage,

    pub staker: AddressStorage,

    pub staker_token: AddressStorage,

    pub admin: AddressStorage,
}
