// Concession: a participant gives a locked wager to the other side. The payout, fee and
// destinations must match resolve exactly, the window must match resolve's, and nothing the
// conceder supplies may send the pot outside the wager.
mod common;
use anchor_lang::Space;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn wager_rent(env: &mut Env) -> u64 {
    env.min_rent(8 + shake_escrow::state::Wager::INIT_SPACE)
}

#[test]
fn a_concession_pays_the_counterparty_as_resolve_would() {
    let mut env = setup();
    let wager = env.active_wager(1, 50 * USDC);
    let vault = ata_for(&wager, &env.mint.clone());
    let ops = env.ops.pubkey();
    let ops_before = env.lamports(&ops);
    let rent_back = wager_rent(&mut env) + env.token_account_rent();

    let a = env.a.insecure_clone();
    let meta = env.concede(&a, &wager).expect("A concedes");
    println!("concede CU: {}", meta.compute_units_consumed);

    // Pot 100, fee 3% = 3, B nets 97 on a 50 stake.
    let (a_token, b_token, fee_token) = (env.a_token, env.b_token, env.fee_token);
    assert_eq!(env.token_amount(&b_token), (1_000 - 50 + 97) * USDC);
    assert_eq!(env.token_amount(&a_token), (1_000 - 50) * USDC);
    assert_eq!(env.token_amount(&fee_token), 3 * USDC);
    let (a_pk, b_pk) = (env.a.pubkey(), env.b.pubkey());
    assert_eq!(env.counter_count(&a_pk), 0);
    assert_eq!(env.counter_count(&b_pk), 0);
    assert_eq!(env.get_config().total_open_value, 0);
    assert!(env.account_gone(&wager));
    assert!(env.account_gone(&vault));
    // The conceder paid the transaction fee; the wager and vault rent went to the collector.
    assert_eq!(env.lamports(&ops), ops_before + rent_back);
}

#[test]
fn side_b_conceding_pays_side_a() {
    let mut env = setup();
    let wager = env.active_wager(1, 20 * USDC);
    let b = env.b.insecure_clone();
    env.concede(&b, &wager).expect("B concedes");
    let (a_token, b_token, fee_token) = (env.a_token, env.b_token, env.fee_token);
    assert_eq!(env.token_amount(&a_token), (1_000 - 20) * USDC + 38_800_000);
    assert_eq!(env.token_amount(&b_token), (1_000 - 20) * USDC);
    assert_eq!(env.token_amount(&fee_token), 1_200_000);
}

#[test]
fn fee_dust_rides_with_the_winner() {
    // Pot 66,666,666 at 300 bps: fee 1,999,999, winner 64,666,667.
    let mut env = setup();
    let wager = env.active_wager(1, 33_333_333);
    let b = env.b.insecure_clone();
    env.concede(&b, &wager).expect("B concedes");
    let (a_token, fee_token) = (env.a_token, env.fee_token);
    assert_eq!(env.token_amount(&fee_token), 1_999_999);
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC - 33_333_333 + 64_666_667);
}

#[test]
fn a_stranger_cannot_concede() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let b_pk = env.b.pubkey();
    let ix = env.ix_concede(&stranger.pubkey(), &wager, &w, &b_pk);
    expect_err(env.send(&[&stranger], &[ix]), "NotAParticipant");
}

#[test]
fn a_concession_needs_the_conceders_signature() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let (a_pk, b_pk) = (env.a.pubkey(), env.b.pubkey());
    let payer = env.ops.insecure_clone();
    let mut ix = env.ix_concede(&a_pk, &wager, &w, &b_pk);
    ix.accounts[0].is_signer = false;
    assert!(env.send(&[&payer], &[ix]).is_err(), "unsigned concession must fail");
    assert_eq!(env.get_wager(&wager).state, shake_escrow::state::WagerState::Active);
}

#[test]
fn the_winner_must_be_the_counterparty() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a = env.a.insecure_clone();

    // Naming themselves: their own ATA passes the account constraint.
    let ix = env.ix_concede(&a.pubkey(), &wager, &w, &a.pubkey());
    expect_err(env.send(&[&a], &[ix]), "NotCounterparty");

    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    env.create_ata(&stranger.pubkey());
    let ix = env.ix_concede(&a.pubkey(), &wager, &w, &stranger.pubkey());
    expect_err(env.send(&[&a], &[ix]), "NotCounterparty");

    env.concede(&a, &wager).expect("canonical concession");
}

#[test]
fn a_wager_still_funding_cannot_be_conceded() {
    // Anchor validates both exposure counters before the handler runs, so a second, fully
    // staked wager creates them and the state check is what fires.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    env.active_wager(2, 5 * USDC);
    expect_err(env.concede(&a, &wager), "NotActive");
}

#[test]
fn concession_window_matches_resolve() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a = env.a.insecure_clone();
    env.warp_to(w.deadline_resolve);
    env.concede(&a, &wager).expect("concede at == deadline_resolve");

    let wager2 = env.active_wager(2, 10 * USDC);
    let w2 = env.get_wager(&wager2);
    env.warp_to(w2.deadline_resolve + 1);
    expect_err(env.concede(&a, &wager2), "ResolveExpired");
    let a_pk = a.pubkey();
    env.refund(&wager2, &a_pk).expect("refund owns the space after the deadline");
}

#[test]
fn a_frozen_winner_account_leaves_the_refunds_open() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let b_token = env.b_token;
    env.freeze(&b_token);
    let a = env.a.insecure_clone();
    assert!(env.concede(&a, &wager).is_err(), "payout into a frozen account must fail");

    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);
    let a_pk = a.pubkey();
    env.refund(&wager, &a_pk).expect("A refunds");
    env.thaw(&b_token);
    let b_pk = env.b.pubkey();
    env.refund(&wager, &b_pk).expect("B refunds after thaw");
    let a_token = env.a_token;
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
}

#[test]
fn substituted_destinations_are_refused() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a = env.a.insecure_clone();
    let (a_pk, b_pk) = (a.pubkey(), env.b.pubkey());
    let mint = env.mint;
    let (fee, ops) = (env.fee_token, env.ops.pubkey());
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let attacker_ata = env.create_ata(&attacker.pubkey());

    // Anchor's associated-token constraint rejects this before the handler runs, with its own
    // error rather than one of the program's, so only the failure is asserted.
    let ix = env.ix_concede_with(&a_pk, &wager, &w, &b_pk, &ata_for(&a_pk, &mint), &fee, &ops);
    assert!(env.send(&[&a], &[ix]).is_err(), "winner_token mismatch must fail");

    let b_ata = ata_for(&b_pk, &mint);
    let ix = env.ix_concede_with(&a_pk, &wager, &w, &b_pk, &b_ata, &attacker_ata, &ops);
    expect_err(env.send(&[&a], &[ix]), "WrongFeeDestination");

    let ix = env.ix_concede_with(&a_pk, &wager, &w, &b_pk, &b_ata, &fee, &attacker.pubkey());
    expect_err(env.send(&[&a], &[ix]), "WrongRentCollector");

    env.concede(&a, &wager).expect("canonical concession");
}

#[test]
fn a_settled_wager_cannot_be_settled_again() {
    let mut env = setup();
    let a = env.a.insecure_clone();
    let (a_pk, b_pk) = (a.pubkey(), env.b.pubkey());

    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    env.concede(&a, &wager).expect("concede");
    let ix = env.ix_concede(&a_pk, &wager, &w, &b_pk);
    assert!(env.send(&[&a], &[ix]).is_err(), "a second concession must fail");
    let resolver = env.resolver.insecure_clone();
    let ix = env.ix_resolve(&wager, &w, &b_pk);
    assert!(env.send(&[&resolver], &[ix]).is_err(), "resolve after a concession must fail");

    let wager2 = env.active_wager(2, 10 * USDC);
    let w2 = env.get_wager(&wager2);
    env.resolve_to(&wager2, &a_pk).expect("resolve");
    let ix = env.ix_concede(&a_pk, &wager2, &w2, &b_pk);
    assert!(env.send(&[&a], &[ix]).is_err(), "a concession after resolve must fail");
}

#[test]
fn an_open_cancel_proposal_and_a_pause_do_not_block_a_concession() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();
    let ix = env.ix_cancel_propose(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("propose cancel");

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

    env.concede(&b, &wager).expect("B concedes while paused with a cancel proposal open");
    assert!(env.account_gone(&wager));
}

#[test]
fn a_donation_into_the_vault_sweeps_to_the_fee_account() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let vault = ata_for(&wager, &env.mint.clone());
    env.mint_to(&vault, 5 * USDC);
    let a = env.a.insecure_clone();
    env.concede(&a, &wager).expect("concede");
    let fee_token = env.fee_token;
    // Fee 0.6 on a 20 pot, plus the 5 donated.
    assert_eq!(env.token_amount(&fee_token), 600_000 + 5 * USDC);
    assert!(env.account_gone(&vault));
}
