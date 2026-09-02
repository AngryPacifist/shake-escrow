use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer};

use crate::{constants::*, error::ShakeError, events::WagerClosed, state::*};

/// Permissionless: reclaims any past-deadline or Cancelled wager whose funded sides have
/// all exited. That includes wagers nobody ever funded, which per-side refunds cannot
/// serve because they have no funded side to pay. Sweeps donated surplus so a vault
/// always closes, and rent goes to the pinned collector rather than the cranker.
///
/// fee_token is optional. With zero surplus the close proceeds without it, so an unusable
/// fee account can never hold rent reclamation hostage. With a surplus present it is
/// required: blocking on a donation is acceptable, blocking rent is not.
#[derive(Accounts)]
#[event_cpi]
pub struct CloseExpired<'info> {
    pub cranker: Signer<'info>,
    #[account(
        mut,
        close = rent_collector,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    #[account(
        mut,
        associated_token::mint = wager.mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub fee_token: Option<Box<Account<'info, TokenAccount>>>,
    /// CHECK: pinned to the wager's stored rent collector. The cranker is never a
    /// lamport destination.
    #[account(mut, address = wager.rent_collector @ ShakeError::WrongRentCollector)]
    pub rent_collector: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_close_expired(ctx: Context<CloseExpired>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;

    let due = match wager.state {
        WagerState::Funding => now > wager.deadline_fund,
        WagerState::Active => now > wager.deadline_resolve,
        WagerState::Cancelled => true,
        // Resolved wagers close inside resolve itself; this state never persists.
        WagerState::Resolved => false,
    };
    require!(due, ShakeError::CloseNotDue);
    // Every funded side must have exited before rent can be reclaimed.
    require!(wager.fully_exited(), ShakeError::NotFullyExited);

    let side_a_key = wager.side_a;
    let nonce_le = wager.nonce.to_le_bytes();
    let bump = wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];
    let signer_seeds = &[seeds];

    let surplus = ctx.accounts.vault.amount;
    if surplus > 0 {
        let fee_token = ctx
            .accounts
            .fee_token
            .as_ref()
            .ok_or(ShakeError::WrongFeeDestination)?;
        require!(
            fee_token.key() == wager.fee_destination,
            ShakeError::WrongFeeDestination
        );
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: fee_token.to_account_info(),
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

    emit_cpi!(WagerClosed {
        wager: ctx.accounts.wager.key(),
        surplus_swept: surplus,
    });
    Ok(())
}
