// Deadline boundary semantics at ±1 second, on both deadlines, in both directions.
// stake, unstake, resolve and cancel are valid at now == deadline; refund and close
// become valid at deadline + 1. The two sets never overlap.
mod common;
use common::*;
use solana_signer::Signer;

#[test]
fn stake_boundary_at_deadline_ok_after_fails() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();

    env.warp_to(w.deadline_fund); // exactly at the deadline: still open
    env.stake(&a, &wager).expect("stake at == deadline_fund must succeed");
    env.warp_to(w.deadline_fund + 1);
    expect_err(env.stake(&b, &wager), "FundingClosed");
}

#[test]
fn unstake_boundary_matches_stake() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake");
    let w = env.get_wager(&wager);

    env.warp_to(w.deadline_fund);
    let ix = env.ix_unstake(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("unstake at == deadline_fund must succeed");

    env.stake(&a, &wager).expect("restake at boundary");
    env.warp_to(w.deadline_fund + 1);
    let ix = env.ix_unstake(&a.pubkey(), &wager);
    expect_err(env.send(&[&a], &[ix]), "FundingClosed");
}

#[test]
fn funding_refund_boundary_exclusive() {
    // At == deadline_fund refund is NOT due (stake still could land); at +1 it is.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake");
    let w = env.get_wager(&wager);
    let a_pk = a.pubkey();

    env.warp_to(w.deadline_fund);
    expect_err(env.refund(&wager, &a_pk), "RefundNotDue");
    env.warp_to(w.deadline_fund + 1);
    env.refund(&wager, &a_pk).expect("refund at deadline+1");
}

#[test]
fn resolve_refund_boundary_exclusive() {
    // The race at the deadline: at == deadline_resolve only resolve is valid, at +1 only
    // refund. They are disjoint by clock comparison, so neither can steal the other.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a_pk = env.a.pubkey();

    env.warp_to(w.deadline_resolve);
    expect_err(env.refund(&wager, &a_pk), "RefundNotDue");
    env.resolve_to(&wager, &a_pk).expect("resolve at == deadline must succeed");

    // Fresh wager: past the deadline, resolve dead, refund alive.
    let wager2 = env.active_wager(2, 10 * USDC);
    let w2 = env.get_wager(&wager2);
    env.warp_to(w2.deadline_resolve + 1);
    expect_err(env.resolve_to(&wager2, &a_pk), "ResolveExpired");
    env.refund(&wager2, &a_pk).expect("refund at deadline+1");
}

#[test]
fn cancel_window_closes_with_resolve_window() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();

    env.warp_to(w.deadline_resolve);
    let ix = env.ix_cancel_propose(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("propose at == deadline ok");

    env.warp_to(w.deadline_resolve + 1);
    let w_state = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&b.pubkey(), &wager, &w_state);
    expect_err(env.send(&[&b], &[ix]), "CancelWindowClosed");
    // And a fresh propose is equally dead.
    let wager2 = env.active_wager(2, 10 * USDC);
    let w2 = env.get_wager(&wager2);
    env.warp_to(w2.deadline_resolve + 1);
    let ix = env.ix_cancel_propose(&a.pubkey(), &wager2);
    expect_err(env.send(&[&a], &[ix]), "CancelWindowClosed");
}
