// The admin surface: snapshot isolation on live wagers, initialization gating, config
// value guards including the exposure caps, and the two-step admin handover.
mod common;
use anchor_lang::Space;
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
    }
}

fn funded_key(env: &mut Env) -> Keypair {
    let k = Keypair::new();
    env.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
    k
}

fn transfer_rent(env: &mut Env) -> u64 {
    env.min_rent(8 + shake_escrow::state::AdminTransfer::INIT_SPACE)
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
fn non_admin_update_rejected() {
    let mut env = setup();
    let outsider = funded_key(&mut env);
    let ix = env.ix_update_config(&outsider.pubkey(), no_change_args());
    expect_err(env.send(&[&outsider], &[ix]), "NotAdmin");
}

#[test]
fn handover_takes_effect_only_when_the_successor_accepts() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let next = funded_key(&mut env);

    let ix = env.ix_propose_admin(&admin.pubkey(), &next.pubkey());
    env.send(&[&admin], &[ix]).expect("propose");

    let ix = env.ix_update_config(&admin.pubkey(), no_change_args());
    env.send(&[&admin], &[ix]).expect("old admin still updates");
    let ix = env.ix_update_config(&next.pubkey(), no_change_args());
    expect_err(env.send(&[&next], &[ix]), "NotAdmin");

    let rent = transfer_rent(&mut env);
    let admin_before = env.lamports(&admin.pubkey());
    let ix = env.ix_accept_admin(&next.pubkey(), &admin.pubkey());
    env.send(&[&next], &[ix]).expect("accept");
    assert_eq!(env.get_config().admin, next.pubkey());
    assert!(env.account_gone(&admin_transfer_pda()));
    assert_eq!(env.lamports(&admin.pubkey()), admin_before + rent);

    let ix = env.ix_update_config(&admin.pubkey(), no_change_args());
    expect_err(env.send(&[&admin], &[ix]), "NotAdmin");
    let ix = env.ix_update_config(&next.pubkey(), no_change_args());
    env.send(&[&next], &[ix]).expect("new admin updates");
}

#[test]
fn propose_guards() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let outsider = funded_key(&mut env);

    let ix = env.ix_propose_admin(&outsider.pubkey(), &outsider.pubkey());
    expect_err(env.send(&[&outsider], &[ix]), "NotAdmin");

    let ix = env.ix_propose_admin(&admin.pubkey(), &anchor_lang::prelude::Pubkey::default());
    expect_err(env.send(&[&admin], &[ix]), "BadNewAdmin");

    let ix = env.ix_propose_admin(&admin.pubkey(), &admin.pubkey());
    expect_err(env.send(&[&admin], &[ix]), "BadNewAdmin");
}

#[test]
fn only_the_proposed_key_can_accept() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let proposed = funded_key(&mut env);
    let impostor = funded_key(&mut env);

    let ix = env.ix_propose_admin(&admin.pubkey(), &proposed.pubkey());
    env.send(&[&admin], &[ix]).expect("propose");

    let ix = env.ix_accept_admin(&impostor.pubkey(), &admin.pubkey());
    expect_err(env.send(&[&impostor], &[ix]), "NotProposedAdmin");

    // The rent destination is pinned to the outgoing admin, so the successor cannot claim it.
    let ix = env.ix_accept_admin(&proposed.pubkey(), &proposed.pubkey());
    expect_err(env.send(&[&proposed], &[ix]), "NotAdmin");

    assert_eq!(env.get_config().admin, admin.pubkey());
}

#[test]
fn proposing_again_replaces_the_target() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let first = funded_key(&mut env);
    let second = funded_key(&mut env);

    let ix = env.ix_propose_admin(&admin.pubkey(), &first.pubkey());
    env.send(&[&admin], &[ix]).expect("first proposal");
    let ix = env.ix_propose_admin(&admin.pubkey(), &second.pubkey());
    env.send(&[&admin], &[ix]).expect("second proposal");

    let ix = env.ix_accept_admin(&first.pubkey(), &admin.pubkey());
    expect_err(env.send(&[&first], &[ix]), "NotProposedAdmin");
    let ix = env.ix_accept_admin(&second.pubkey(), &admin.pubkey());
    env.send(&[&second], &[ix]).expect("second accepts");
    assert_eq!(env.get_config().admin, second.pubkey());
}

#[test]
fn a_cancelled_proposal_cannot_be_accepted() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let proposed = funded_key(&mut env);

    let ix = env.ix_propose_admin(&admin.pubkey(), &proposed.pubkey());
    env.send(&[&admin], &[ix]).expect("propose");

    let ix = env.ix_cancel_admin_transfer(&proposed.pubkey());
    expect_err(env.send(&[&proposed], &[ix]), "NotAdmin");

    let rent = transfer_rent(&mut env);
    let admin_before = env.lamports(&admin.pubkey());
    // A second signer pays the fee, so the admin's balance moves by the rent alone.
    let payer = funded_key(&mut env);
    let ix = env.ix_cancel_admin_transfer(&admin.pubkey());
    env.send(&[&payer, &admin], &[ix]).expect("cancel");
    assert!(env.account_gone(&admin_transfer_pda()));
    assert_eq!(env.lamports(&admin.pubkey()), admin_before + rent);

    let ix = env.ix_accept_admin(&proposed.pubkey(), &admin.pubkey());
    assert!(env.send(&[&proposed], &[ix]).is_err(), "accept after cancel must fail");
    assert_eq!(env.get_config().admin, admin.pubkey());
}

#[test]
fn a_mistyped_proposal_changes_nothing() {
    // The fault the handover exists for: a well-formed key nobody holds. It can hold the
    // proposal for ever and never the role.
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let typo = Keypair::new().pubkey();
    let ix = env.ix_propose_admin(&admin.pubkey(), &typo);
    env.send(&[&admin], &[ix]).expect("propose to a typo");

    assert_eq!(env.get_config().admin, admin.pubkey());
    let ix = env.ix_update_config(&admin.pubkey(), no_change_args());
    env.send(&[&admin], &[ix]).expect("admin keeps the role");
}

#[test]
fn initialization_refuses_unusable_exposure_caps() {
    let mut env = setup_uninitialized();
    let admin = env.admin.insecure_clone();

    let mut args = env.default_config_args();
    args.max_open_per_wallet = 0;
    let ix = env.ix_initialize_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadExposureCaps");

    // One unit short of two maximum stakes: a wager at the maximum could never lock.
    let mut args = env.default_config_args();
    args.max_total_open = 2 * args.max_stake - 1;
    let ix = env.ix_initialize_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadExposureCaps");

    let mut args = env.default_config_args();
    args.max_total_open = 2 * args.max_stake;
    let ix = env.ix_initialize_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("room for exactly one maximum wager is enough");
}

#[test]
fn updates_refuse_unusable_exposure_caps() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let max_stake = env.get_config().max_stake;

    let mut args = no_change_args();
    args.max_open_per_wallet = Some(0);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadExposureCaps");

    let mut args = no_change_args();
    args.max_total_open = Some(2 * max_stake - 1);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadExposureCaps");

    let mut args = no_change_args();
    args.max_total_open = Some(2 * max_stake);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("global cap at two maximum stakes");

    // Raising the maximum stake past half the global cap is refused in the same way.
    let mut args = no_change_args();
    args.max_stake = Some(max_stake + 1);
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "BadExposureCaps");
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
