use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::{constants::*, error::ShakeError, events::SideUnstaked, state::*};

/// Before lock, meaning state == Funding with the funding deadline not yet passed, a
/// funded side may withdraw its own stake unilaterally. Nothing is agreed until both
/// sides have funded, so pre-lock money should not need the counterparty's permission to
/// leave. After the funding deadline this path closes and the permissionless refund owns
/// that space instead.
#[derive(Accounts)]
#[event_cpi]
pub struct UnstakeSide<'info> {
    pub staker: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    #[account(
        mut,
        seeds = [COUNTER_SEED, staker.key().as_ref()],
        bump = counter.bump,
    )]
    pub counter: Box<Account<'info, ExposureCounter>>,
    /// Exits always pay the side's canonical ATA.
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = staker,
    )]
    pub staker_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_unstake_side(ctx: Context<UnstakeSide>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let staker = ctx.accounts.staker.key();

    require!(
        ctx.accounts.wager.state == WagerState::Funding,
        ShakeError::NotFunding
    );
    require!(
        now <= ctx.accounts.wager.deadline_fund,
        ShakeError::FundingClosed
    );
    let is_a = staker == ctx.accounts.wager.side_a;
    let is_b = staker == ctx.accounts.wager.side_b;
    require!(is_a || is_b, ShakeError::NotAParticipant);
    if is_a {
        require!(ctx.accounts.wager.funded_a, ShakeError::SideNotFunded);
    } else {
        require!(ctx.accounts.wager.funded_b, ShakeError::SideNotFunded);
    }

    let amount = ctx.accounts.wager.stake;
    let side_a_key = ctx.accounts.wager.side_a;
    let nonce_le = ctx.accounts.wager.nonce.to_le_bytes();
    let bump = ctx.accounts.wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];
    token::transfer(
        CpiContext::new_with_signer(
            token::ID,
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.staker_token.to_account_info(),
                authority: ctx.accounts.wager.to_account_info(),
            },
            &[seeds],
        ),
        amount,
    )?;

    // Exit paths must never fail on bookkeeping: saturating on the way down.
    ctx.accounts.config.total_open_value =
        ctx.accounts.config.total_open_value.saturating_sub(amount);
    ctx.accounts.counter.count = ctx.accounts.counter.count.saturating_sub(1);

    let wager = &mut ctx.accounts.wager;
    if is_a {
        wager.funded_a = false;
    } else {
        wager.funded_b = false;
    }

    emit_cpi!(SideUnstaked {
        wager: wager.key(),
        staker,
        amount,
    });
    Ok(())
}
