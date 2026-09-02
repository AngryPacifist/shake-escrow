// Shared test rig. Every helper is deliberately explicit about which accounts it passes,
// so a substitution test can override any single one of them.
#![allow(dead_code)]

use {
    anchor_lang::{
        prelude::Pubkey,
        solana_program::{clock::Clock, instruction::Instruction, rent::Rent, system_program},
        AccountDeserialize, InstructionData, Space, ToAccountMetas,
    },
    litesvm::{
        types::{FailedTransactionMetadata, TransactionMetadata},
        LiteSVM,
    },
    litesvm_token::{CreateAssociatedTokenAccount, CreateMint, MintTo},
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::str::FromStr,
};

pub const USDC: u64 = 1_000_000;
pub const DAY: i64 = 86_400;

pub fn token_program_id() -> Pubkey {
    Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
}
pub fn ata_program_id() -> Pubkey {
    Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap()
}
pub fn upgradeable_loader_id() -> Pubkey {
    Pubkey::from_str("BPFLoaderUpgradeab1e11111111111111111111111").unwrap()
}

pub fn ata_for(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program_id().as_ref(), mint.as_ref()],
        &ata_program_id(),
    )
    .0
}

pub fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[shake_escrow::constants::CONFIG_SEED], &shake_escrow::id()).0
}
pub fn wager_pda(side_a: &Pubkey, nonce: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            shake_escrow::constants::WAGER_SEED,
            side_a.as_ref(),
            &nonce.to_le_bytes(),
        ],
        &shake_escrow::id(),
    )
    .0
}
pub fn counter_pda(wallet: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[shake_escrow::constants::COUNTER_SEED, wallet.as_ref()],
        &shake_escrow::id(),
    )
    .0
}
pub fn event_authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &shake_escrow::id()).0
}

pub struct Env {
    pub svm: LiteSVM,
    /// Upgrade authority of the deployed program; becomes config admin.
    pub admin: Keypair,
    /// The operator key: rent payer for wagers and counters, and the rent collector.
    pub ops: Keypair,
    /// Owner of the fee token account.
    pub agent: Keypair,
    pub resolver: Keypair,
    pub a: Keypair,
    pub b: Keypair,
    pub mint: Pubkey,
    pub fee_token: Pubkey,
    pub a_token: Pubkey,
    pub b_token: Pubkey,
}

pub type TxResult = Result<TransactionMetadata, FailedTransactionMetadata>;

impl Env {
    pub fn send(&mut self, signers: &[&Keypair], ixs: &[Instruction]) -> TxResult {
        // A fresh blockhash per send: identical retries would otherwise collide with
        // LiteSVM's duplicate-signature guard (AlreadyProcessed).
        self.svm.expire_blockhash();
        let payer = signers[0];
        let blockhash = self.svm.latest_blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
            .unwrap();
        self.svm.send_transaction(tx)
    }

    pub fn now(&mut self) -> i64 {
        let c: Clock = self.svm.get_sysvar();
        c.unix_timestamp
    }
    pub fn warp_to(&mut self, ts: i64) {
        let mut c: Clock = self.svm.get_sysvar();
        c.unix_timestamp = ts;
        self.svm.set_sysvar(&c);
        self.svm.expire_blockhash();
    }
    pub fn warp_by(&mut self, secs: i64) {
        let now = self.now();
        self.warp_to(now + secs);
    }

    pub fn token_amount(&mut self, ata: &Pubkey) -> u64 {
        let acc = self.svm.get_account(ata).expect("token account missing");
        u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
    }
    /// Rent-exempt minimum for an account of `len` bytes, read from the rig's own Rent
    /// sysvar. Tests derive their expectations from this rather than pasting a figure
    /// that would silently go stale if the rent parameters ever changed.
    pub fn min_rent(&mut self, len: usize) -> u64 {
        let r: Rent = self.svm.get_sysvar();
        r.minimum_balance(len)
    }
    /// Rent floated by one exposure counter. Counters are never closed, by design, so
    /// this is the standing cost of a wallet's first stake.
    pub fn counter_rent(&mut self) -> u64 {
        self.min_rent(8 + shake_escrow::state::ExposureCounter::INIT_SPACE)
    }
    /// Rent for one SPL token account (fixed 165-byte layout).
    pub fn token_account_rent(&mut self) -> u64 {
        self.min_rent(165)
    }
    pub fn lamports(&mut self, key: &Pubkey) -> u64 {
        self.svm.get_account(key).map(|a| a.lamports).unwrap_or(0)
    }
    pub fn account_gone(&mut self, key: &Pubkey) -> bool {
        match self.svm.get_account(key) {
            None => true,
            Some(a) => a.data.is_empty() && a.lamports == 0,
        }
    }
    pub fn get_wager(&mut self, wager: &Pubkey) -> shake_escrow::state::Wager {
        let acc = self.svm.get_account(wager).expect("wager missing");
        let mut d: &[u8] = &acc.data;
        shake_escrow::state::Wager::try_deserialize(&mut d).unwrap()
    }
    pub fn get_config(&mut self) -> shake_escrow::state::Config {
        let acc = self.svm.get_account(&config_pda()).expect("config missing");
        let mut d: &[u8] = &acc.data;
        shake_escrow::state::Config::try_deserialize(&mut d).unwrap()
    }
    pub fn counter_count(&mut self, wallet: &Pubkey) -> u8 {
        match self.svm.get_account(&counter_pda(wallet)) {
            None => 0,
            Some(a) if a.data.is_empty() => 0,
            Some(a) => {
                let mut d: &[u8] = &a.data;
                shake_escrow::state::ExposureCounter::try_deserialize(&mut d)
                    .map(|c| c.count)
                    .unwrap_or(0)
            }
        }
    }

    // --- SPL raw helpers (single-byte-tag token instructions; layouts are frozen) ---
    fn token_auth_ix(&self, tag: u8, target: &Pubkey, auth: &Pubkey) -> Instruction {
        Instruction {
            program_id: token_program_id(),
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(*target, false),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    self.mint, false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(*auth, true),
            ],
            data: vec![tag],
        }
    }
    pub fn freeze(&mut self, ata: &Pubkey) {
        let admin = self.admin.insecure_clone();
        let ix = self.token_auth_ix(10, ata, &admin.pubkey());
        self.send(&[&admin], &[ix]).expect("freeze failed");
    }
    pub fn thaw(&mut self, ata: &Pubkey) {
        let admin = self.admin.insecure_clone();
        let ix = self.token_auth_ix(11, ata, &admin.pubkey());
        self.send(&[&admin], &[ix]).expect("thaw failed");
    }
    /// Close a wallet's own (empty) token account: tag 9 = CloseAccount.
    pub fn close_token_account(&mut self, ata: &Pubkey, owner: &Keypair) {
        let ix = Instruction {
            program_id: token_program_id(),
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(*ata, false),
                anchor_lang::solana_program::instruction::AccountMeta::new(owner.pubkey(), false),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    owner.pubkey(),
                    true,
                ),
            ],
            data: vec![9],
        };
        let owner = owner.insecure_clone();
        self.send(&[&owner], &[ix]).expect("close token acct failed");
    }
    pub fn create_ata(&mut self, owner: &Pubkey) -> Pubkey {
        let payer = self.ops.insecure_clone();
        CreateAssociatedTokenAccount::new(&mut self.svm, &payer, &self.mint)
            .owner(owner)
            .send()
            .unwrap()
    }
    pub fn mint_to(&mut self, ata: &Pubkey, amount: u64) {
        let admin = self.admin.insecure_clone();
        MintTo::new(&mut self.svm, &admin, &self.mint, ata, amount)
            .send()
            .unwrap();
    }

    // --- instruction builders (canonical accounts; override via the *_with variants) ---

    pub fn ix_initialize_config(
        &self,
        authority: &Pubkey,
        args: shake_escrow::instructions::InitializeConfigArgs,
    ) -> Instruction {
        let program_id = shake_escrow::id();
        let programdata =
            Pubkey::find_program_address(&[program_id.as_ref()], &upgradeable_loader_id()).0;
        Instruction::new_with_bytes(
            program_id,
            &shake_escrow::instruction::InitializeConfig { args }.data(),
            shake_escrow::accounts::InitializeConfig {
                authority: *authority,
                config: config_pda(),
                program: program_id,
                program_data: programdata,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        )
    }

    pub fn default_config_args(&self) -> shake_escrow::instructions::InitializeConfigArgs {
        shake_escrow::instructions::InitializeConfigArgs {
            mint: self.mint,
            fee_bps: 300,
            fee_destination: self.fee_token,
            rent_collector: self.ops.pubkey(),
            min_stake: USDC,
            max_stake: 100 * USDC,
            max_open_per_wallet: 5,
            resolvers: vec![self.resolver.pubkey()],
            max_window: 30 * DAY,
            max_total_open: u64::MAX,
        }
    }

    pub fn ix_update_config(
        &self,
        admin: &Pubkey,
        args: shake_escrow::instructions::UpdateConfigArgs,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::UpdateConfig { args }.data(),
            shake_escrow::accounts::UpdateConfig {
                admin: *admin,
                config: config_pda(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_create_wager(
        &self,
        payer: &Pubkey,
        args: shake_escrow::instructions::CreateWagerArgs,
    ) -> Instruction {
        self.ix_create_wager_with(payer, &self.mint, args)
    }
    pub fn ix_create_wager_with(
        &self,
        payer: &Pubkey,
        mint: &Pubkey,
        args: shake_escrow::instructions::CreateWagerArgs,
    ) -> Instruction {
        let wager = wager_pda(&args.side_a, args.nonce);
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CreateWager { args }.data(),
            shake_escrow::accounts::CreateWager {
                payer: *payer,
                config: config_pda(),
                wager,
                mint: *mint,
                vault: ata_for(&wager, mint),
                token_program: token_program_id(),
                associated_token_program: ata_program_id(),
                system_program: system_program::ID,
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn default_wager_args(
        &mut self,
        nonce: u64,
        stake: u64,
    ) -> shake_escrow::instructions::CreateWagerArgs {
        let now = self.now();
        shake_escrow::instructions::CreateWagerArgs {
            nonce,
            side_a: self.a.pubkey(),
            side_b: self.b.pubkey(),
            stake,
            terms_hash: [7u8; 32],
            deadline_fund: now + 600,
            deadline_resolve: now + 1_200,
            resolver: self.resolver.pubkey(),
        }
    }

    pub fn ix_stake(&self, staker: &Pubkey, payer: &Pubkey, wager: &Pubkey) -> Instruction {
        let mint = self.mint;
        self.ix_stake_with(
            staker,
            payer,
            wager,
            &ata_for(staker, &mint),
            &ata_for(wager, &mint),
            &counter_pda(staker),
        )
    }
    pub fn ix_stake_with(
        &self,
        staker: &Pubkey,
        payer: &Pubkey,
        wager: &Pubkey,
        staker_token: &Pubkey,
        vault: &Pubkey,
        counter: &Pubkey,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::StakeSide {}.data(),
            shake_escrow::accounts::StakeSide {
                staker: *staker,
                payer: *payer,
                config: config_pda(),
                wager: *wager,
                counter: *counter,
                staker_token: *staker_token,
                vault: *vault,
                token_program: token_program_id(),
                system_program: system_program::ID,
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_unstake(&self, staker: &Pubkey, wager: &Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::UnstakeSide {}.data(),
            shake_escrow::accounts::UnstakeSide {
                staker: *staker,
                config: config_pda(),
                wager: *wager,
                counter: counter_pda(staker),
                staker_token: ata_for(staker, &self.mint),
                vault: ata_for(wager, &self.mint),
                token_program: token_program_id(),
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_resolve(&self, wager: &Pubkey, w: &shake_escrow::state::Wager, winner: &Pubkey) -> Instruction {
        self.ix_resolve_with(wager, w, winner, &ata_for(winner, &self.mint), &self.fee_token, &self.ops.pubkey())
    }
    pub fn ix_resolve_with(
        &self,
        wager: &Pubkey,
        w: &shake_escrow::state::Wager,
        winner: &Pubkey,
        winner_token: &Pubkey,
        fee_token: &Pubkey,
        rent_collector: &Pubkey,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::Resolve { winner: *winner }.data(),
            shake_escrow::accounts::Resolve {
                resolver: self.resolver.pubkey(),
                config: config_pda(),
                wager: *wager,
                winner: *winner,
                winner_token: *winner_token,
                fee_token: *fee_token,
                vault: ata_for(wager, &self.mint),
                counter_a: counter_pda(&w.side_a),
                counter_b: counter_pda(&w.side_b),
                rent_collector: *rent_collector,
                token_program: token_program_id(),
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_refund(&self, cranker: &Pubkey, wager: &Pubkey, side: &Pubkey) -> Instruction {
        self.ix_refund_with(cranker, wager, side, &ata_for(side, &self.mint), &counter_pda(side))
    }
    pub fn ix_refund_with(
        &self,
        cranker: &Pubkey,
        wager: &Pubkey,
        side: &Pubkey,
        side_token: &Pubkey,
        counter: &Pubkey,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::RefundSide {}.data(),
            shake_escrow::accounts::RefundSide {
                cranker: *cranker,
                config: config_pda(),
                wager: *wager,
                side: *side,
                side_token: *side_token,
                counter: *counter,
                vault: ata_for(wager, &self.mint),
                token_program: token_program_id(),
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_cancel_propose(&self, participant: &Pubkey, wager: &Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CancelPropose {}.data(),
            shake_escrow::accounts::CancelPropose {
                participant: *participant,
                wager: *wager,
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }
    pub fn ix_cancel_clear(&self, participant: &Pubkey, wager: &Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CancelClear {}.data(),
            shake_escrow::accounts::CancelClear {
                participant: *participant,
                wager: *wager,
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }
    pub fn ix_cancel_accept(
        &self,
        acceptor: &Pubkey,
        wager: &Pubkey,
        w: &shake_escrow::state::Wager,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CancelAccept {}.data(),
            shake_escrow::accounts::CancelAccept {
                acceptor: *acceptor,
                config: config_pda(),
                wager: *wager,
                side_a: w.side_a,
                side_b: w.side_b,
                side_a_token: ata_for(&w.side_a, &self.mint),
                side_b_token: ata_for(&w.side_b, &self.mint),
                counter_a: counter_pda(&w.side_a),
                counter_b: counter_pda(&w.side_b),
                fee_token: self.fee_token,
                vault: ata_for(wager, &self.mint),
                rent_collector: self.ops.pubkey(),
                token_program: token_program_id(),
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    pub fn ix_close_expired(
        &self,
        cranker: &Pubkey,
        wager: &Pubkey,
        fee_token: Option<Pubkey>,
        rent_collector: &Pubkey,
    ) -> Instruction {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CloseExpired {}.data(),
            shake_escrow::accounts::CloseExpired {
                cranker: *cranker,
                wager: *wager,
                vault: ata_for(wager, &self.mint),
                fee_token,
                rent_collector: *rent_collector,
                token_program: token_program_id(),
                event_authority: event_authority_pda(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    }

    // --- composite flows ---

    /// create (ops pays) → returns wager pda
    pub fn create(&mut self, nonce: u64, stake: u64) -> Pubkey {
        let args = self.default_wager_args(nonce, stake);
        let ops = self.ops.insecure_clone();
        let ix = self.ix_create_wager(&ops.pubkey(), args.clone());
        self.send(&[&ops], &[ix]).expect("create_wager failed");
        wager_pda(&args.side_a, nonce)
    }
    pub fn stake(&mut self, staker: &Keypair, wager: &Pubkey) -> TxResult {
        let ops = self.ops.insecure_clone();
        let staker = staker.insecure_clone();
        let ix = self.ix_stake(&staker.pubkey(), &ops.pubkey(), wager);
        self.send(&[&ops, &staker], &[ix])
    }
    /// create + both stakes → Active wager
    pub fn active_wager(&mut self, nonce: u64, stake: u64) -> Pubkey {
        let wager = self.create(nonce, stake);
        let a = self.a.insecure_clone();
        let b = self.b.insecure_clone();
        self.stake(&a, &wager).expect("stake A failed");
        self.stake(&b, &wager).expect("stake B failed");
        wager
    }
    pub fn resolve_to(&mut self, wager: &Pubkey, winner: &Pubkey) -> TxResult {
        let w = self.get_wager(wager);
        let resolver = self.resolver.insecure_clone();
        let ix = self.ix_resolve(wager, &w, winner);
        self.send(&[&resolver], &[ix])
    }
    pub fn refund(&mut self, wager: &Pubkey, side: &Pubkey) -> TxResult {
        let cranker = Keypair::new();
        self.svm.airdrop(&cranker.pubkey(), 1_000_000_000).unwrap();
        let ix = self.ix_refund(&cranker.pubkey(), wager, side);
        self.send(&[&cranker], &[ix])
    }
    pub fn close_expired(&mut self, wager: &Pubkey) -> TxResult {
        let cranker = Keypair::new();
        self.svm.airdrop(&cranker.pubkey(), 1_000_000_000).unwrap();
        let ops = self.ops.pubkey();
        let ix = self.ix_close_expired(&cranker.pubkey(), wager, Some(self.fee_token), &ops);
        self.send(&[&cranker], &[ix])
    }
}

/// Force the deployed program's upgrade authority to `authority` inside LiteSVM, so the
/// tests can exercise instructions gated on it. LiteSVM's add_program writes a loader-v3
/// program account pointing at a programdata account; this rewrites the authority option
/// bytes in place.
fn force_upgrade_authority(svm: &mut LiteSVM, program_id: &Pubkey, authority: &Pubkey) {
    let prog = svm.get_account(program_id).expect("program account missing");
    assert_eq!(
        prog.owner,
        upgradeable_loader_id(),
        "LiteSVM did not deploy under the upgradeable loader; E39 fixture needs adapting"
    );
    assert_eq!(
        u32::from_le_bytes(prog.data[0..4].try_into().unwrap()),
        2,
        "program account is not UpgradeableLoaderState::Program"
    );
    let programdata = Pubkey::try_from(&prog.data[4..36]).unwrap();
    let mut pd = svm
        .get_account(&programdata)
        .expect("programdata account missing");
    assert_eq!(
        u32::from_le_bytes(pd.data[0..4].try_into().unwrap()),
        3,
        "programdata account is not UpgradeableLoaderState::ProgramData"
    );
    pd.data[12] = 1; // Some
    pd.data[13..45].copy_from_slice(authority.as_ref());
    svm.set_account(programdata, pd).unwrap();
}

pub fn setup() -> Env {
    setup_with(|e| e.default_config_args())
}

pub fn setup_with(
    make_args: impl Fn(&Env) -> shake_escrow::instructions::InitializeConfigArgs,
) -> Env {
    let program_id = shake_escrow::id();
    let mut svm = LiteSVM::new();
    let bytes = include_bytes!(concat!(
        env!("CARGO_TARGET_TMPDIR"),
        "/../deploy/shake_escrow.so"
    ));
    svm.add_program(program_id, bytes).unwrap();

    let admin = Keypair::new();
    let ops = Keypair::new();
    let agent = Keypair::new();
    let resolver = Keypair::new();
    let a = Keypair::new();
    let b = Keypair::new();
    for k in [&admin, &ops, &agent, &resolver, &a, &b] {
        svm.airdrop(&k.pubkey(), 100_000_000_000).unwrap();
    }
    force_upgrade_authority(&mut svm, &program_id, &admin.pubkey());

    // Mint with admin as both mint and freeze authority, matching real USDC, whose issuer
    // holds a freeze authority. The frozen-account tests need it to exist.
    let admin_pk = admin.pubkey();
    let mint = CreateMint::new(&mut svm, &admin)
        .decimals(6)
        .freeze_authority(&admin_pk)
        .send()
        .unwrap();
    let fee_token = CreateAssociatedTokenAccount::new(&mut svm, &admin, &mint)
        .owner(&agent.pubkey())
        .send()
        .unwrap();
    let a_token = CreateAssociatedTokenAccount::new(&mut svm, &admin, &mint)
        .owner(&a.pubkey())
        .send()
        .unwrap();
    let b_token = CreateAssociatedTokenAccount::new(&mut svm, &admin, &mint)
        .owner(&b.pubkey())
        .send()
        .unwrap();
    MintTo::new(&mut svm, &admin, &mint, &a_token, 1_000 * USDC).send().unwrap();
    MintTo::new(&mut svm, &admin, &mint, &b_token, 1_000 * USDC).send().unwrap();

    let mut env = Env {
        svm,
        admin,
        ops,
        agent,
        resolver,
        a,
        b,
        mint,
        fee_token,
        a_token,
        b_token,
    };
    let args = make_args(&env);
    let admin = env.admin.insecure_clone();
    let ix = env.ix_initialize_config(&admin.pubkey(), args);
    env.send(&[&admin], &[ix]).expect("initialize_config failed");
    env
}

/// Assert a failed tx carries the named Anchor error (by error name in the logs).
pub fn expect_err(res: TxResult, code: &str) {
    match res {
        Ok(_) => panic!("expected failure with {code}, but tx succeeded"),
        Err(f) => {
            let logs = f.meta.logs.join("\n");
            assert!(
                logs.contains(code),
                "expected error {code}, got logs:\n{logs}\nerr: {:?}",
                f.err
            );
        }
    }
}
