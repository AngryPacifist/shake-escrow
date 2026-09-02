use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::{constants::*, error::ShakeError, events::SideRefunded, state::*};

/// Permissionless: anyone can crank a due refund for one side. Sides exit independently,
/// so a frozen or closed account on one side only ever delays that side's own money.
/// Serves Funding and Active once their deadlines pass, and Cancelled with a side still
/// in. Never charges a fee. Closing the accounts afterwards is close_expired's job.
#[derive(Accounts)]
#[event_cpi]
pub struct RefundSide<'info> {
    /// Anyone. Pays the transaction fee and receives nothing.
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    /// CHECK: the side being refunded; must be a participant (handler).
    pub side: UncheckedAccount<'info>,
    /// Refunds pay the side's canonical ATA. Callers should pre-create it idempotently;
    /// a side that closed its ATA can re-create it and crank again later.
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = side,
    )]
    pub side_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [COUNTER_SEED, side.key().as_ref()],
        bump = counter.bump,
    )]
    pub counter: Box<Account<'info, ExposureCounter>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_refund_side(ctx: Context<RefundSide>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;
    let side = ctx.accounts.side.key();

    // Strictly after the operative deadline. Cancelled has no deadline gate, because the
    // wager is already over by consent.
    let due = match wager.state {
        WagerState::Funding => now > wager.deadline_fund,
        WagerState::Active => now > wager.deadline_resolve,
        WagerState::Cancelled => true,
        WagerState::Resolved => false,
    };
    require!(due, ShakeError::RefundNotDue);
    require!(wager.is_participant(&side), ShakeError::NotAParticipant);

    let is_a = side == wager.side_a;
    let (funded, refunded) = if is_a {
        (wager.funded_a, wager.refunded_a)
    } else {
        (wager.funded_b, wager.refunded_b)
    };
    require!(funded, ShakeError::SideNotFunded);
    require!(!refunded, ShakeError::AlreadyExited);

    let amount = wager.stake;
    let side_a_key = wager.side_a;
    let nonce_le = wager.nonce.to_le_bytes();
    let bump = wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];
    token::transfer(
        CpiContext::new_with_signer(
            token::ID,
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.side_token.to_account_info(),
                authority: ctx.accounts.wager.to_account_info(),
            },
            &[seeds],
        ),
        amount,
    )?;

    ctx.accounts.counter.count = ctx.accounts.counter.count.saturating_sub(1);
    ctx.accounts.config.total_open_value =
        ctx.accounts.config.total_open_value.saturating_sub(amount);

    let wager = &mut ctx.accounts.wager;
    if is_a {
        wager.refunded_a = true;
    } else {
        wager.refunded_b = true;
    }

    emit_cpi!(SideRefunded {
        wager: wager.key(),
        side,
        amount,
    });
    Ok(())
}
