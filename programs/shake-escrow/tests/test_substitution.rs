// The constraint contract: every account swapped for a plausible imposter must fail.
// Vaults, payout destinations, exposure counters, the config PDA, and the staker's own
// signature.
mod common;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

#[test]
fn fake_and_foreign_vaults_rejected() {
    // Only the ATA derived from this wager and this mint is ever accepted as the vault.
    let mut env = setup();
    let wager1 = env.create(1, 10 * USDC);
    let wager2 = env.create(2, 10 * USDC);
    let a = env.a.insecure_clone();
    let ops = env.ops.insecure_clone();
    let mint = env.mint;

    // Foreign wager's vault.
    let ix = env.ix_stake_with(
        &a.pubkey(),
        &ops.pubkey(),
        &wager1,
        &ata_for(&a.pubkey(), &mint),
        &ata_for(&wager2, &mint),
        &counter_pda(&a.pubkey()),
    );
    assert!(env.send(&[&ops, &a], &[ix]).is_err(), "foreign vault must fail");

    // An arbitrary attacker-owned token account as the vault.
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let attacker_ata = env.create_ata(&attacker.pubkey());
    let ix = env.ix_stake_with(
        &a.pubkey(),
        &ops.pubkey(),
        &wager1,
        &ata_for(&a.pubkey(), &mint),
        &attacker_ata,
        &counter_pda(&a.pubkey()),
    );
    assert!(env.send(&[&ops, &a], &[ix]).is_err(), "attacker vault must fail");
}

#[test]
fn refund_destination_substitutions_rejected() {
    // Refunds pay the side's canonical ATA and nothing else, even an account that side
    // owns.
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_resolve + 1);

    let a_pk = env.a.pubkey();
    let b_token = env.b_token;
    let cranker = Keypair::new();
    env.svm.airdrop(&cranker.pubkey(), 1_000_000_000).unwrap();

    // Someone else's ATA as A's refund target.
    let ix = env.ix_refund_with(&cranker.pubkey(), &wager, &a_pk, &b_token, &counter_pda(&a_pk));
    assert!(env.send(&[&cranker], &[ix]).is_err(), "foreign ATA refund must fail");

    // A non-ATA token account OWNED by A still fails the canonical-ATA constraint.
    let aux = Keypair::new();
    let rent = env.token_account_rent();
    let create = anchor_lang::solana_program::system_instruction::create_account(
        &cranker.pubkey(),
        &aux.pubkey(),
        rent,
        165,
        &token_program_id(),
    );
    let init = anchor_lang::solana_program::instruction::Instruction {
        program_id: token_program_id(),
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(aux.pubkey(), false),
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(env.mint, false),
        ],
        data: [vec![18u8], a_pk.to_bytes().to_vec()].concat(), // InitializeAccount3
    };
    env.send(&[&cranker, &aux], &[create, init]).expect("aux token account");
    let ix = env.ix_refund_with(&cranker.pubkey(), &wager, &a_pk, &aux.pubkey(), &counter_pda(&a_pk));
    assert!(env.send(&[&cranker], &[ix]).is_err(), "non-canonical account must fail");

    // Wrong counter PDA (B's) with A's refund.
    let b_pk = env.b.pubkey();
    let a_ata = ata_for(&a_pk, &env.mint.clone());
    let ix = env.ix_refund_with(&cranker.pubkey(), &wager, &a_pk, &a_ata, &counter_pda(&b_pk));
    assert!(env.send(&[&cranker], &[ix]).is_err(), "foreign counter must fail");

    // The canonical call still works after all the failed attempts.
    env.refund(&wager, &a_pk).expect("canonical refund");
}

#[test]
fn resolve_destination_substitutions_rejected() {
    let mut env = setup();
    let wager = env.active_wager(1, 10 * USDC);
    let w = env.get_wager(&wager);
    let a_pk = env.a.pubkey();
    let resolver = env.resolver.insecure_clone();

    // Winner arg says A but winner_token is B's ATA.
    let b_token = env.b_token;
    let ops = env.ops.pubkey();
    let fee = env.fee_token;
    let ix = env.ix_resolve_with(&wager, &w, &a_pk, &b_token, &fee, &ops);
    assert!(env.send(&[&resolver], &[ix]).is_err(), "winner_token mismatch must fail");

    // Fee token swapped for an attacker account.
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let attacker_ata = env.create_ata(&attacker.pubkey());
    let a_ata = ata_for(&a_pk, &env.mint.clone());
    let ix = env.ix_resolve_with(&wager, &w, &a_pk, &a_ata, &attacker_ata, &ops);
    expect_err(env.send(&[&resolver], &[ix]), "WrongFeeDestination");

    // Rent collector swapped for the attacker's own account.
    let ix = env.ix_resolve_with(&wager, &w, &a_pk, &a_ata, &fee, &attacker.pubkey());
    expect_err(env.send(&[&resolver], &[ix]), "WrongRentCollector");

    // Canonical resolve still fine.
    env.resolve_to(&wager, &a_pk).expect("canonical resolve");
}

#[test]
fn close_rent_collector_substitution_rejected() {
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_fund + 1);
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let ix = env.ix_close_expired(&attacker.pubkey(), &wager, None, &attacker.pubkey());
    expect_err(env.send(&[&attacker], &[ix]), "WrongRentCollector");
    env.close_expired(&wager).expect("canonical close");
}

#[test]
fn staker_token_and_vault_duplication_rejected() {
    // Aliasing staker_token to the vault, so a "transfer" would be a self-transfer.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    let ops = env.ops.insecure_clone();
    let vault = ata_for(&wager, &env.mint.clone());
    let ix = env.ix_stake_with(
        &a.pubkey(),
        &ops.pubkey(),
        &wager,
        &vault,
        &vault,
        &counter_pda(&a.pubkey()),
    );
    // Anchor's built-in duplicate-mutable-account guard (error 2040) fires before the
    // owner constraint even runs. The aliasing is rejected either way.
    assert!(env.send(&[&ops, &a], &[ix]).is_err(), "vault aliasing must fail");
}

#[test]
fn config_pda_cannot_be_substituted() {
    // config is seeds-pinned on every instruction, so a look-alike account fails address
    // derivation before any handler code runs.
    let mut env = setup();
    let a = env.a.insecure_clone();
    let ops = env.ops.insecure_clone();
    let wager = env.create(1, 10 * USDC);
    let mut ix = env.ix_stake(&a.pubkey(), &ops.pubkey(), &wager);
    // accounts[2] is config in StakeSide's ordering.
    let fake = Keypair::new().pubkey();
    ix.accounts[2].pubkey = fake;
    assert!(env.send(&[&ops, &a], &[ix]).is_err(), "fake config must fail");
}

#[test]
fn stranger_cannot_sign_someone_elses_stake() {
    // The transfer authority is the staker's own signature — nobody else's.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let ops = env.ops.insecure_clone();
    let a_pk = env.a.pubkey();
    let mint = env.mint;
    // Attacker tries to push A's funds into the vault by signing as themselves.
    let mut ix = env.ix_stake_with(
        &a_pk,
        &ops.pubkey(),
        &wager,
        &ata_for(&a_pk, &mint),
        &ata_for(&wager, &mint),
        &counter_pda(&a_pk),
    );
    ix.accounts[0].pubkey = attacker.pubkey(); // staker field now attacker...
    let res = env.send(&[&ops, &attacker], &[ix]);
    // ...which makes them NotAParticipant; and leaving staker=A without A's signature
    // is unsignable at the transaction layer. Either way: no path.
    assert!(res.is_err());
}
