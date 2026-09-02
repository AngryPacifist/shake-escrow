use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::{constants::*, error::ShakeError, events::SideStaked, state::*};

/// The staker signs the transfer. A separate payer fronts the exposure counter's rent on
/// a wallet's first stake, which lets an operator absorb that cost so the staker sees
/// only their own stake leave. The payer may equal the staker.
#[derive(Accounts)]
#[event_cpi]
pub struct StakeSide<'info> {
    pub staker: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + ExposureCounter::INIT_SPACE,
        seeds = [COUNTER_SEED, staker.key().as_ref()],
        bump
    )]
    pub counter: Box<Account<'info, ExposureCounter>>,
    #[account(
        mut,
        constraint = staker_token.owner == staker.key() @ ShakeError::WrongTokenOwner,
        constraint = staker_token.mint == wager.mint @ ShakeError::WrongMint,
    )]
    pub staker_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_stake_side(ctx: Context<StakeSide>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let staker = ctx.accounts.staker.key();

    // State and deadline checks fail before any transfer moves.
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
        require!(!ctx.accounts.wager.funded_a, ShakeError::AlreadyFunded);
    } else {
        require!(!ctx.accounts.wager.funded_b, ShakeError::AlreadyFunded);
    }

    // Per-wallet exposure cap, counted at stake time. Counting funded exposure rather
    // than created wagers is what makes a wager created against you unable to fill it.
    require!(
        ctx.accounts.counter.count < ctx.accounts.config.max_open_per_wallet,
        ShakeError::WalletCapExceeded
    );
    // Global circuit-breaker: bounds total value at risk across every live wager.
    let amount = ctx.accounts.wager.stake;
    let new_total = ctx
        .accounts
        .config
        .total_open_value
        .checked_add(amount)
        .ok_or(ShakeError::MathOverflow)?;
    require!(
        new_total <= ctx.accounts.config.max_total_open,
        ShakeError::GlobalCapExceeded
    );

    token::transfer(
        CpiContext::new(
            token::ID,
            Transfer {
                from: ctx.accounts.staker_token.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
                authority: ctx.accounts.staker.to_account_info(),
            },
        ),
        amount,
    )?;

    ctx.accounts.config.total_open_value = new_total;
    let counter = &mut ctx.accounts.counter;
    counter.count = counter.count.checked_add(1).ok_or(ShakeError::MathOverflow)?;
    counter.bump = ctx.bumps.counter;

    let wager = &mut ctx.accounts.wager;
    if is_a {
        wager.funded_a = true;
    } else {
        wager.funded_b = true;
    }
    let now_active = wager.funded_a && wager.funded_b;
    if now_active {
        wager.state = WagerState::Active;
    }

    emit_cpi!(SideStaked {
        wager: wager.key(),
        staker,
        is_side_a: is_a,
        amount,
        now_active,
    });
    Ok(())
}
