use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};

use crate::{constants::*, error::ShakeError, events::WagerCreated, state::*};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateWagerArgs {
    pub nonce: u64,
    pub side_a: Pubkey,
    pub side_b: Pubkey,
    pub stake: u64,
    pub terms_hash: [u8; 32],
    pub deadline_fund: i64,
    pub deadline_resolve: i64,
    pub resolver: Pubkey,
}

/// Creating a wager is permissionless. The payer fronts the rent, and the named sides
/// commit nothing until they sign their own stakes. It touches no exposure counters, so
/// a wager created to grief someone is inert: nobody funds it, it cannot fill anyone's
/// cap, and close_expired reclaims its rent to the pinned collector afterwards.
#[derive(Accounts)]
#[instruction(args: CreateWagerArgs)]
#[event_cpi]
pub struct CreateWager<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(
        init,
        payer = payer,
        space = 8 + Wager::INIT_SPACE,
        seeds = [WAGER_SEED, args.side_a.as_ref(), &args.nonce.to_le_bytes()],
        bump
    )]
    pub wager: Box<Account<'info, Wager>>,
    #[account(address = config.mint @ ShakeError::WrongMint)]
    pub mint: Box<Account<'info, Mint>>,
    // init_if_needed rather than init: the vault ATA address is derivable in advance, so
    // anyone can pre-create it and make a plain `init` fail. A pre-existing vault is
    // harmless, because its authority is the wager PDA by derivation and any balance
    // donated into it sweeps to fee_destination at close.
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = wager,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_wager(ctx: Context<CreateWager>, args: CreateWagerArgs) -> Result<()> {
    let config = &ctx.accounts.config;
    let now = Clock::get()?.unix_timestamp;

    require!(!config.paused, ShakeError::Paused);
    require!(args.stake >= config.min_stake, ShakeError::StakeTooSmall);
    require!(args.stake <= config.max_stake, ShakeError::StakeTooLarge);
    require!(
        config.resolver_allowed(&args.resolver),
        ShakeError::ResolverNotAllowed
    );
    // The resolver is never a participant in the wager it decides.
    require!(
        args.resolver != args.side_a && args.resolver != args.side_b,
        ShakeError::ResolverIsParticipant
    );
    // Both sides are real wallets. A wager with an open side would be claimable by any
    // watcher on the chain, so that surface does not exist here.
    require!(args.side_b != Pubkey::default(), ShakeError::OpenSideNotAllowed);
    require!(args.side_a != Pubkey::default(), ShakeError::OpenSideNotAllowed);
    // No betting yourself.
    require!(args.side_b != args.side_a, ShakeError::SelfWager);
    // Ordered deadlines, with both windows bounded by config.max_window.
    require!(
        now < args.deadline_fund && args.deadline_fund < args.deadline_resolve,
        ShakeError::BadDeadlines
    );
    require!(
        args.deadline_fund - now <= config.max_window
            && args.deadline_resolve - args.deadline_fund <= config.max_window,
        ShakeError::WindowTooLong
    );
    require!(args.terms_hash != [0u8; 32], ShakeError::EmptyTermsHash);

    let wager = &mut ctx.accounts.wager;
    wager.state = WagerState::Funding;
    wager.side_a = args.side_a;
    wager.side_b = args.side_b;
    wager.stake = args.stake;
    wager.mint = config.mint;
    wager.funded_a = false;
    wager.funded_b = false;
    wager.refunded_a = false;
    wager.refunded_b = false;
    wager.resolver = args.resolver;
    wager.terms_hash = args.terms_hash;
    wager.deadline_fund = args.deadline_fund;
    wager.deadline_resolve = args.deadline_resolve;
    wager.fee_bps = config.fee_bps;
    wager.fee_destination = config.fee_destination;
    wager.rent_collector = config.rent_collector;
    wager.nonce = args.nonce;
    wager.cancel_proposed_by = Pubkey::default();
    wager.bump = ctx.bumps.wager;

    emit_cpi!(WagerCreated {
        wager: wager.key(),
        side_a: args.side_a,
        side_b: args.side_b,
        stake: args.stake,
        mint: config.mint,
        resolver: args.resolver,
        terms_hash: args.terms_hash,
        deadline_fund: args.deadline_fund,
        deadline_resolve: args.deadline_resolve,
        nonce: args.nonce,
    });
    Ok(())
}
