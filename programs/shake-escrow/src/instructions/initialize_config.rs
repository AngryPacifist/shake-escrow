use anchor_lang::prelude::*;

use crate::{constants::*, error::ShakeError, state::Config};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeConfigArgs {
    pub mint: Pubkey,
    pub fee_bps: u16,
    pub fee_destination: Pubkey,
    pub rent_collector: Pubkey,
    pub min_stake: u64,
    pub max_stake: u64,
    pub max_open_per_wallet: u8,
    pub resolvers: Vec<Pubkey>,
    pub max_window: i64,
    pub max_total_open: u64,
}

/// One-time, and gated to the program's upgrade authority. After deploy nobody can race
/// this call to become admin, because the signer must be the key that deployed.
#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(
        constraint = program.programdata_address()? == Some(program_data.key())
            @ ShakeError::NotUpgradeAuthority
    )]
    pub program: Program<'info, crate::program::ShakeEscrow>,
    #[account(
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ ShakeError::NotUpgradeAuthority
    )]
    pub program_data: Box<Account<'info, ProgramData>>,
    pub system_program: Program<'info, System>,
}

pub fn validate_config_values(
    fee_bps: u16,
    min_stake: u64,
    max_stake: u64,
    resolvers: &[Pubkey],
    max_window: i64,
) -> Result<()> {
    require!(fee_bps <= MAX_FEE_BPS, ShakeError::FeeTooHigh);
    require!(min_stake > 0 && min_stake <= max_stake, ShakeError::BadStakeBounds);
    require!(
        !resolvers.is_empty()
            && resolvers.len() <= RESOLVER_ALLOWLIST_LEN
            && resolvers.iter().all(|r| *r != Pubkey::default()),
        ShakeError::BadResolverList
    );
    require!(max_window > 0 && max_window <= MAX_WINDOW_CAP, ShakeError::BadWindow);
    Ok(())
}

pub fn handle_initialize_config(
    ctx: Context<InitializeConfig>,
    args: InitializeConfigArgs,
) -> Result<()> {
    validate_config_values(
        args.fee_bps,
        args.min_stake,
        args.max_stake,
        &args.resolvers,
        args.max_window,
    )?;
    // A default-pubkey destination would make resolves and closes fail into the refund
    // path, which is safe but silent. Reject it here where the operator can see it.
    require!(
        args.mint != Pubkey::default()
            && args.fee_destination != Pubkey::default()
            && args.rent_collector != Pubkey::default(),
        ShakeError::DefaultAddress
    );

    let config = &mut ctx.accounts.config;
    config.admin = ctx.accounts.authority.key();
    config.mint = args.mint;
    config.fee_bps = args.fee_bps;
    config.fee_destination = args.fee_destination;
    config.rent_collector = args.rent_collector;
    config.min_stake = args.min_stake;
    config.max_stake = args.max_stake;
    config.max_open_per_wallet = args.max_open_per_wallet;
    config.resolver_allowlist = [Pubkey::default(); RESOLVER_ALLOWLIST_LEN];
    for (i, r) in args.resolvers.iter().enumerate() {
        config.resolver_allowlist[i] = *r;
    }
    config.resolver_count = args.resolvers.len() as u8;
    config.paused = false;
    config.max_window = args.max_window;
    config.max_total_open = args.max_total_open;
    config.total_open_value = 0;
    config.bump = ctx.bumps.config;
    Ok(())
}
