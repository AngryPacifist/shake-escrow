use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer};

use crate::{
    constants::*,
    error::ShakeError,
    events::{WagerClosed, WagerResolved},
    state::*,
};

/// The resolver's only power: pick the winner among the two participants, before the
/// deadline. Payout and fee destinations are pinned, the instruction closes everything,
/// and rent returns to the collector the wager pinned at creation.
#[derive(Accounts)]
#[event_cpi]
pub struct Resolve<'info> {
    #[account(address = wager.resolver @ ShakeError::NotResolver)]
    pub resolver: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        close = rent_collector,
        seeds = [WAGER_SEED, wager.side_a.as_ref(), &wager.nonce.to_le_bytes()],
        bump = wager.bump,
    )]
    pub wager: Box<Account<'info, Wager>>,
    /// CHECK: must be one of the two participants (core invariant, enforced in handler).
    pub winner: UncheckedAccount<'info>,
    /// The winner's canonical ATA. Callers should pre-create it idempotently in the same
    /// transaction; a missing or frozen account fails the resolve cleanly and leaves the
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
    /// CHECK: pinned to the wager's stored rent collector. The cranker is never a
    /// lamport destination.
    #[account(mut, address = wager.rent_collector @ ShakeError::WrongRentCollector)]
    pub rent_collector: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn handle_resolve(ctx: Context<Resolve>, winner: Pubkey) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let wager = &ctx.accounts.wager;

    require!(wager.state == WagerState::Active, ShakeError::NotActive);
    // resolve requires now <= deadline and refund requires now > deadline, so the two
    // can never both be valid in the same slot.
    require!(now <= wager.deadline_resolve, ShakeError::ResolveExpired);
    require!(wager.is_participant(&winner), ShakeError::WinnerNotParticipant);
    require!(
        ctx.accounts.winner.key() == winner,
        ShakeError::WinnerNotParticipant
    );

    // pot = 2·stake. The fee floor-rounds through u128, and the remainder rides with the
    // winner rather than the fee account.
    let stake = wager.stake;
    let pot = stake.checked_mul(2).ok_or(ShakeError::MathOverflow)?;
    let fee = u64::try_from(
        (pot as u128)
            .checked_mul(wager.fee_bps as u128)
            .ok_or(ShakeError::MathOverflow)?
            / 10_000u128,
    )
    .map_err(|_| ShakeError::MathOverflow)?;
    let payout = pot.checked_sub(fee).ok_or(ShakeError::MathOverflow)?;

    let side_a_key = wager.side_a;
    let nonce_le = wager.nonce.to_le_bytes();
    let bump = wager.bump;
    let seeds: &[&[u8]] = &[WAGER_SEED, side_a_key.as_ref(), &nonce_le, &[bump]];
    let signer = &[seeds];

    token::transfer(
        CpiContext::new_with_signer(
            token::ID,
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.winner_token.to_account_info(),
                authority: ctx.accounts.wager.to_account_info(),
            },
            signer,
        ),
        payout,
    )?;
    if fee > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                token::ID,
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.fee_token.to_account_info(),
                    authority: ctx.accounts.wager.to_account_info(),
                },
                signer,
            ),
            fee,
        )?;
    }
    // Sweep any donated surplus so the vault always closes. A donation becomes a tip
    // instead of an account that can never be emptied.
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
                signer,
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
        signer,
    ))?;

    // Both sides' funded exposure ends here (saturating: exits never fail on books).
    ctx.accounts.counter_a.count = ctx.accounts.counter_a.count.saturating_sub(1);
    ctx.accounts.counter_b.count = ctx.accounts.counter_b.count.saturating_sub(1);
    ctx.accounts.config.total_open_value =
        ctx.accounts.config.total_open_value.saturating_sub(pot);

    let wager = &mut ctx.accounts.wager;
    wager.state = WagerState::Resolved;

    emit_cpi!(WagerResolved {
        wager: wager.key(),
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
