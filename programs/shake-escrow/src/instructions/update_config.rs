use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::ShakeError,
    instructions::initialize_config::validate_config_values,
    state::Config,
};

/// Every field optional; omitted fields keep their value. The mint is immutable for the
/// program's life and total_open_value is program-owned accounting, so neither can be set
/// here. The admin role changes hands through propose_admin and accept_admin. Changes reach
/// future wagers only: a live wager carries the snapshot it took at creation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateConfigArgs {
    pub fee_bps: Option<u16>,
    pub fee_destination: Option<Pubkey>,
    pub rent_collector: Option<Pubkey>,
    pub min_stake: Option<u64>,
    pub max_stake: Option<u64>,
    pub max_open_per_wallet: Option<u8>,
    pub resolvers: Option<Vec<Pubkey>>,
    pub paused: Option<bool>,
    pub max_window: Option<i64>,
    pub max_total_open: Option<u64>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(address = config.admin @ ShakeError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
}

pub fn handle_update_config(ctx: Context<UpdateConfig>, args: UpdateConfigArgs) -> Result<()> {
    let config = &mut ctx.accounts.config;

    let fee_bps = args.fee_bps.unwrap_or(config.fee_bps);
    let min_stake = args.min_stake.unwrap_or(config.min_stake);
    let max_stake = args.max_stake.unwrap_or(config.max_stake);
    let max_window = args.max_window.unwrap_or(config.max_window);
    let max_open_per_wallet = args.max_open_per_wallet.unwrap_or(config.max_open_per_wallet);
    let max_total_open = args.max_total_open.unwrap_or(config.max_total_open);
    let default_resolvers: Vec<Pubkey> =
        config.resolver_allowlist[..config.resolver_count as usize].to_vec();
    let resolvers = args.resolvers.as_deref().unwrap_or(&default_resolvers);

    validate_config_values(
        fee_bps,
        min_stake,
        max_stake,
        resolvers,
        max_window,
        max_open_per_wallet,
        max_total_open,
    )?;

    config.fee_bps = fee_bps;
    config.min_stake = min_stake;
    config.max_stake = max_stake;
    config.max_window = max_window;
    config.max_open_per_wallet = max_open_per_wallet;
    config.max_total_open = max_total_open;
    if let Some(v) = args.fee_destination {
        require!(v != Pubkey::default(), ShakeError::DefaultAddress);
        config.fee_destination = v;
    }
    if let Some(v) = args.rent_collector {
        require!(v != Pubkey::default(), ShakeError::DefaultAddress);
        config.rent_collector = v;
    }
    if args.resolvers.is_some() {
        config.resolver_allowlist = [Pubkey::default(); RESOLVER_ALLOWLIST_LEN];
        for (i, r) in resolvers.iter().enumerate() {
            config.resolver_allowlist[i] = *r;
        }
        config.resolver_count = resolvers.len() as u8;
    }
    if let Some(v) = args.paused {
        config.paused = v;
    }
    Ok(())
}
