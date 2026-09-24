use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};

use crate::{
    constants::*,
    error::ShakeError,
    events::{WagerClosed, WagerConceded},
    instructions::payout::{pay_winner_and_close_vault, split_pot},
    state::*,
};

/// A participant gives a locked wager to the other side. The payout, the fee and every
/// destination are exactly what resolve would produce with the counterparty as winner. The only
/// stake this can move against its owner's will is the signer's own, so it needs no resolver.
/// The fee applies as it does on resolve; otherwise any settlement could be routed through a
/// concession to skip it.
#[derive(Accounts)]
#[event_cpi]
pub struct Concede<'info> {
    pub conceder: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        close = rent_collector,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    /// CHECK: must be the conceder's counterparty (enforced in handler).
    pub winner: UncheckedAccount<'info>,
    /// The winner's canonical ATA. Callers should pre-create it idempotently in the same
    /// transaction; a missing or frozen account fails the concession cleanly and leaves the
    /// timeout refund as the exit.
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = winner,
    )]
    pub winner_token: Box<Account<'info, TokenAccount>>,
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
    /// CHECK: pinned to the wager's stored rent collector. The conceder is never a lamport
    /// destination.
    #[account(mut, address = wager.rent_collector @ ShakeError::WrongRentCollector)]
    pub rent_collector: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_concede(ctx: Context<Concede>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;
    let conceder = ctx.accounts.conceder.key();

    require!(wager.state == WagerState::Active, ShakeError::NotActive);
    // The same window as resolve. A locked wager's refunds start one second after
    // deadline_resolve, so before then neither side can have been refunded and a concession
    // always meets a vault holding both stakes.
    require!(now <= wager.deadline_resolve, ShakeError::ResolveExpired);
    require!(wager.is_participant(&conceder), ShakeError::NotAParticipant);
    let winner = if conceder == wager.side_a {
        wager.side_b
    } else {
        wager.side_a
    };
    // The accounts struct leaves `winner` free, and `winner_token` only has to belong to it, so
    // this check is what keeps the pot inside the wager: without it a conceder could name
    // themselves, or a stranger, and the payout would go there.
    require!(
        ctx.accounts.winner.key() == winner,
        ShakeError::NotCounterparty
    );

    let (payout, fee) = split_pot(wager.stake, wager.fee_bps)?;
    let pot = payout + fee;

    let side_a_key = wager.side_a;
    let nonce_le = wager.nonce.to_le_bytes();
    let bump = wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];

    let wager_info = ctx.accounts.wager.to_account_info();
    let winner_token = ctx.accounts.winner_token.to_account_info();
    let fee_token = ctx.accounts.fee_token.to_account_info();
    let rent_collector = ctx.accounts.rent_collector.to_account_info();
    let surplus = pay_winner_and_close_vault(
        wager_info,
        &[seeds],
        &mut ctx.accounts.vault,
        winner_token,
        fee_token,
        rent_collector,
        payout,
        fee,
    )?;

    // Both sides' funded exposure ends here (saturating: exits never fail on books).
    ctx.accounts.counter_a.count = ctx.accounts.counter_a.count.saturating_sub(1);
    ctx.accounts.counter_b.count = ctx.accounts.counter_b.count.saturating_sub(1);
    ctx.accounts.config.total_open_value =
        ctx.accounts.config.total_open_value.saturating_sub(pot);

    let wager = &mut ctx.accounts.wager;
    wager.state = WagerState::Resolved;

    emit_cpi!(WagerConceded {
        wager: wager.key(),
        conceder,
        winner,
        payout,
        fee,
    });
    emit_cpi!(WagerClosed {
        wager: wager.key(),
        surplus_swept: surplus,
    });
    Ok(())
}
