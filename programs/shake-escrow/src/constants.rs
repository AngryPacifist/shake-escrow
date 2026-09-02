use anchor_lang::prelude::*;

#[constant]
pub const CONFIG_SEED: &[u8] = b"config";

#[constant]
pub const WAGER_SEED: &[u8] = b"wager";

#[constant]
pub const COUNTER_SEED: &[u8] = b"counter";

/// Hard ceiling on the fee, in code rather than config: 10%. A rogue or fat-fingered
/// admin cannot exceed it even for future wagers.
#[constant]
pub const MAX_FEE_BPS: u16 = 1_000;

/// Fixed capacity of the resolver allowlist. Deliberately small.
pub const RESOLVER_ALLOWLIST_LEN: usize = 4;

/// Hard ceiling on `Config.max_window`: 180 days in seconds. The config value is
/// admin-tunable below this cap only, so no admin can stretch a wager's life past it.
#[constant]
pub const MAX_WINDOW_CAP: i64 = 180 * 86_400;
