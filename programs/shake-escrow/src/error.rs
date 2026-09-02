use anchor_lang::prelude::*;

#[error_code]
pub enum ShakeError {
    // --- config ---
    #[msg("Signer is not the program's upgrade authority")]
    NotUpgradeAuthority,
    #[msg("Fee exceeds the hard cap")]
    FeeTooHigh,
    #[msg("Stake bounds invalid: need 0 < min_stake <= max_stake")]
    BadStakeBounds,
    #[msg("Resolver allowlist must hold 1 to 4 non-default keys")]
    BadResolverList,
    #[msg("max_window out of range (must be > 0 and <= MAX_WINDOW_CAP)")]
    BadWindow,
    #[msg("Signer is not the config admin")]
    NotAdmin,

    // --- create ---
    #[msg("New wagers are paused")]
    Paused,
    #[msg("Stake below the configured minimum")]
    StakeTooSmall,
    #[msg("Stake above the configured maximum")]
    StakeTooLarge,
    #[msg("Resolver is not on the allowlist")]
    ResolverNotAllowed,
    #[msg("Resolver cannot be a participant")]
    ResolverIsParticipant,
    #[msg("Side A and side B cannot be the same wallet")]
    SelfWager,
    #[msg("side_b must be a real participant: open wagers never reach the chain")]
    OpenSideNotAllowed,
    #[msg("Deadlines must satisfy now < deadline_fund < deadline_resolve")]
    BadDeadlines,
    #[msg("Deadline window exceeds the configured max_window")]
    WindowTooLong,
    #[msg("Terms hash must not be zero")]
    EmptyTermsHash,

    // --- stake / unstake ---
    #[msg("Wager is not accepting stakes")]
    NotFunding,
    #[msg("Funding deadline has passed")]
    FundingClosed,
    #[msg("This side has already staked")]
    AlreadyFunded,
    #[msg("Signer is not a participant in this wager")]
    NotAParticipant,
    #[msg("This wallet has reached its open-exposure cap")]
    WalletCapExceeded,
    #[msg("Global open-exposure cap reached")]
    GlobalCapExceeded,
    #[msg("This side has not staked")]
    SideNotFunded,

    // --- resolve ---
    #[msg("Wager is not active")]
    NotActive,
    #[msg("Resolution deadline has passed")]
    ResolveExpired,
    #[msg("Winner must be one of the two participants")]
    WinnerNotParticipant,
    #[msg("Signer is not this wager's resolver")]
    NotResolver,

    // --- refund / close ---
    #[msg("Refund conditions not met for the current state")]
    RefundNotDue,
    #[msg("This side already exited")]
    AlreadyExited,
    #[msg("Wager still has funded sides that have not exited")]
    NotFullyExited,
    #[msg("Close conditions not met for the current state")]
    CloseNotDue,

    // --- cancel ---
    #[msg("Cancel proposals only exist on active wagers before the resolve deadline")]
    CancelWindowClosed,
    #[msg("A cancel proposal is already open")]
    CancelAlreadyOpen,
    #[msg("No cancel proposal is open")]
    CancelNotOpen,
    #[msg("The proposer cannot accept their own cancel proposal")]
    CannotAcceptOwnProposal,

    // --- account validation ---
    #[msg("Token account owner does not match the expected side")]
    WrongTokenOwner,
    #[msg("Token account mint does not match the wager mint")]
    WrongMint,
    #[msg("Account does not match the wager's pinned fee destination")]
    WrongFeeDestination,
    #[msg("Account does not match the wager's pinned rent collector")]
    WrongRentCollector,
    #[msg("Duplicate account where distinct accounts are required")]
    DuplicateAccounts,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Config addresses must not be the default pubkey")]
    DefaultAddress,
}
