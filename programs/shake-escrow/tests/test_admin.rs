// The admin surface: snapshot isolation on live wagers, initialization gating, config
// value guards, and admin transfer.
mod common;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn no_change_args() -> shake_escrow::instructions::UpdateConfigArgs {
    shake_escrow::instructions::UpdateConfigArgs {
        fee_bps: None,
        fee_destination: None,
        rent_collector: None,
        min_stake: None,
        max_stake: None,
        max_open_per_wallet: None,
        resolvers: None,
        paused: None,
        max_window: None,
        max_total_open: None,
        new_admin: None,
    }
}

#[test]
fn init_by_non_upgrade_authority_fails() {
    // Only the key that deployed the program can initialize its config, which closes the
    // race where anyone could claim admin in the window right after a deploy.
    let mut env = setup();
    let racer = Keypair::new();
    env.svm.airdrop(&racer.pubkey(), 10_000_000_000).unwrap();
    let args = env.default_config_args();
    let ix = env.ix_initialize_config(&racer.pubkey(), args);
    assert!(env.send(&[&racer], &[ix]).is_err(), "non-authority init must fail");
}

#[test]
fn init_twice_fails() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let args = env.default_config_args();
    let ix = env.ix_initialize_config(&admin.pubkey(), args);
    assert!(env.send(&[&admin], &[ix]).is_err(), "second init must fail");
}

#[test]
fn config_snapshot_isolation() {
    // A live wager keeps the fee and destinations it snapshotted at creation, forever.
    let mut env = setup();
    let wager = env.active_wager(1, 50 * USDC);

    let admin = env.admin.insecure_clone();
    let mut args = no_change_args();
    args.fee_bps = Some(1_000); // crank fee to the 10% cap
    let ix = env.ix_update_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("fee update");

    let a_pk = env.a.pubkey();
    env.resolve_to(&wager, &a_pk).expect("resolve");
    // Old wager still pays 3%, not 10%.
    let fee_token = env.fee_token;
    assert_eq!(env.token_amount(&fee_token), 3 * USDC);

    // New wager picks up the new fee.
    let wager2 = env.active_wager(2, 50 * USDC);
    env.resolve_to(&wager2, &a_pk).expect("resolve 2");
    assert_eq!(env.token_amount(&fee_token), 3 * USDC + 10 * USDC);
}

#[test]
fn config_value_guards() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();

    let mut args = no_change_args();
    args.fee_bps = Some(1_001);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "FeeTooHigh");

    let mut args = no_change_args();
    args.max_window = Some(shake_escrow::constants::MAX_WINDOW_CAP + 1);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadWindow");

    let mut args = no_change_args();
    args.min_stake = Some(0);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadStakeBounds");

    let mut args = no_change_args();
    args.resolvers = Some(vec![]);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadResolverList");

    let mut args = no_change_args();
    args.resolvers = Some(vec![Keypair::new().pubkey(); 5]);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadResolverList");
}

#[test]
fn non_admin_update_rejected_and_admin_transfer_works() {
    let mut env = setup();
    let outsider = Keypair::new();
    env.svm.airdrop(&outsider.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_update_config(&outsider.pubkey(), no_change_args());
    expect_err(env.send(&[&outsider], &[ix]), "NotAdmin");

    // Transfer admin → outsider; old admin loses power, new one has it.
    let admin = env.admin.insecure_clone();
    let mut args = no_change_args();
    args.new_admin = Some(outsider.pubkey());
    let ix = env.ix_update_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("admin transfer");

    let ix = env.ix_update_config(&admin.pubkey(), no_change_args());
    expect_err(env.send(&[&admin], &[ix]), "NotAdmin");
    let ix = env.ix_update_config(&outsider.pubkey(), no_change_args());
    env.send(&[&outsider], &[ix]).expect("new admin works");
}

#[test]
fn resolver_rotation_affects_future_wagers_only() {
    // Rotating a compromised resolver out of the allowlist stops it being named on new
    // wagers. Live wagers keep the resolver they snapshotted, which is why rotation
    // bounds the damage rather than ending it.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);

    let new_resolver = Keypair::new();
    env.svm.airdrop(&new_resolver.pubkey(), 1_000_000_000).unwrap();
    let admin = env.admin.insecure_clone();
    let mut args = no_change_args();
    args.resolvers = Some(vec![new_resolver.pubkey()]);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("rotate");

    // Old resolver still resolves the live wager (snapshotted)...
    let a_pk = env.a.pubkey();
    env.resolve_to(&wager, &a_pk).expect("old resolver on old wager");
    // ...but cannot be named on a new one.
    let old = env.resolver.pubkey();
    let mut wargs = env.default_wager_args(2, 10 * USDC);
    wargs.resolver = old;
    let ops = env.ops.insecure_clone();
    let ix = env.ix_create_wager(&ops.pubkey(), wargs);
    expect_err(env.send(&[&ops], &[ix]), "ResolverNotAllowed");
}
