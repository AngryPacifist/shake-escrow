use anchor_lang::prelude::*;

use crate::constants::RESOLVER_ALLOWLIST_LEN;

/// One per deployment. PDA seeds ["config"]. The admin can update THIS account only, and
/// updates reach future wagers alone: no instruction lets any key touch a live Wager or
/// its vault.
#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    /// The only accepted stake mint, immutable for the program's life.
    pub mint: Pubkey,
    /// Fee in bps on the winner's payout only; refunds are always free. Capped in code.
    pub fee_bps: u16,
    /// Where settlement fees land. Any token account for `mint`.
    pub fee_destination: Pubkey,
    /// Receives the rent from every close. Set this to whichever account fronts create
    /// and counter rent, so the float cycles back instead of draining one way.
    pub rent_collector: Pubkey,
    pub min_stake: u64,
    pub max_stake: u64,
    /// How many funded wagers one wallet may hold at once, counted at stake time.
    pub max_open_per_wallet: u8,
    pub resolver_allowlist: [Pubkey; RESOLVER_ALLOWLIST_LEN],
    pub resolver_count: u8,
    /// Blocks create_wager only; never blocks any exit path.
    pub paused: bool,
    /// Max seconds for (deadline_fund − now) and (deadline_resolve − deadline_fund).
    /// Admin-tunable up to MAX_WINDOW_CAP.
    pub max_window: i64,
    /// Global circuit-breaker: stake_side fails once total_open_value + stake would
    /// exceed this. Admin-tunable, and effectively removable by setting u64::MAX.
    pub max_total_open: u64,
    /// Sum of all funded, un-exited stakes across live wagers.
    pub total_open_value: u64,
    pub bump: u8,
}

impl Config {
    pub fn resolver_allowed(&self, key: &Pubkey) -> bool {
        self.resolver_allowlist[..self.resolver_count as usize].contains(key)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum WagerState {
    Funding,
    Active,
    Resolved,
    Cancelled,
}

/// PDA seeds ["wager", side_a, nonce_le]. The vault is this PDA's associated token
/// account for `mint`, derived by constraint on every instruction and never trusted from
/// the caller.
#[account]
#[derive(InitSpace)]
pub struct Wager {
    pub state: WagerState,
    pub side_a: Pubkey,
    /// Always a real participant. A wager with an unfilled side never reaches the chain.
    pub side_b: Pubkey,
    pub stake: u64,
    pub mint: Pubkey,
    pub funded_a: bool,
    pub funded_b: bool,
    /// Sides exit independently, so one side's frozen or closed token account can only
    /// ever delay that side's own money.
    pub refunded_a: bool,
    pub refunded_b: bool,
    pub resolver: Pubkey,
    /// SHA-256 of the canonical (RFC 8785) terms JSON the participants agreed to.
    pub terms_hash: [u8; 32],
    pub deadline_fund: i64,
    pub deadline_resolve: i64,
    /// Snapshotted from Config at create, so later config changes never reach this wager.
    pub fee_bps: u16,
    pub fee_destination: Pubkey,
    pub rent_collector: Pubkey,
    pub nonce: u64,
    /// Pubkey::default() means no open cancel proposal. Cancel is a two-step ceremony and
    /// exists only while the wager is Active.
    pub cancel_proposed_by: Pubkey,
    pub bump: u8,
}

impl Wager {
    pub fn is_participant(&self, key: &Pubkey) -> bool {
        *key == self.side_a || *key == self.side_b
    }
    /// True when no funded side remains un-exited. Precondition for closing.
    pub fn fully_exited(&self) -> bool {
        (!self.funded_a || self.refunded_a) && (!self.funded_b || self.refunded_b)
    }
}

/// A pending handover of the admin role. PDA seeds ["admin_transfer"]. It exists only between
/// a proposal and its acceptance or cancellation, and both of those close it. It lives outside
/// Config so Config keeps the layout that existing deployments hold.
#[account]
#[derive(InitSpace)]
pub struct AdminTransfer {
    pub proposed: Pubkey,
    pub bump: u8,
}

/// Per-wallet funded-exposure counter. PDA seeds ["counter", wallet]. Deliberately has no
/// close instruction, because closing one would reset the cap it exists to enforce. Its
/// rent is fronted at the wallet's first stake and floats for the wallet's lifetime.
#[account]
#[derive(InitSpace)]
pub struct ExposureCounter {
    pub count: u8,
    pub bump: u8,
}
