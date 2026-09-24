// Create-time guards: self-wagers, resolver identity, open sides, nonce reuse, stake
// bounds, foreign mints, deadline ordering, window caps, and pause semantics.
mod common;
use common::*;
use litesvm_token::CreateMint;
use solana_signer::Signer;

fn try_create(
    env: &mut Env,
    mutate: impl Fn(&mut shake_escrow::instructions::CreateWagerArgs),
) -> TxResult {
    let mut args = env.default_wager_args(99, 10 * USDC);
    mutate(&mut args);
    let ops = env.ops.insecure_clone();
    let ix = env.ix_create_wager(&ops.pubkey(), args);
    env.send(&[&ops], &[ix])
}

#[test]
fn self_wager_rejected() {
    let mut env = setup();
    let a = env.a.pubkey();
    expect_err(try_create(&mut env, |args| args.side_b = a), "SelfWager");
}

#[test]
fn resolver_participant_rejected() {
    let mut env = setup();
    let r = env.resolver.pubkey();
    expect_err(try_create(&mut env, |args| args.side_a = r), "ResolverIsParticipant");
    expect_err(try_create(&mut env, |args| args.side_b = r), "ResolverIsParticipant");
}

#[test]
fn open_side_rejected() {
    // A wager with an unfilled side would be claimable by any wallet watching the chain,
    // ahead of the person it was meant for. That surface does not exist here.
    let mut env = setup();
    expect_err(
        try_create(&mut env, |args| args.side_b = anchor_lang::prelude::Pubkey::default()),
        "OpenSideNotAllowed",
    );
}

#[test]
fn unlisted_resolver_rejected() {
    let mut env = setup();
    let stranger = solana_keypair::Keypair::new().pubkey();
    expect_err(try_create(&mut env, |args| args.resolver = stranger), "ResolverNotAllowed");
}

#[test]
fn deadline_and_terms_guards() {
    let mut env = setup();
    let now = env.now();
    // Unordered deadlines.
    expect_err(try_create(&mut env, |args| args.deadline_fund = now - 10), "BadDeadlines");
    expect_err(
        try_create(&mut env, |args| {
            args.deadline_resolve = args.deadline_fund - 1;
        }),
        "BadDeadlines",
    );
    // Windows above config.max_window, which defaults to 30 days here. The ordering is
    // kept valid so the window guard is what fires rather than BadDeadlines.
    expect_err(
        try_create(&mut env, |args| {
            args.deadline_fund = now + 31 * DAY;
            args.deadline_resolve = now + 31 * DAY + 600;
        }),
        "WindowTooLong",
    );
    expect_err(
        try_create(&mut env, |args| {
            args.deadline_resolve = args.deadline_fund + 31 * DAY;
        }),
        "WindowTooLong",
    );
    // A zero terms hash would commit to nothing.
    expect_err(try_create(&mut env, |args| args.terms_hash = [0u8; 32]), "EmptyTermsHash");
}

#[test]
fn stake_bounds_enforced() {
    // Both ends of the configured stake range, which is 1..100 USDC here.
    let mut env = setup();
    expect_err(try_create(&mut env, |args| args.stake = USDC - 1), "StakeTooSmall");
    expect_err(try_create(&mut env, |args| args.stake = 100 * USDC + 1), "StakeTooLarge");
}

#[test]
fn nonce_reuse_fails_cleanly() {
    // Initializing a PDA on seeds that already exist fails; a fresh nonce succeeds.
    let mut env = setup();
    env.create(7, 10 * USDC);
    let ops = env.ops.insecure_clone();
    let args = env.default_wager_args(7, 10 * USDC);
    let ix = env.ix_create_wager(&ops.pubkey(), args);
    assert!(env.send(&[&ops], &[ix]).is_err(), "duplicate nonce must fail");
    env.create(8, 10 * USDC);
}

#[test]
fn foreign_mint_rejected() {
    // The mint account must be the one the config pinned. There is no path here for a
    // token-2022 mint or a transfer hook to reach a vault.
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let other_mint = CreateMint::new(&mut env.svm, &admin).decimals(6).send().unwrap();
    let ops = env.ops.insecure_clone();
    let args = env.default_wager_args(9, 10 * USDC);
    let ix = env.ix_create_wager_with(&ops.pubkey(), &other_mint, args);
    expect_err(env.send(&[&ops], &[ix]), "WrongMint");
}

#[test]
fn paused_blocks_create_never_exits() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake");

    let admin = env.admin.insecure_clone();
    let ix = env.ix_update_config(
        &admin.pubkey(),
        shake_escrow::instructions::UpdateConfigArgs {
            paused: Some(true),
            fee_bps: None,
            fee_destination: None,
            rent_collector: None,
            min_stake: None,
            max_stake: None,
            max_open_per_wallet: None,
            resolvers: None,
            max_window: None,
            max_total_open: None,
        },
    );
    env.send(&[&admin], &[ix]).expect("pause");

    expect_err(try_create(&mut env, |_| {}), "Paused");

    // Exits stay open while paused: unstake now, refund after deadline.
    let ix = env.ix_unstake(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("unstake while paused");
    let b = env.b.insecure_clone();
    env.stake(&b, &wager).expect("stake B while paused (existing wager)");
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_fund + 1);
    let b_pk = b.pubkey();
    env.refund(&wager, &b_pk).expect("refund while paused");
}

#[test]
fn junk_create_touches_no_counter() {
    // A stranger paying rent to create wagers that name A never moves A's counter, so
    // nobody can fill someone else's exposure cap to lock them out.
    let mut env = setup();
    let stranger = solana_keypair::Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();
    for nonce in 50..55 {
        let args = env.default_wager_args(nonce, 10 * USDC);
        let ix = env.ix_create_wager(&stranger.pubkey(), args);
        env.send(&[&stranger], &[ix]).expect("junk create");
    }
    let a_pk = env.a.pubkey();
    assert_eq!(env.counter_count(&a_pk), 0);
    // A's real betting is unaffected.
    let wager = env.create(60, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("real stake unaffected");
}
