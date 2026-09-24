// Behaviours that are easy to get wrong and easy to break later: atomicity when one
// side's account is frozen, the deliberate asymmetry between where a stake may come from
// and where an exit must go, and a vault ATA pre-created by someone else.
mod common;
use common::*;
use litesvm_token::CreateAccount as CreateTokenAccount;
use solana_signer::Signer;

#[test]
fn cancel_accept_atomic_under_frozen_side_reverts_whole() {
    // With one side's account frozen, cancel_accept must revert entirely. No half-refund,
    // the wager stays Active with the proposal still open, and the per-side deadline
    // refunds take over from there.
    let mut env = setup();
    let wager = env.active_wager(1, 15 * USDC);
    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();

    let ix = env.ix_cancel_propose(&a.pubkey(), &wager);
    env.send(&[&a], &[ix]).expect("propose");

    let a_token = env.a_token;
    env.freeze(&a_token);
    let w = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&b.pubkey(), &wager, &w);
    assert!(env.send(&[&b], &[ix]).is_err(), "accept with frozen side must revert");

    // Nothing half-happened: still Active, proposal still open, vault untouched.
    let w = env.get_wager(&wager);
    assert_eq!(w.state, shake_escrow::state::WagerState::Active);
    assert_eq!(w.cancel_proposed_by, a.pubkey());
    let vault = ata_for(&wager, &env.mint.clone());
    assert_eq!(env.token_amount(&vault), 30 * USDC);

    // Thaw → accept succeeds cleanly.
    env.thaw(&a_token);
    let w = env.get_wager(&wager);
    let ix = env.ix_cancel_accept(&b.pubkey(), &wager, &w);
    env.send(&[&b], &[ix]).expect("accept after thaw");
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC);
    assert!(env.account_gone(&wager));
}

#[test]
fn stake_from_non_canonical_owned_account_is_allowed_by_design() {
    // A deliberate asymmetry: a stake may come from any token account the staker owns,
    // while every exit pays the canonical ATA. Pinned by a test so that changing it later
    // has to be a decision rather than an accident.
    let mut env = setup();
    let wager = env.create(1, 10 * USDC);
    let a = env.a.insecure_clone();
    let ops = env.ops.insecure_clone();
    let mint = env.mint;

    // A funds an auxiliary (non-ATA) token account and stakes from it.
    let aux = CreateTokenAccount::new(&mut env.svm, &ops, &mint)
        .owner(&a.pubkey())
        .send()
        .unwrap();
    let a_token = env.a_token;
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: token_program_id(),
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(a_token, false),
            anchor_lang::solana_program::instruction::AccountMeta::new(aux, false),
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(a.pubkey(), true),
        ],
        data: [vec![3u8], (10 * USDC).to_le_bytes().to_vec()].concat(),
    };
    env.send(&[&a], &[ix]).expect("fund aux");

    let ix = env.ix_stake_with(
        &a.pubkey(),
        &ops.pubkey(),
        &wager,
        &aux,
        &ata_for(&wager, &mint),
        &counter_pda(&a.pubkey()),
    );
    env.send(&[&ops, &a], &[ix]).expect("stake from owned non-ATA account");

    // The exit pays the canonical ATA. The auxiliary account it was staked from ends
    // empty.
    let w = env.get_wager(&wager);
    env.warp_to(w.deadline_fund + 1);
    let a_pk = a.pubkey();
    env.refund(&wager, &a_pk).expect("refund");
    assert_eq!(env.token_amount(&a_token), 1_000 * USDC); // 990 kept + 10 refunded
    assert_eq!(env.token_amount(&aux), 0);
}

#[test]
fn pre_created_vault_ata_cannot_block_create() {
    // Someone pre-creates the derivable vault ATA to block the wager. Create must proceed
    // anyway, and anything they donated into it sweeps at close like any other surplus.
    let mut env = setup();
    let args = env.default_wager_args(1, 10 * USDC);
    let wager = wager_pda(&args.side_a, args.nonce);
    let griefer = solana_keypair::Keypair::new();
    env.svm.airdrop(&griefer.pubkey(), 10_000_000_000).unwrap();
    let griefer_kp = griefer.insecure_clone();
    litesvm_token::CreateAssociatedTokenAccount::new(&mut env.svm, &griefer_kp, &env.mint.clone())
        .owner(&wager)
        .send()
        .unwrap();
    let vault = ata_for(&wager, &env.mint.clone());
    env.mint_to(&vault, 2 * USDC); // and donates into it, to make the close harder too

    let ops = env.ops.insecure_clone();
    let ix = env.ix_create_wager(&ops.pubkey(), args);
    env.send(&[&ops], &[ix]).expect("create over pre-created vault");

    let a = env.a.insecure_clone();
    let b = env.b.insecure_clone();
    env.stake(&a, &wager).expect("stake A");
    env.stake(&b, &wager).expect("stake B");
    let a_pk = a.pubkey();
    let fee_before = {
        let f = env.fee_token;
        env.token_amount(&f)
    };
    env.resolve_to(&wager, &a_pk).expect("resolve");
    let fee_token = env.fee_token;
    // 0.6 USDC fee plus the 2 USDC that was donated into the vault, swept together.
    assert_eq!(env.token_amount(&fee_token) - fee_before, 600_000 + 2 * USDC);
    assert!(env.account_gone(&wager));
}

#[test]
fn default_address_config_rejected() {
    // Default-pubkey destinations are refused at both init and update, so an operator
    // cannot quietly configure a wager that pays into the void.
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let mut args = shake_escrow::instructions::UpdateConfigArgs {
        fee_bps: None,
        fee_destination: Some(anchor_lang::prelude::Pubkey::default()),
        rent_collector: None,
        min_stake: None,
        max_stake: None,
        max_open_per_wallet: None,
        resolvers: None,
        paused: None,
        max_window: None,
        max_total_open: None,
    };
    let ix = env.ix_update_config(&admin.pubkey(), args.clone());
    expect_err(env.send(&[&admin], &[ix]), "DefaultAddress");
    args.fee_destination = None;
    args.rent_collector = Some(anchor_lang::prelude::Pubkey::default());
    let ix = env.ix_update_config(&admin.pubkey(), args);
    expect_err(env.send(&[&admin], &[ix]), "DefaultAddress");
}
