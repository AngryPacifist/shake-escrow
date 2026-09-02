use anchor_lang::prelude::*;

// One CPI-carried event per state transition. Plain `emit!` writes to transaction logs,
// which RPCs truncate under load; emit_cpi survives that.

#[event]
pub struct WagerCreated {
    pub wager: Pubkey,
    pub side_a: Pubkey,
    pub side_b: Pubkey,
    pub stake: u64,
    pub mint: Pubkey,
    pub resolver: Pubkey,
    pub terms_hash: [u8; 32],
    pub deadline_fund: i64,
    pub deadline_resolve: i64,
    pub nonce: u64,
}

#[event]
pub struct SideStaked {
    pub wager: Pubkey,
    pub staker: Pubkey,
    pub is_side_a: bool,
    pub amount: u64,
    /// True when this stake activated the wager (both sides now in).
    pub now_active: bool,
}

#[event]
pub struct SideUnstaked {
    pub wager: Pubkey,
    pub staker: Pubkey,
    pub amount: u64,
}

#[event]
pub struct WagerResolved {
    pub wager: Pubkey,
    pub winner: Pubkey,
    pub payout: u64,
    pub fee: u64,
}

#[event]
pub struct SideRefunded {
    pub wager: Pubkey,
    pub side: Pubkey,
    pub amount: u64,
}

#[event]
pub struct CancelProposed {
    pub wager: Pubkey,
    pub by: Pubkey,
}

#[event]
pub struct CancelCleared {
    pub wager: Pubkey,
    pub by: Pubkey,
}

#[event]
pub struct WagerCancelled {
    pub wager: Pubkey,
}

#[event]
pub struct WagerClosed {
    pub wager: Pubkey,
    /// Donations or dust swept to fee_destination at close. Zero in the normal case.
    pub surplus_swept: u64,
}
