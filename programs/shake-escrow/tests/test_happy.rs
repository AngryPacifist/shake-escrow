// Happy paths and money math: full lifecycles, fee arithmetic down to the base unit,
// per-side exits, and the close that follows them.
mod common;
use common::*;
use solana_signer::Signer;

#[test]
fn full_resolve_lifecycle_fees_and_close() {
    let mut env = setup();
    let ops_before = env.lamports(&env.ops.pubkey().clone());
    let wager = env.active_wager(1, 50 * USDC);
    let vault = ata_for(&wager, &env.mint.clone());
    assert_eq!(env.token_amount(&vault.clone()), 100 * USDC);
    {
        let w = env.get_wager(&wager);
        assert_eq!(w.state, shake_escrow::state::WagerState::Active);
    }
    let a_pk = env.a.pubkey();
    let b_pk = env.b.pubkey();
    assert_eq!(env.counter_count(&a_pk), 1);
    assert_eq!(env.counter_count(&b_pk), 1);
    assert_eq!(env.get_config().total_open_value, 100 * USDC);

    let meta = env.resolve_to(&wager, &a_pk).expect("resolve failed");
    println!("resolve CU: {}", meta.compute_units_consumed);

    // fee = 100 USDC * 300bps = 3 USDC; payout 97.
    let a_token = env.a_token;
    let fee_token = env.fee_token;
    assert_eq!(env.token_amount(&a_token), (1_000 - 50 + 97) * USDC);
    assert_eq!(env.token_amount(&fee_token), 3 * USDC);
    // Books released, accounts closed, rent home to the pinned collector.
    assert_eq!(env.counter_count(&a_pk), 0);
    assert_eq!(env.counter_count(&b_pk), 0);
    assert_eq!(env.get_config().total_open_value, 0);
    assert!(env.account_gone(&wager));
    assert!(env.account_gone(&vault));
    let ops_after = env.lamports(&env.ops.pubkey().clone());
    // The wager and vault rent came back at close. What the payer still floats is exactly
    // the two exposure counters, which have no close instruction by design, plus the
    // transaction fees it paid along the way.
    let counter_rent = 2 * env.counter_rent();
    let net = ops_before - ops_after;
    assert!(
        net >= counter_rent && net < counter_rent + 200_000,
        "ops net cost {} outside counter-float+fees band ({}..{})",
        net,
        counter_rent,
        counter_rent + 200_000
    );
}

#[test]
fn fee_dust_rides_with_winner() {
    // pot 66_666_666; fee floor(66_666_666 * 300 / 10_000) = 1_999_999, so the dust unit
    // stays with the winner rather than the fee account. payout = 64_666_667.
    let mut env = setup();
    let wager = env.active_wager(1, 33_333_333);
    let b_pk = env.b.pubkey();
    env.resolve_to(&wager, &b_pk).expect("resolve failed");
    let b_token = env.b_token;
    let fee_token = env.fee_token;
    assert_eq!(env.token_amount(&fee_token), 1_999_999);
    assert_eq!(
        env.token_amount(&b_token),
        1_000 * USDC - 33_333_333 + 64_666_667
    );
}

#[test]
fn per_side_refunds_after_resolve_deadline_then_close() {
    // Sides exit independently, close only happens once both are out, and rent goes to
    // the pinned collector.
    let mut env = setup();
    let wager = env.active_wager(1, 20 * USDC);
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);

    let a_pk = env.a.pubkey();
    let b_pk = env.b.pubkey();
    // Closing before both sides have exited must fail.
    expect_err(env.close_expired(&wager), "NotFullyExited");

    env.refund(&wager, &a_pk).expect("refund A");
    let a_token = env.a_token;
    let b_token = env.b_token;
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
    assert_eq!(env.token_amount(&b_token), 980 * USDC);
    assert_eq!(env.counter_count(&a_pk), 0);
    assert_eq!(env.counter_count(&b_pk), 1);
    // Still not fully exited.
    expect_err(env.close_expired(&wager), "NotFullyExited");

    env.refund(&wager, &b_pk).expect("refund B");
    assert_eq!(env.token_amount(&b_token), 1_000 * USDC);
    assert_eq!(env.get_config().total_open_value, 0);

    let ops_before = env.lamports(&env.ops.pubkey().clone());
    env.close_expired(&wager).expect("close");
    let ops_after = env.lamports(&env.ops.pubkey().clone());
    assert!(ops_after > ops_before, "rent did not return to ops");
    assert!(env.account_gone(&wager));
}

#[test]
fn single_funder_timeout_refund_and_close() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_fund + 1);
    let a_pk = env.a.pubkey();
    env.refund(&wager, &a_pk).expect("refund A");
    let a_token = env.a_token;
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
    env.close_expired(&wager).expect("close");
    assert!(env.account_gone(&wager));
}

#[test]
fn zero_funded_wager_closes_after_deadline() {
    // A wager nobody funded still has an exit. Without close_expired its rent would
    // strand, because per-side refunds have no funded side to serve.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    expect_err(env.close_expired(&wager), "CloseNotDue");
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_fund + 1);
    let ops_before = env.lamports(&env.ops.pubkey().clone());
    env.close_expired(&wager).expect("close");
    assert!(env.lamports(&env.ops.pubkey().clone()) > ops_before);
    assert!(env.account_gone(&wager));
}

#[test]
fn cancel_two_step_clear_then_accept() {
    // Propose, counterparty clears it, propose again, counterparty accepts.
    let mut env = setup();
    let wager = env.active_wager(1, 25 * USDC);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();

    let ix = env.ix_cancel_propose(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("propose");
    assert_eq!(env.get_wager(&wager).cancel_proposed_by, a.pubkey());

    // Proposer cannot accept their own proposal.
    let w = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&a.pubkey(), &wager, &w);
    expect_err(env.send(&[&a], &[ix]), "CannotAcceptOwnProposal");

    let ix = env.ix_cancel_clear(&b.pubkey(), &wager);
    env.send(&[&b], &[ix]).expect("clear");
    assert_eq!(
        env.get_wager(&wager).cancel_proposed_by,
        anchor_lang::prelude::Pubkey::default()
    );
    // Accept with no proposal open fails.
    let w = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&b.pubkey(), &wager, &w);
    expect_err(env.send(&[&b], &[ix]), "CancelNotOpen");

    let ix = env.ix_cancel_propose(&b.pubkey(), &wager);
    env.send(&[&b], &[ix]).expect("re-propose");
    let w = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&a.pubkey(), &wager, &w);
    let meta = env.send(&[&a], &[ix]).expect("accept");
    println!("cancel_accept CU: {}", meta.compute_units_consumed);

    let a_token = env.a_token;
    let b_token = env.b_token;
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
    assert_eq!(env.token_amount(&b_token), 1_000 * USDC);
    assert_eq!(env.get_config().total_open_value, 0);
    let a_pk = env.a.pubkey();
    let b_pk = env.b.pubkey();
    assert_eq!(env.counter_count(&a_pk), 0);
    assert_eq!(env.counter_count(&b_pk), 0);
    assert!(env.account_gone(&wager));
}

#[test]
fn unstake_prelock_then_restake_activates() {
    // A pre-lock exit is unilateral, and the books and flags return cleanly afterwards.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    let a_pk = a.pubkey();
    assert_eq!(env.counter_count(&a_pk), 1);

    let ix = env.ix_unstake(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("unstake A");
    let a_token = env.a_token;
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
    assert_eq!(env.counter_count(&a_pk), 0);
    assert_eq!(env.get_config().total_open_value, 0);
    let w = env.get_wager(&wager);
    assert!(!w.funded_a && w.state == shake_escrow::state::WagerState::Funding);

    // Unstaking again fails; a never-funded side cannot unstake.
    let ix = env.ix_unstake(&a.pubkey(), &wager);
    expect_err(env.send(&[&a], &[ix]), "SideNotFunded");

    env.stake(&b, &wager).expect("stake B");
    env.stake(&a, &wager).expect("restake A");
    assert_eq!(
        env.get_wager(&wager).state,
        shake_escrow::state::WagerState::Active
    );
    // Post-lock, unstake is dead (state Active).
    let ix = env.ix_unstake(&b.pubkey(), &wager);
    expect_err(env.send(&[&b], &[ix]), "NotFunding");
}

#[test]
fn donation_surplus_swept_at_close() {
    // Extra tokens sent into the vault never block the close; they sweep to the fee
    // account instead.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let vault = ata_for(&wager, &env.mint.clone());
    env.mint_to(&vault, 5 * USDC); // donation attack
    let a_pk = env.a.pubkey();
    env.resolve_to(&wager, &a_pk).expect("resolve");
    let fee_token = env.fee_token;
    // fee 0.6 USDC (20 USDC pot * 3%) + 5 USDC surplus
    assert_eq!(env.token_amount(&fee_token), 600_000 + 5 * USDC);
    assert!(env.account_gone(&wager));
}
