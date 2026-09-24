pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("Hv4f6R9GgZ9nvK8HvA3KYwxPTrmiqyiE9KFvrENjgD67");

/// Shake escrow rail: a general peer-to-peer wager escrow on Solana.
///
/// Trust model in brief. No key can move vault funds anywhere except to a participant or
/// back to the stakers. Every state has a permissionless exit once its deadline passes.
/// The config admin can never reach a live wager, because each wager snapshots the config
/// values it needs at creation.
#[program]
pub mod shake_escrow {
    use super::*;

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        args: InitializeConfigArgs,
    ) -> Result<()> {
        instructions::initialize_config::handle_initialize_config(ctx, args)
    }

    pub fn update_config(ctx: Context<UpdateConfig>, args: UpdateConfigArgs) -> Result<()> {
        instructions::update_config::handle_update_config(ctx, args)
    }

    pub fn create_wager(ctx: Context<CreateWager>, args: CreateWagerArgs) -> Result<()> {
        instructions::create_wager::handle_create_wager(ctx, args)
    }

    pub fn stake_side(ctx: Context<StakeSide>) -> Result<()> {
        instructions::stake_side::handle_stake_side(ctx)
    }

    pub fn unstake_side(ctx: Context<UnstakeSide>) -> Result<()> {
        instructions::unstake_side::handle_unstake_side(ctx)
    }

    pub fn propose_admin(ctx: Context<ProposeAdmin>, new_admin: Pubkey) -> Result<()> {
        instructions::admin_transfer::handle_propose_admin(ctx, new_admin)
    }

    pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
        instructions::admin_transfer::handle_accept_admin(ctx)
    }

    pub fn cancel_admin_transfer(ctx: Context<CancelAdminTransfer>) -> Result<()> {
        instructions::admin_transfer::handle_cancel_admin_transfer(ctx)
    }

    pub fn resolve(ctx: Context<Resolve>, winner: Pubkey) -> Result<()> {
        instructions::resolve::handle_resolve(ctx, winner)
    }

    pub fn concede(ctx: Context<Concede>) -> Result<()> {
        instructions::concede::handle_concede(ctx)
    }

    pub fn refund_side(ctx: Context<RefundSide>) -> Result<()> {
        instructions::refund_side::handle_refund_side(ctx)
    }

    pub fn cancel_propose(ctx: Context<CancelPropose>) -> Result<()> {
        instructions::cancel::handle_cancel_propose(ctx)
    }

    pub fn cancel_clear(ctx: Context<CancelClear>) -> Result<()> {
        instructions::cancel::handle_cancel_clear(ctx)
    }

    pub fn cancel_accept(ctx: Context<CancelAccept>) -> Result<()> {
        instructions::cancel::handle_cancel_accept(ctx)
    }

    pub fn close_expired(ctx: Context<CloseExpired>) -> Result<()> {
        instructions::close_expired::handle_close_expired(ctx)
    }
}
