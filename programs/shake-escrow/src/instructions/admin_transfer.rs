use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::ShakeError,
    events::{AdminTransferCancelled, AdminTransferProposed, AdminTransferred},
    state::{AdminTransfer, Config},
};

/// The admin names a successor. Nothing changes until that key signs accept_admin, so a
/// mistyped or unintended key can hold a proposal and never the role. Proposing again replaces
/// the pending proposal.
#[derive(Accounts)]
#[event_cpi]
pub struct ProposeAdmin<'info> {
    #[account(mut, address = config.admin @ ShakeError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + AdminTransfer::INIT_SPACE,
        seeds = [ADMIN_TRANSFER_SEED],
        bump
    )]
    pub admin_transfer: Box<Account<'info, AdminTransfer>>,
    pub system_program: Program<'info, System>,
}

pub fn handle_propose_admin(ctx: Context<ProposeAdmin>, new_admin: Pubkey) -> Result<()> {
    require!(
        new_admin != Pubkey::default() && new_admin != ctx.accounts.config.admin,
        ShakeError::BadNewAdmin
    );
    let transfer = &mut ctx.accounts.admin_transfer;
    transfer.proposed = new_admin;
    transfer.bump = ctx.bumps.admin_transfer;
    emit_cpi!(AdminTransferProposed { proposed: new_admin });
    Ok(())
}

/// The proposed key takes the role by signing. The pending proposal closes, and its rent goes
/// back to the outgoing admin, who paid it.
#[derive(Accounts)]
#[event_cpi]
pub struct AcceptAdmin<'info> {
    pub new_admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        close = previous_admin,
        seeds = [ADMIN_TRANSFER_SEED],
        bump = admin_transfer.bump,
        constraint = admin_transfer.proposed == new_admin.key() @ ShakeError::NotProposedAdmin,
    )]
    pub admin_transfer: Box<Account<'info, AdminTransfer>>,
    /// CHECK: the admin being replaced, pinned to Config before the handler reassigns it.
    #[account(mut, address = config.admin @ ShakeError::NotAdmin)]
    pub previous_admin: UncheckedAccount<'info>,
}

pub fn handle_accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
    let previous = ctx.accounts.config.admin;
    let admin = ctx.accounts.new_admin.key();
    ctx.accounts.config.admin = admin;
    emit_cpi!(AdminTransferred { previous, admin });
    Ok(())
}

/// The admin withdraws a pending proposal. Without this, a proposal naming a real but
/// unintended key would stay acceptable indefinitely.
#[derive(Accounts)]
#[event_cpi]
pub struct CancelAdminTransfer<'info> {
    #[account(mut, address = config.admin @ ShakeError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        close = admin,
        seeds = [ADMIN_TRANSFER_SEED],
        bump = admin_transfer.bump,
    )]
    pub admin_transfer: Box<Account<'info, AdminTransfer>>,
}

pub fn handle_cancel_admin_transfer(ctx: Context<CancelAdminTransfer>) -> Result<()> {
    emit_cpi!(AdminTransferCancelled {
        proposed: ctx.accounts.admin_transfer.proposed,
    });
    Ok(())
}
