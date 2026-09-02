// Resolver powers and their exact bounds, and what frozen or closed token accounts can
// and cannot do to an exit.
mod common;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

#[test]
fn winner_outside_participants_rejected() {
    // The core invariant: even the true resolver cannot send the pot to a stranger.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    env.create_ata(&stranger.pubkey());
    expect_err(
        env.resolve_to(&wager, &stranger.pubkey()),
        "WinnerNotParticipant",
    );
    // The resolver key itself is not a valid winner either.
    let r_pk = env.resolver.pubkey();
    env.create_ata(&r_pk);
    expect_err(env.resolve_to(&wager, &r_pk), "WinnerNotParticipant");
}

#[test]
fn non_resolver_cannot_resolve() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a_pk = env.a.pubkey();
    let impostor = Keypair::new();
    env.svm.airdrop(&impostor.pubkey(), 1_000_000_000).unwrap();
    let mut ix = env.ix_resolve(&wager, &w, &a_pk);
    // Swap the resolver account+signature for the impostor's.
    ix.accounts[0].pubkey = impostor.pubkey();
    expect_err(env.send(&[&impostor], &[ix]), "NotResolver");
}

#[test]
fn resolve_requires_active() {
    // While a wager is still Funding, the resolver has no power over it. Anchor validates
    // the account struct before the handler runs, so both participants' exposure counters
    // must already exist for the instruction to reach the state check at all; a second,
    // fully staked wager creates them.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    env.active_wager(2, 5 * USDC);

    let a_pk = a.pubkey();
    let w = env.get_wager(&wager);
    let resolver = env.resolver.insecure_clone();
    let ix = env.ix_resolve(&wager, &w, &a_pk);
    expect_err(env.send(&[&resolver], &[ix]), "NotActive");
}

#[test]
fn double_resolve_and_refund_after_resolve_die_on_missing_account() {
    // Terminal states close their accounts, so a replayed instruction has nothing left to
    // act on and fails structurally rather than on a flag.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let a_pk = env.a.pubkey();
    let w = env.get_wager(&wager); // cache before the account closes
    env.resolve_to(&wager, &a_pk).expect("resolve");
    assert!(env.account_gone(&wager));
    // Replay the same resolve against the now-closed account: structural failure.
    let resolver = env.resolver.insecure_clone();
    let ix = env.ix_resolve(&wager, &w, &a_pk);
    assert!(env.send(&[&resolver], &[ix]).is_err(), "second resolve must fail");
    assert!(env.refund(&wager, &a_pk).is_err(), "refund after resolve must fail");
}

#[test]
fn frozen_winner_ata_fails_resolve_then_sides_exit_independently() {
    // A frozen winner account can only ever delay the winner's own money. The other side
    // exits on schedule regardless.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let a_token = env.a_token;
    env.freeze(&a_token);

    let a_pk = env.a.pubkey();
    let b_pk = env.b.pubkey();
    assert!(env.resolve_to(&wager, &a_pk).is_err(), "resolve into frozen ATA must fail");

    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);
    // B (healthy) exits immediately; A (frozen) cannot — yet.
    env.refund(&wager, &b_pk).expect("healthy side exits");
    assert!(env.refund(&wager, &a_pk).is_err(), "frozen side waits");
    expect_err(env.close_expired(&wager), "NotFullyExited");

    env.thaw(&a_token);
    env.refund(&wager, &a_pk).expect("thawed side exits");
    env.close_expired(&wager).expect("close");
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
}

#[test]
fn closed_side_ata_recreated_then_refund_lands() {
    // A side that closed its token account can re-create it and crank the refund again.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);

    let a = env.a.insecure_clone();
    let a_pk = a.pubkey();
    let a_token = env.a_token;
    // A must empty the account before closing it. The stake took 10, so 990 remains.
    let stash = Keypair::new();
    env.svm.airdrop(&stash.pubkey(), 1_000_000_000).unwrap();
    let stash_ata = env.create_ata(&stash.pubkey());
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: token_program_id(),
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(a_token, false),
            anchor_lang::solana_program::instruction::AccountMeta::new(stash_ata, false),
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(a_pk, true),
        ],
        data: [vec![3u8], (990 * USDC).to_le_bytes().to_vec()].concat(), // Transfer
    };
    env.send(&[&a], &[ix]).expect("empty A ata");
    env.close_token_account(&a_token, &a);
    assert!(env.refund(&wager, &a_pk).is_err(), "refund into missing ATA fails");

    env.create_ata(&a_pk);
    env.refund(&wager, &a_pk).expect("refund after recreation");
    assert_eq!(env.token_amount(&a_token), 10 * USDC);
}

#[test]
fn frozen_vault_blocks_until_thaw() {
    // The mint's freeze authority freezing the vault itself stalls the funds without
    // losing them. They stay custodied by the program until it thaws, and no alternative
    // destination exists by design.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let vault = ata_for(&wager, &env.mint.clone());
    env.freeze(&vault);

    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);
    let a_pk = env.a.pubkey();
    assert!(env.refund(&wager, &a_pk).is_err(), "frozen vault blocks refunds");
    env.thaw(&vault);
    env.refund(&wager, &a_pk).expect("refund after thaw");
}

#[test]
fn frozen_fee_ata_blocks_resolve_refund_path_stays_open() {
    // A broken fee account can never strand user funds. It costs the operator the fee and
    // nothing else, because the refund path stays open throughout.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let fee_token = env.fee_token;
    env.freeze(&fee_token);

    let a_pk = env.a.pubkey();
    let b_pk = env.b.pubkey();
    assert!(env.resolve_to(&wager, &a_pk).is_err(), "resolve with frozen fee ATA fails");

    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);
    env.refund(&wager, &a_pk).expect("refund A");
    env.refund(&wager, &b_pk).expect("refund B");
    // With no surplus to sweep, the close needs no fee account at all, so a frozen one
    // cannot hold the rent hostage.
    let cranker = Keypair::new();
    env.svm.airdrop(&cranker.pubkey(), 1_000_000_000).unwrap();
    let ops = env.ops.pubkey();
    let ix = env.ix_close_expired(&cranker.pubkey(), &wager, None, &ops);
    env.send(&[&cranker], &[ix]).expect("close without fee account");
    assert!(env.account_gone(&wager));
}
