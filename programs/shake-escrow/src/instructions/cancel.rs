use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer};

use crate::{
    constants::*,
    error::ShakeError,
    events::{CancelCleared, CancelProposed, WagerCancelled, WagerClosed},
    state::*,
};

/// A two-step ceremony, and Active only. Before lock a side exits unilaterally with
/// unstake_side, so no Funding-state cancel exists. A proposal lives only while resolve
/// could still fire; past the deadline, refunds own that space exclusively.
#[derive(Accounts)]
#[event_cpi]
pub struct CancelPropose<'info> {
    pub participant: Signer<'info>,
    #[account(
        mut,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
}

pub fn handle_cancel_propose(ctx: Context<CancelPropose>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;
    let signer = ctx.accounts.participant.key();

    require!(wager.state == WagerState::Active, ShakeError::CancelWindowClosed);
    require!(now <= wager.deadline_resolve, ShakeError::CancelWindowClosed);
    require!(wager.is_participant(&signer), ShakeError::NotAParticipant);
    require!(
        wager.cancel_proposed_by == Pubkey::default(),
        ShakeError::CancelAlreadyOpen
    );

    let wager = &mut ctx.accounts.wager;
    wager.cancel_proposed_by = signer;
    emit_cpi!(CancelProposed {
        wager: wager.key(),
        by: signer,
    });
    Ok(())
}

/// Either participant clears an open proposal: the proposer retracting, or the
/// counterparty declining and keeping the wager alive. Without it, a forgotten proposal
/// would stay acceptable right up to the deadline.
#[derive(Accounts)]
#[event_cpi]
pub struct CancelClear<'info> {
    pub participant: Signer<'info>,
    #[account(
        mut,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
}

pub fn handle_cancel_clear(ctx: Context<CancelClear>) -> Result<()> {
    let wager = &ctx.accounts.wager;
    let signer = ctx.accounts.participant.key();
    require!(wager.is_participant(&signer), ShakeError::NotAParticipant);
    require!(
        wager.cancel_proposed_by != Pubkey::default(),
        ShakeError::CancelNotOpen
    );

    let wager = &mut ctx.accounts.wager;
    wager.cancel_proposed_by = Pubkey::default();
    emit_cpi!(CancelCleared {
        wager: wager.key(),
        by: signer,
    });
    Ok(())
}

/// The counterparty accepts. Both stakes walk home in full and never pay a fee, the
/// exposure counters and the global accumulator release, any surplus sweeps, and
/// everything closes with rent to the pinned collector. The whole thing is atomic: if
/// either transfer cannot execute because an account is frozen, the instruction reverts
/// and the wager stays Active, which leaves the per-side deadline refunds to finish it.
#[derive(Accounts)]
#[event_cpi]
pub struct CancelAccept<'info> {
    pub acceptor: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        close = rent_collector,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    /// CHECK: side A of the wager (address-pinned).
    #[account(address = wager.side_a @ ShakeError::NotAParticipant)]
    pub side_a: UncheckedAccount<'info>,
    /// CHECK: side B of the wager (address-pinned).
    #[account(address = wager.side_b @ ShakeError::NotAParticipant)]
    pub side_b: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = side_a,
    )]
    pub side_a_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = side_b,
    )]
    pub side_b_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [COUNTER_SEED, wager.side_a.as_ref()],
        bump = counter_a.bump,
    )]
    pub counter_a: Box<Account<'info, ExposureCounter>>,
    #[account(
        mut,
        seeds = [COUNTER_SEED, wager.side_b.as_ref()],
        bump = counter_b.bump,
    )]
    pub counter_b: Box<Account<'info, ExposureCounter>>,
    #[account(
        mut,
        address = wager.fee_destination @ ShakeError::WrongFeeDestination,
    )]
    pub fee_token: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: pinned to the wager's stored rent collector. The acceptor is never a
    /// lamport destination.
    #[account(mut, address = wager.rent_collector @ ShakeError::WrongRentCollector)]
    pub rent_collector: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_cancel_accept(ctx: Context<CancelAccept>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;
    let signer = ctx.accounts.acceptor.key();

    require!(wager.state == WagerState::Active, ShakeError::CancelWindowClosed);
    require!(now <= wager.deadline_resolve, ShakeError::CancelWindowClosed);
    require!(
        wager.cancel_proposed_by != Pubkey::default(),
        ShakeError::CancelNotOpen
    );
    require!(wager.is_participant(&signer), ShakeError::NotAParticipant);
    require!(
        signer != wager.cancel_proposed_by,
        ShakeError::CannotAcceptOwnProposal
    );

    let stake = wager.stake;
    let side_a_key = wager.side_a;
    let nonce_le = wager.nonce.to_le_bytes();
    let bump = wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];
    let signer_seeds = &[seeds];

    for to in [
        ctx.accounts.side_a_token.to_account_info(),
        ctx.accounts.side_b_token.to_account_info(),
    ] {
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to,
                    authority: ctx.accounts.wager.to_account_info(),
                },
                signer_seeds,
            ),
            stake,
        )?;
    }
    ctx.accounts.vault.reload()?;
    let surplus = ctx.accounts.vault.amount;
    if surplus > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.fee_token.to_account_info(),
                    authority: ctx.accounts.wager.to_account_info(),
                },
                signer_seeds,
            ),
            surplus,
        )?;
    }
    token::close_account(CpiContext::new_with_signer(
        token::ID,
        CloseAccount {
            account: ctx.accounts.vault.to_account_info(),
            destination: ctx.accounts.rent_collector.to_account_info(),
            authority: ctx.accounts.wager.to_account_info(),
        },
        signer_seeds,
    ))?;

    ctx.accounts.counter_a.count = ctx.accounts.counter_a.count.saturating_sub(1);
    ctx.accounts.counter_b.count = ctx.accounts.counter_b.count.saturating_sub(1);
    ctx.accounts.config.total_open_value = ctx
        .accounts
        .config
        .total_open_value
        .saturating_sub(stake.saturating_mul(2));

    let wager = &mut ctx.accounts.wager;
    wager.state = WagerState::Cancelled;
    wager.refunded_a = true;
    wager.refunded_b = true;

    emit_cpi!(WagerCancelled { wager: wager.key() });
    emit_cpi!(WagerClosed {
        wager: wager.key(),
        surplus_swept: surplus,
    });
    Ok(())
}
