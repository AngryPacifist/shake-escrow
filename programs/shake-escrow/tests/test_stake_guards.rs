// Stake-time guards and the exposure books: double staking, non-participants, the
// per-wallet cap, the global cap, and fee arithmetic near the top of the u64 range.
mod common;
use common::*;
use litesvm_token::CreateAssociatedTokenAccount;
use solana_keypair::Keypair;
use solana_signer::Signer;

#[test]
fn double_stake_and_stranger_rejected() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    expect_err(env.stake(&a, &wager), "AlreadyFunded");

    // A funded stranger with their own ATA is still not a participant.
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();
    let ops = env.ops.insecure_clone();
    let s_ata = CreateAssociatedTokenAccount::new(&mut env.svm, &ops, &env.mint.clone())
        .owner(&stranger.pubkey())
        .send()
        .unwrap();
    env.mint_to(&s_ata, 100 * USDC);
    expect_err(env.stake(&stranger, &wager), "NotAParticipant");
}

#[test]
fn stake_after_active_rejected() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let a = env.a.insecure_clone();
    // The state check fires before the already-funded check, so this reports NotFunding.
    expect_err(env.stake(&a, &wager), "NotFunding");
}

#[test]
fn wallet_cap_enforced_and_released() {
    // Five funded stakes per wallet in this config; releasing one frees a slot.
    let mut env = setup();
    let a = env.a.insecure_clone();
    let mut wagers = vec![];
    for nonce in 1..=5 {
        let w = env.create(nonce, 5 * USDC);
        env.stake(&a, &w).expect("stake within cap");
        wagers.push(w);
    }
    let a_pk = a.pubkey();
    assert_eq!(env.counter_count(&a_pk), 5);

    // Sixth funded stake busts the cap.
    let w6 = env.create(6, 5 * USDC);
    expect_err(env.stake(&a, &w6), "WalletCapExceeded");

    // Unstake one → slot freed → the sixth stake lands.
    let ix = env.ix_unstake(&a.pubkey(), &wagers[0]);
    env.send(&[&a], &[ix]).expect("unstake");
    assert_eq!(env.counter_count(&a_pk), 4);
    env.stake(&a, &w6).expect("stake after release");
    assert_eq!(env.counter_count(&a_pk), 5);
}

#[test]
fn global_cap_enforced() {
    // The circuit-breaker counts funded value across every live wager, so creating them
    // costs nothing against it and only real money moves the number.
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let ix = env.ix_update_config(
        &admin.pubkey(),
        shake_escrow::instructions::UpdateConfigArgs {
            max_total_open: Some(60 * USDC),
            fee_bps: None,
            fee_destination: None,
            rent_collector: None,
            min_stake: None,
            max_stake: None,
            max_open_per_wallet: None,
            resolvers: None,
            paused: None,
            max_window: None,
            new_admin: None,
        },
    );
    env.send(&[&admin], &[ix]).expect("set global cap");

    let wager = env.create(1, 50 * USDC);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();
    env.stake(&a, &wager).expect("first 50 fits under 60");
    expect_err(env.stake(&b, &wager), "GlobalCapExceeded"); // 100 > 60

    // A releases → 0 open → B's 50 fits.
    let ix = env.ix_unstake(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("unstake");
    env.stake(&b, &wager).expect("stake under freed cap");
    assert_eq!(env.get_config().total_open_value, 50 * USDC);
}

#[test]
fn huge_stake_fee_math_safe() {
    // A pot near the top of the u64 range, driven through the u128 fee path. payout plus
    // fee must equal pot exactly, with nothing lost to rounding or truncation.
    let mut env = setup_with(|e| {
        let mut args = e.default_config_args();
        args.max_stake = u64::MAX / 4;
        args.fee_bps = 1_000; // 10%
        args
    });
    let big = 4_000_000_000_000_000_000u64; // 4e18
    let a_token = env.a_token;
    let b_token = env.b_token;
    env.mint_to(&a_token, big);
    env.mint_to(&b_token, big);

    let mut args = env.default_wager_args(1, big);
    args.stake = big;
    let ops = env.ops.insecure_clone();
    let ix = env.ix_create_wager(&ops.pubkey(), args);
    env.send(&[&ops], &[ix]).expect("create huge");
    let wager = wager_pda(&env.a.pubkey().clone(), 1);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    env.stake(&b, &wager).expect("stake B");

    let a_pk = a.pubkey();
    env.resolve_to(&wager, &a_pk).expect("resolve huge");
    let pot = big * 2;
    let fee = ((pot as u128) * 1_000 / 10_000) as u64;
    let fee_token = env.fee_token;
    assert_eq!(env.token_amount(&fee_token), fee);
    // A: initial 1,000 USDC + minted `big` − staked `big` + payout.
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC + (pot - fee));
    // Conservation: payout + fee == pot, to the base unit.
    assert_eq!((pot - fee) + fee, pot);
}
