// Coverage-guided fuzzing, run as a second engine against the same shadow model as
// programs/shake-escrow/tests/fuzz_state_machine.rs. Two independent implementations of
// the rules disagreeing is a much stronger signal than one of them passing.
//
// Trident drives instruction selection with its own RNG and coverage feedback. After
// every flow the invariants are checked against an independently maintained model:
// per-actor balances, fee balance, counters, the global accumulator, per-vault balance,
// closed-means-gone, and total token conservation.
//
// On signatures: TridentSVM processes transactions without signature verification, so
// this engine exercises the program's own authorization logic (participant checks,
// resolver identity, admin gating) and not the runtime's signature layer. That layer is
// covered by the LiteSVM suite in test_substitution.rs.
use fuzz_accounts::*;
use trident_fuzz::fuzzing::*;
mod fuzz_accounts;
mod types;
use types::shake_escrow::*;
use types::*;

const USDC: u64 = 1_000_000;
const ACTORS: usize = 3;
const CAP_PER_WALLET: u8 = 5;
const GLOBAL_CAP: u64 = 400 * USDC;
const START_BALANCE: u64 = 10_000 * USDC;
const FEE_BPS: u16 = 300;

fn kp(seed: u8) -> solana_sdk::signature::Keypair {
    solana_sdk::signature::Keypair::new_from_array([seed; 32])
}

/// Fail loudly, in a way the runner cannot swallow.
///
/// Trident v0.12.0 catches harness panics with `catch_unwind` and, in this configuration,
/// reports nothing: no message, and exit code 0 even with TRIDENT_WITH_EXIT_CODE=1. A
/// deliberate `panic!` in `#[init]` produced a clean pass while no flows had run at all.
/// So every invariant here aborts the process instead of panicking. `abort()` cannot be
/// caught, which makes a violation always visible as a non-zero exit and a stderr line.
///
/// If you change this harness, break something on purpose and confirm the run fails
/// before trusting a green result from it.
macro_rules! invariant {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            use std::io::Write;
            eprintln!("\n!!! SHAKE INVARIANT VIOLATION: {}", format!($($arg)*));
            let _ = std::io::stderr().flush();
            std::process::abort();
        }
    };
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MState {
    Funding,
    Active,
}

#[derive(Clone, Debug)]
struct MWager {
    key: Pubkey,
    sa: usize,
    sb: usize,
    stake: u64,
    fee_bps: u16,
    d_fund: i64,
    d_resolve: i64,
    state: MState,
    funded: [bool; 2],
    refunded: [bool; 2],
    proposal: Option<usize>,
    closed: bool,
}

impl MWager {
    fn side_of(&self, actor: usize) -> Option<usize> {
        if actor == self.sa {
            Some(0)
        } else if actor == self.sb {
            Some(1)
        } else {
            None
        }
    }
    fn vault_expected(&self) -> u64 {
        (0..2)
            .filter(|s| self.funded[*s] && !self.refunded[*s])
            .map(|_| self.stake)
            .sum()
    }
    fn fully_exited(&self) -> bool {
        (0..2).all(|s| !self.funded[s] || self.refunded[s])
    }
}

#[derive(FuzzTestMethods)]
struct FuzzTest {
    trident: Trident,
    fuzz_accounts: AccountAddresses,
    // --- fixtures ---
    mint: Pubkey,
    fee_token: Pubkey,
    actor_keys: Vec<Pubkey>,
    actor_atas: Vec<Pubkey>,
    config: Pubkey,
    event_authority: Pubkey,
    nonce: u64,
    // --- shadow model ---
    balances: [u64; ACTORS],
    fee_balance: u64,
    counters: [u8; ACTORS],
    total_open: u64,
    fee_bps_current: u16,
    wagers: Vec<MWager>,
}

impl FuzzTest {
    fn pid() -> Pubkey {
        program_id()
    }
    fn admin() -> solana_sdk::signature::Keypair {
        kp(1)
    }
    fn ops() -> Pubkey {
        kp(2).pubkey()
    }
    fn resolver() -> Pubkey {
        kp(3).pubkey()
    }
    fn agent() -> Pubkey {
        kp(4).pubkey()
    }
    fn mint_kp() -> solana_sdk::signature::Keypair {
        kp(5)
    }

    fn wager_pda(&self, side_a: &Pubkey, nonce: u64) -> Pubkey {
        Pubkey::find_program_address(&[b"wager", side_a.as_ref(), &nonce.to_le_bytes()], &Self::pid()).0
    }
    fn counter_pda(&self, wallet: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[b"counter", wallet.as_ref()], &Self::pid()).0
    }
    fn ata(&mut self, owner: &Pubkey) -> Pubkey {
        self.trident
            .get_associated_token_address(&self.mint, owner, &spl_token_id())
    }
    fn token_amount(&mut self, ata: &Pubkey) -> u64 {
        self.trident
            .get_token_account(*ata)
            .map(|t| t.account.amount)
            .unwrap_or(0)
    }
    fn account_gone(&mut self, key: &Pubkey) -> bool {
        let a = self.trident.get_account(key);
        a.data().is_empty() || a.lamports() == 0
    }
    fn counter_count(&mut self, wallet: &Pubkey) -> u8 {
        let pda = self.counter_pda(wallet);
        self.trident
            .get_account_with_type::<ExposureCounter>(&pda, 8)
            .map(|c| c.count)
            .unwrap_or(0)
    }
    fn config_state(&mut self) -> Option<Config> {
        let c = self.config;
        self.trident.get_account_with_type::<Config>(&c, 8)
    }
    fn wager_state(&mut self, key: &Pubkey) -> Option<Wager> {
        self.trident.get_account_with_type::<Wager>(key, 8)
    }
    fn now(&self) -> i64 {
        self.trident.get_current_timestamp()
    }

    /// Every invariant, after every flow. Any drift panics with context.
    fn check_invariants(&mut self, tag: &str) {
        for i in 0..ACTORS {
            let ata = self.actor_atas[i];
            let chain = self.token_amount(&ata);
            invariant!(
                chain == self.balances[i],
                "[{tag}] actor {i} balance drift: chain {chain} model {}",
                self.balances[i]
            );
        }
        let fee_ata = self.fee_token;
        let chain_fee = self.token_amount(&fee_ata);
        invariant!(
            chain_fee == self.fee_balance,
            "[{tag}] fee balance drift: chain {chain_fee} model {}",
            self.fee_balance
        );
        for i in 0..ACTORS {
            let k = self.actor_keys[i];
            let chain = self.counter_count(&k);
            invariant!(
                chain == self.counters[i],
                "[{tag}] counter drift actor {i}: chain {chain} model {}",
                self.counters[i]
            );
        }
        if let Some(cfg) = self.config_state() {
            invariant!(
                cfg.total_open_value == self.total_open,
                "[{tag}] total_open drift: chain {} model {}",
                cfg.total_open_value,
                self.total_open
            );
        }
        let mut vault_sum = 0u64;
        let wagers = self.wagers.clone();
        for w in &wagers {
            let vault = self.ata(&w.key);
            if w.closed {
                let gone_w = self.account_gone(&w.key);
                invariant!(gone_w, "[{tag}] closed wager still exists");
                let gone_v = self.account_gone(&vault);
                invariant!(gone_v, "[{tag}] closed vault still exists");
            } else {
                let expected = w.vault_expected();
                let chain = self.token_amount(&vault);
                invariant!(
                    chain == expected,
                    "[{tag}] vault drift: chain {chain} model {expected}"
                );
                vault_sum += expected;
            }
        }
        let circulating: u64 = self.balances.iter().sum::<u64>() + self.fee_balance + vault_sum;
        let minted = (ACTORS as u64) * START_BALANCE;
        invariant!(
            circulating == minted,
            "[{tag}] token conservation violated: circulating {circulating} minted {minted}"
        );
    }
}

fn spl_token_id() -> Pubkey {
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
}

#[flow_executor]
impl FuzzTest {
    fn new() -> Self {
        Self {
            trident: Trident::default(),
            fuzz_accounts: AccountAddresses::default(),
            mint: Pubkey::default(),
            fee_token: Pubkey::default(),
            actor_keys: vec![],
            actor_atas: vec![],
            config: Pubkey::default(),
            event_authority: Pubkey::default(),
            nonce: 0,
            balances: [START_BALANCE; ACTORS],
            fee_balance: 0,
            counters: [0; ACTORS],
            total_open: 0,
            fee_bps_current: FEE_BPS,
            wagers: vec![],
        }
    }

    #[init]
    fn start(&mut self) {
        let admin = Self::admin();
        let mint_kp = Self::mint_kp();
        self.mint = mint_kp.pubkey();
        self.config = Pubkey::find_program_address(&[b"config"], &Self::pid()).0;
        self.event_authority =
            Pubkey::find_program_address(&[b"__event_authority"], &Self::pid()).0;
        self.nonce = 0;
        self.balances = [START_BALANCE; ACTORS];
        self.fee_balance = 0;
        self.counters = [0; ACTORS];
        self.total_open = 0;
        self.fee_bps_current = FEE_BPS;
        self.wagers.clear();

        for k in [admin.pubkey(), Self::ops(), Self::resolver(), Self::agent()] {
            self.trident.airdrop(&k, 1_000_000_000_000);
        }
        self.actor_keys = (0..ACTORS).map(|i| kp(10 + i as u8).pubkey()).collect();
        for k in self.actor_keys.clone() {
            self.trident.airdrop(&k, 1_000_000_000_000);
        }

        // Mint with admin as mint AND freeze authority (mirrors USDC).
        let ap = admin.pubkey();
        let mk = self.mint;
        let ixs = self.trident.initialize_mint(&ap, &mk, 6, &ap, Some(&ap));
        self.trident.process_transaction(&ixs, Some("init_mint"));

        // ATAs: three actors + the agent's fee account.
        self.actor_atas = vec![];
        let mut owners = self.actor_keys.clone();
        owners.push(Self::agent());
        for owner in owners.clone() {
            let ix = self
                .trident
                .initialize_associated_token_account(&ap, &mk, &owner);
            self.trident.process_transaction(&[ix], Some("init_ata"));
        }
        for i in 0..ACTORS {
            let owner = self.actor_keys[i];
            let ata = self.ata(&owner);
            self.actor_atas.push(ata);
            let ix = self.trident.mint_to(&ata, &mk, &ap, START_BALANCE);
            self.trident.process_transaction(&[ix], Some("mint_to"));
        }
        let agent = Self::agent();
        self.fee_token = self.ata(&agent);

        // initialize_config — gated to the upgrade authority pinned in Trident.toml.
        let program_data = self.trident.get_program_data_address_v3(&Self::pid());
        let args = InitializeConfigArgs {
            mint: self.mint,
            fee_bps: FEE_BPS,
            fee_destination: self.fee_token,
            rent_collector: Self::ops(),
            min_stake: USDC,
            max_stake: 100 * USDC,
            max_open_per_wallet: CAP_PER_WALLET,
            resolvers: vec![Self::resolver()],
            max_window: 30 * 86_400,
            max_total_open: GLOBAL_CAP,
        };
        let ix = InitializeConfigInstruction::data(InitializeConfigInstructionData::new(args))
            .accounts(InitializeConfigInstructionAccounts::new(
                ap,
                self.config,
                program_data,
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("initialize_config"));
        invariant!(res.is_success(), "initialize_config failed: {}", res.logs());
        self.check_invariants("init");
    }

    /// create_wager — valid params; the model records the new wager.
    #[flow]
    fn flow_create(&mut self) {
        self.nonce += 1;
        let sa = self.trident.random_from_range(0..ACTORS);
        let mut sb = self.trident.random_from_range(0..ACTORS);
        if sb == sa {
            sb = (sb + 1) % ACTORS;
        }
        let stake = (1 + self.trident.random_from_range(0u64..50)) * USDC;
        let now = self.now();
        let d_fund = now + 30 + self.trident.random_from_range(0i64..500);
        let d_resolve = d_fund + 30 + self.trident.random_from_range(0i64..500);
        let (side_a, side_b) = (self.actor_keys[sa], self.actor_keys[sb]);
        let wager = self.wager_pda(&side_a, self.nonce);
        let vault = self.ata(&wager);
        let args = CreateWagerArgs {
            nonce: self.nonce,
            side_a,
            side_b,
            stake,
            terms_hash: [9u8; 32],
            deadline_fund: d_fund,
            deadline_resolve: d_resolve,
            resolver: Self::resolver(),
        };
        let ix = CreateWagerInstruction::data(CreateWagerInstructionData::new(args))
            .accounts(CreateWagerInstructionAccounts::new(
                Self::ops(),
                self.config,
                wager,
                self.mint,
                vault,
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("create_wager"));
        invariant!(res.is_success(), "valid create failed: {}", res.logs());
        self.wagers.push(MWager {
            key: wager,
            sa,
            sb,
            stake,
            fee_bps: self.fee_bps_current,
            d_fund,
            d_resolve,
            state: MState::Funding,
            funded: [false; 2],
            refunded: [false; 2],
            proposal: None,
            closed: false,
        });
        self.check_invariants("create");
    }

    /// stake_side — any actor against any wager; model decides expected outcome.
    #[flow]
    fn flow_stake(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let actor = self.trident.random_from_range(0..ACTORS);
        let w = self.wagers[wi].clone();
        let now = self.now();
        let side = w.side_of(actor);
        let ok = !w.closed
            && w.state == MState::Funding
            && now <= w.d_fund
            && side.map(|s| !w.funded[s]).unwrap_or(false)
            && self.counters[actor] < CAP_PER_WALLET
            && self.total_open + w.stake <= GLOBAL_CAP
            && self.balances[actor] >= w.stake;

        let staker = self.actor_keys[actor];
        let staker_token = self.actor_atas[actor];
        let vault = self.ata(&w.key);
        let counter = self.counter_pda(&staker);
        let ix = StakeSideInstruction::data(StakeSideInstructionData::new())
            .accounts(StakeSideInstructionAccounts::new(
                staker,
                Self::ops(),
                self.config,
                w.key,
                counter,
                staker_token,
                vault,
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("stake_side"));
        if ok {
            invariant!(res.is_success(), "model-ok stake failed: {}", res.logs());
            let s = side.unwrap();
            let mw = &mut self.wagers[wi];
            mw.funded[s] = true;
            if mw.funded[0] && mw.funded[1] {
                mw.state = MState::Active;
            }
            self.balances[actor] -= w.stake;
            self.counters[actor] += 1;
            self.total_open += w.stake;
        } else {
            invariant!(res.is_error(), "model-fail stake accepted (actor {actor})");
        }
        self.check_invariants("stake");
    }

    /// unstake_side: the unilateral pre-lock exit.
    #[flow]
    fn flow_unstake(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let actor = self.trident.random_from_range(0..ACTORS);
        let w = self.wagers[wi].clone();
        let now = self.now();
        let side = w.side_of(actor);
        let ok = !w.closed
            && w.state == MState::Funding
            && now <= w.d_fund
            && side.map(|s| w.funded[s]).unwrap_or(false);

        let staker = self.actor_keys[actor];
        let staker_token = self.actor_atas[actor];
        let vault = self.ata(&w.key);
        let counter = self.counter_pda(&staker);
        let ix = UnstakeSideInstruction::data(UnstakeSideInstructionData::new())
            .accounts(UnstakeSideInstructionAccounts::new(
                staker,
                self.config,
                w.key,
                counter,
                staker_token,
                vault,
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("unstake_side"));
        if ok {
            invariant!(res.is_success(), "model-ok unstake failed: {}", res.logs());
            let s = side.unwrap();
            let mw = &mut self.wagers[wi];
            mw.funded[s] = false;
            self.balances[actor] += w.stake;
            self.counters[actor] -= 1;
            self.total_open -= w.stake;
        } else {
            invariant!(res.is_error(), "model-fail unstake accepted");
        }
        self.check_invariants("unstake");
    }

    /// resolve — winner usually a participant, sometimes the resolver (must be refused).
    #[flow]
    fn flow_resolve(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let w = self.wagers[wi].clone();
        if w.closed {
            return;
        }
        let now = self.now();
        let pick_participant = self.trident.random_from_range(0u64..10) < 8;
        let widx = if self.trident.random_from_range(0u64..2) == 0 { w.sa } else { w.sb };
        let winner = if pick_participant {
            self.actor_keys[widx]
        } else {
            Self::resolver()
        };
        let winner_is_part = winner == self.actor_keys[w.sa] || winner == self.actor_keys[w.sb];
        let ok = w.state == MState::Active && now <= w.d_resolve && winner_is_part;

        let winner_token = self.ata(&winner);
        let vault = self.ata(&w.key);
        let (ca, cb) = (
            self.counter_pda(&self.actor_keys[w.sa]),
            self.counter_pda(&self.actor_keys[w.sb]),
        );
        let ix = ResolveInstruction::data(ResolveInstructionData::new(winner))
            .accounts(ResolveInstructionAccounts::new(
                Self::resolver(),
                self.config,
                w.key,
                winner,
                winner_token,
                self.fee_token,
                vault,
                ca,
                cb,
                Self::ops(),
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("resolve"));
        if ok {
            invariant!(res.is_success(), "model-ok resolve failed: {}", res.logs());
            let pot = w.stake * 2;
            let fee = ((pot as u128) * (w.fee_bps as u128) / 10_000) as u64;
            let wi_actor = if winner == self.actor_keys[w.sa] { w.sa } else { w.sb };
            self.balances[wi_actor] += pot - fee;
            self.fee_balance += fee;
            self.counters[w.sa] -= 1;
            self.counters[w.sb] -= 1;
            self.total_open -= pot;
            self.wagers[wi].closed = true;
        } else {
            invariant!(res.is_error(), "model-fail resolve accepted");
        }
        self.check_invariants("resolve");
    }

    /// concede — any actor; the named winner is usually the counterparty, sometimes the
    /// conceder itself, which must be refused.
    #[flow]
    fn flow_concede(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let w = self.wagers[wi].clone();
        if w.closed {
            return;
        }
        let now = self.now();
        let actor = self.trident.random_from_range(0..ACTORS);
        let side = w.side_of(actor);
        let names_self = self.trident.random_from_range(0u64..10) >= 8;
        let winner_idx = match side {
            Some(0) if !names_self => w.sb,
            Some(1) if !names_self => w.sa,
            _ => actor,
        };
        let ok = w.state == MState::Active && now <= w.d_resolve && side.is_some() && !names_self;

        let conceder = self.actor_keys[actor];
        let winner = self.actor_keys[winner_idx];
        let winner_token = self.actor_atas[winner_idx];
        let vault = self.ata(&w.key);
        let (ca, cb) = (
            self.counter_pda(&self.actor_keys[w.sa]),
            self.counter_pda(&self.actor_keys[w.sb]),
        );
        let ix = ConcedeInstruction::data(ConcedeInstructionData::new())
            .accounts(ConcedeInstructionAccounts::new(
                conceder,
                self.config,
                w.key,
                winner,
                winner_token,
                self.fee_token,
                vault,
                ca,
                cb,
                Self::ops(),
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("concede"));
        if ok {
            invariant!(res.is_success(), "model-ok concede failed: {}", res.logs());
            let pot = w.stake * 2;
            let fee = ((pot as u128) * (w.fee_bps as u128) / 10_000) as u64;
            self.balances[winner_idx] += pot - fee;
            self.fee_balance += fee;
            self.counters[w.sa] -= 1;
            self.counters[w.sb] -= 1;
            self.total_open -= pot;
            self.wagers[wi].closed = true;
        } else {
            invariant!(res.is_error(), "model-fail concede accepted");
        }
        self.check_invariants("concede");
    }

    /// refund_side: permissionless, and one side at a time.
    #[flow]
    fn flow_refund(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let w = self.wagers[wi].clone();
        if w.closed {
            return;
        }
        let s = self.trident.random_from_range(0usize..2);
        let side_actor = if s == 0 { w.sa } else { w.sb };
        let now = self.now();
        let due = match w.state {
            MState::Funding => now > w.d_fund,
            MState::Active => now > w.d_resolve,
        };
        let ok = due && w.funded[s] && !w.refunded[s];

        let side = self.actor_keys[side_actor];
        let side_token = self.actor_atas[side_actor];
        let vault = self.ata(&w.key);
        let counter = self.counter_pda(&side);
        let cranker = self.trident.random_pubkey(); // anyone
        let ix = RefundSideInstruction::data(RefundSideInstructionData::new())
            .accounts(RefundSideInstructionAccounts::new(
                cranker,
                self.config,
                w.key,
                side,
                side_token,
                counter,
                vault,
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("refund_side"));
        if ok {
            invariant!(res.is_success(), "model-ok refund failed: {}", res.logs());
            self.wagers[wi].refunded[s] = true;
            self.balances[side_actor] += w.stake;
            self.counters[side_actor] -= 1;
            self.total_open -= w.stake;
        } else {
            invariant!(res.is_error(), "model-fail refund accepted");
        }
        self.check_invariants("refund");
    }

    /// close_expired: permissionless rent reclamation.
    #[flow]
    fn flow_close(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let w = self.wagers[wi].clone();
        let now = self.now();
        let vault = self.ata(&w.key);
        let cranker = self.trident.random_pubkey();
        let ix = CloseExpiredInstruction::data(CloseExpiredInstructionData::new())
            .accounts(CloseExpiredInstructionAccounts::new(
                cranker,
                w.key,
                vault,
                self.fee_token,
                Self::ops(),
                self.event_authority,
                Self::pid(),
            ))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("close_expired"));
        if w.closed {
            invariant!(res.is_error(), "close on already-closed wager accepted");
            self.check_invariants("close");
            return;
        }
        let due = match w.state {
            MState::Funding => now > w.d_fund,
            MState::Active => now > w.d_resolve,
        };
        let ok = due && w.fully_exited();
        if ok {
            invariant!(res.is_success(), "model-ok close failed: {}", res.logs());
            self.wagers[wi].closed = true;
        } else {
            invariant!(res.is_error(), "model-fail close accepted");
        }
        self.check_invariants("close");
    }

    /// The cancel trio: propose, clear, accept.
    #[flow]
    fn flow_cancel(&mut self) {
        if self.wagers.is_empty() {
            return;
        }
        let wi = self.trident.random_from_range(0..self.wagers.len());
        let w = self.wagers[wi].clone();
        if w.closed {
            return;
        }
        let actor = self.trident.random_from_range(0..ACTORS);
        let participant = self.actor_keys[actor];
        let now = self.now();
        let which = self.trident.random_from_range(0u64..3);

        if which == 0 {
            let ok = w.state == MState::Active
                && now <= w.d_resolve
                && w.side_of(actor).is_some()
                && w.proposal.is_none();
            let ix = CancelProposeInstruction::data(CancelProposeInstructionData::new())
                .accounts(CancelProposeInstructionAccounts::new(
                    participant,
                    w.key,
                    self.event_authority,
                    Self::pid(),
                ))
                .instruction();
            let res = self.trident.process_transaction(&[ix], Some("cancel_propose"));
            if ok {
                invariant!(res.is_success(), "model-ok propose failed: {}", res.logs());
                self.wagers[wi].proposal = Some(actor);
            } else {
                invariant!(res.is_error(), "model-fail propose accepted");
            }
        } else if which == 1 {
            let ok = w.proposal.is_some() && w.side_of(actor).is_some();
            let ix = CancelClearInstruction::data(CancelClearInstructionData::new())
                .accounts(CancelClearInstructionAccounts::new(
                    participant,
                    w.key,
                    self.event_authority,
                    Self::pid(),
                ))
                .instruction();
            let res = self.trident.process_transaction(&[ix], Some("cancel_clear"));
            if ok {
                invariant!(res.is_success(), "model-ok clear failed: {}", res.logs());
                self.wagers[wi].proposal = None;
            } else {
                invariant!(res.is_error(), "model-fail clear accepted");
            }
        } else {
            let ok = w.state == MState::Active
                && now <= w.d_resolve
                && w.side_of(actor).is_some()
                && w.proposal.is_some()
                && w.proposal != Some(actor);
            let (sa_key, sb_key) = (self.actor_keys[w.sa], self.actor_keys[w.sb]);
            let (sa_ata, sb_ata) = (self.actor_atas[w.sa], self.actor_atas[w.sb]);
            let (ca, cb) = (self.counter_pda(&sa_key), self.counter_pda(&sb_key));
            let vault = self.ata(&w.key);
            let ix = CancelAcceptInstruction::data(CancelAcceptInstructionData::new())
                .accounts(CancelAcceptInstructionAccounts::new(
                    participant,
                    self.config,
                    w.key,
                    sa_key,
                    sb_key,
                    sa_ata,
                    sb_ata,
                    ca,
                    cb,
                    self.fee_token,
                    vault,
                    Self::ops(),
                    self.event_authority,
                    Self::pid(),
                ))
                .instruction();
            let res = self.trident.process_transaction(&[ix], Some("cancel_accept"));
            if ok {
                invariant!(res.is_success(), "model-ok accept failed: {}", res.logs());
                self.balances[w.sa] += w.stake;
                self.balances[w.sb] += w.stake;
                self.counters[w.sa] -= 1;
                self.counters[w.sb] -= 1;
                self.total_open -= w.stake * 2;
                self.wagers[wi].closed = true;
            } else {
                invariant!(res.is_error(), "model-fail accept accepted");
            }
        }
        self.check_invariants("cancel");
    }

    /// Admin fee change. Must reach future wagers only, never a live one.
    #[flow]
    fn flow_update_fee(&mut self) {
        let new_fee = self.trident.random_from_range(0u16..=1_000);
        let args = UpdateConfigArgs {
            fee_bps: Some(new_fee),
            fee_destination: None,
            rent_collector: None,
            min_stake: None,
            max_stake: None,
            max_open_per_wallet: None,
            resolvers: None,
            paused: None,
            max_window: None,
            max_total_open: None,
        };
        let admin = Self::admin().pubkey();
        let ix = UpdateConfigInstruction::data(UpdateConfigInstructionData::new(args))
            .accounts(UpdateConfigInstructionAccounts::new(admin, self.config))
            .instruction();
        let res = self.trident.process_transaction(&[ix], Some("update_config"));
        invariant!(res.is_success(), "fee update failed: {}", res.logs());
        self.fee_bps_current = new_fee;
        self.check_invariants("update_fee");
    }

    /// Time passes.
    #[flow]
    fn flow_warp(&mut self) {
        let secs = 1 + self.trident.random_from_range(0i64..400);
        self.trident.forward_in_time(secs);
        self.check_invariants("warp");
    }

    #[end]
    fn end(&mut self) {
        self.check_invariants("end");
    }
}

/// Read a positive integer from the environment, falling back to `default`.
fn from_env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    // Iterations, then flows per iteration. Raise both for a longer campaign:
    //   SHAKE_FUZZ_ITERATIONS=2000 SHAKE_FUZZ_FLOWS=120 trident fuzz run fuzz_0
    FuzzTest::fuzz(
        from_env("SHAKE_FUZZ_ITERATIONS", 300),
        from_env("SHAKE_FUZZ_FLOWS", 60),
    );
}
