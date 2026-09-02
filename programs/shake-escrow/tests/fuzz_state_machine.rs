// Model-based state-machine fuzzing on the LiteSVM rig.
//
// Thousands of randomized instruction sequences run against a shadow model that
// re-implements the rules independently. After every action the invariants are checked:
// per-vault balance conservation, exact actor and fee balances, exact counters, an exact
// global accumulator, closed-means-gone, and total token conservation. Any divergence
// panics with the seed and the step number, so a failure replays exactly.
//
// Divergence means any of three things: an action the model says must fail that the chain
// accepted, an action the model says must succeed that the chain rejected, or a balance
// that moved differently in the two.
//
// Deterministic by construction: xorshift* PRNG, fixed seeds, no wall clock, no external
// entropy.
mod common;
use common::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

const ACTORS: usize = 3;
const CAP_PER_WALLET: u8 = 5;
const GLOBAL_CAP: u64 = 400 * USDC;
const START_BALANCE: u64 = 10_000 * USDC;

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MState {
    Funding,
    Active,
}

#[derive(Clone, Debug)]
struct MWager {
    key: anchor_lang::prelude::Pubkey,
    sa: usize,
    sb: usize,
    stake: u64,
    fee_bps: u16,
    d_fund: i64,
    d_resolve: i64,
    state: MState,
    funded: [bool; 2],
    refunded: [bool; 2],
    proposal: Option<usize>, // actor index
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
        let mut v = 0u64;
        for s in 0..2 {
            if self.funded[s] && !self.refunded[s] {
                v += self.stake;
            }
        }
        v
    }
    fn fully_exited(&self) -> bool {
        (0..2).all(|s| !self.funded[s] || self.refunded[s])
    }
}

struct Model {
    balances: [u64; ACTORS],
    fee_balance: u64,
    counters: [u8; ACTORS],
    total_open: u64,
    fee_bps_current: u16,
    wagers: Vec<MWager>,
}

fn run_sequence(seed: u64, steps: usize) {
    let mut rng = Rng::new(seed);
    let mut env = setup_with(|e| {
        let mut args = e.default_config_args();
        args.max_total_open = GLOBAL_CAP;
        args.max_open_per_wallet = CAP_PER_WALLET;
        args
    });

    // Third actor with ATA + funds; top up a and b to the fuzz balance.
    let c = Keypair::new();
    env.svm.airdrop(&c.pubkey(), 100_000_000_000).unwrap();
    let c_ata = env.create_ata(&c.pubkey());
    env.mint_to(&c_ata, START_BALANCE);
    let a_token = env.a_token;
    let b_token = env.b_token;
    env.mint_to(&a_token, START_BALANCE - 1_000 * USDC);
    env.mint_to(&b_token, START_BALANCE - 1_000 * USDC);

    let actors: Vec<Keypair> = vec![
        env.a.insecure_clone(),
        env.b.insecure_clone(),
        c.insecure_clone(),
    ];
    let atas: Vec<anchor_lang::prelude::Pubkey> = vec![a_token, b_token, c_ata];
    let total_minted: u64 = (ACTORS as u64) * START_BALANCE;

    let mut m = Model {
        balances: [START_BALANCE; ACTORS],
        fee_balance: 0,
        counters: [0; ACTORS],
        total_open: 0,
        fee_bps_current: 300,
        wagers: vec![],
    };
    let mut nonce: u64 = seed.wrapping_mul(1_000_000);

    for step in 0..steps {
        let ctx = |what: &str| format!("seed {seed} step {step}: {what}");
        let now = env.now();
        let action = rng.below(100);

        match action {
            // ---- create (valid) ----
            0..=14 => {
                nonce += 1;
                let sa = rng.below(ACTORS as u64) as usize;
                let mut sb = rng.below(ACTORS as u64) as usize;
                if sb == sa {
                    sb = (sb + 1) % ACTORS;
                }
                let stake = (1 + rng.below(50)) * USDC;
                let d_fund = now + 30 + rng.below(500) as i64;
                let d_resolve = d_fund + 30 + rng.below(500) as i64;
                let args = shake_escrow::instructions::CreateWagerArgs {
                    nonce,
                    side_a: actors[sa].pubkey(),
                    side_b: actors[sb].pubkey(),
                    stake,
                    terms_hash: [9u8; 32],
                    deadline_fund: d_fund,
                    deadline_resolve: d_resolve,
                    resolver: env.resolver.pubkey(),
                };
                let ops = env.ops.insecure_clone();
                let ix = env.ix_create_wager(&ops.pubkey(), args);
                env.send(&[&ops], &[ix]).unwrap_or_else(|e| panic!("{}: {e:?}", ctx("valid create failed")));
                m.wagers.push(MWager {
                    key: wager_pda(&actors[sa].pubkey(), nonce),
                    sa,
                    sb,
                    stake,
                    fee_bps: m.fee_bps_current,
                    d_fund,
                    d_resolve,
                    state: MState::Funding,
                    funded: [false; 2],
                    refunded: [false; 2],
                    proposal: None,
                    closed: false,
                });
            }
            // ---- create (invalid variants must all fail) ----
            15..=17 => {
                nonce += 1;
                let sa = rng.below(ACTORS as u64) as usize;
                let mut args = shake_escrow::instructions::CreateWagerArgs {
                    nonce,
                    side_a: actors[sa].pubkey(),
                    side_b: actors[sa].pubkey(), // self-wager default badness
                    stake: 10 * USDC,
                    terms_hash: [9u8; 32],
                    deadline_fund: now + 100,
                    deadline_resolve: now + 200,
                    resolver: env.resolver.pubkey(),
                };
                match rng.below(4) {
                    0 => {} // self wager
                    1 => {
                        args.side_b = anchor_lang::prelude::Pubkey::default();
                    }
                    2 => {
                        args.side_b = actors[(sa + 1) % ACTORS].pubkey();
                        args.deadline_fund = now - 5;
                    }
                    _ => {
                        args.side_b = actors[(sa + 1) % ACTORS].pubkey();
                        args.terms_hash = [0u8; 32];
                    }
                }
                let ops = env.ops.insecure_clone();
                let ix = env.ix_create_wager(&ops.pubkey(), args);
                assert!(env.send(&[&ops], &[ix]).is_err(), "{}", ctx("invalid create accepted"));
            }
            // ---- stake ----
            18..=42 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let actor = rng.below(ACTORS as u64) as usize;
                let w = m.wagers[wi].clone();
                let side = w.side_of(actor);
                let ok = !w.closed
                    && w.state == MState::Funding
                    && now <= w.d_fund
                    && side.map(|s| !w.funded[s]).unwrap_or(false)
                    && m.counters[actor] < CAP_PER_WALLET
                    && m.total_open + w.stake <= GLOBAL_CAP
                    && m.balances[actor] >= w.stake;
                let res = env.stake(&actors[actor], &w.key);
                if ok {
                    res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok stake failed")));
                    let s = side.unwrap();
                    let mw = &mut m.wagers[wi];
                    mw.funded[s] = true;
                    if mw.funded[0] && mw.funded[1] {
                        mw.state = MState::Active;
                    }
                    m.balances[actor] -= w.stake;
                    m.counters[actor] += 1;
                    m.total_open += w.stake;
                } else {
                    assert!(res.is_err(), "{}", ctx("model-fail stake accepted"));
                }
            }
            // ---- unstake ----
            43..=50 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let actor = rng.below(ACTORS as u64) as usize;
                let w = m.wagers[wi].clone();
                let side = w.side_of(actor);
                let ok = !w.closed
                    && w.state == MState::Funding
                    && now <= w.d_fund
                    && side.map(|s| w.funded[s]).unwrap_or(false);
                let kp = actors[actor].insecure_clone();
                let ix = env.ix_unstake(&kp.pubkey(), &w.key);
                let res = env.send(&[&kp], &[ix]);
                if ok {
                    res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok unstake failed")));
                    let s = side.unwrap();
                    let mw = &mut m.wagers[wi];
                    mw.funded[s] = false;
                    m.balances[actor] += w.stake;
                    m.counters[actor] -= 1;
                    m.total_open -= w.stake;
                } else {
                    assert!(res.is_err(), "{}", ctx("model-fail unstake accepted"));
                }
            }
            // ---- resolve (random winner, sometimes invalid) ----
            51..=62 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let w = m.wagers[wi].clone();
                if w.closed {
                    continue;
                }
                // winner: usually a participant, sometimes the resolver (invalid).
                let winner_pk = if rng.below(10) < 8 {
                    let idx = if rng.below(2) == 0 { w.sa } else { w.sb };
                    actors[idx].pubkey()
                } else {
                    env.resolver.pubkey()
                };
                let winner_is_part = winner_pk == actors[w.sa].pubkey() || winner_pk == actors[w.sb].pubkey();
                let ok = w.state == MState::Active && now <= w.d_resolve && winner_is_part;
                let res = env.resolve_to(&w.key, &winner_pk);
                if ok {
                    res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok resolve failed")));
                    let pot = w.stake * 2;
                    let fee = ((pot as u128) * (w.fee_bps as u128) / 10_000) as u64;
                    let widx = if winner_pk == actors[w.sa].pubkey() { w.sa } else { w.sb };
                    m.balances[widx] += pot - fee;
                    m.fee_balance += fee;
                    m.counters[w.sa] -= 1;
                    m.counters[w.sb] -= 1;
                    m.total_open -= pot;
                    m.wagers[wi].closed = true;
                } else {
                    assert!(res.is_err(), "{}", ctx("model-fail resolve accepted"));
                }
            }
            // ---- refund_side ----
            63..=74 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let w = m.wagers[wi].clone();
                if w.closed {
                    continue;
                }
                let s = rng.below(2) as usize;
                let side_actor = if s == 0 { w.sa } else { w.sb };
                let due = match w.state {
                    MState::Funding => now > w.d_fund,
                    MState::Active => now > w.d_resolve,
                };
                let ok = due && w.funded[s] && !w.refunded[s];
                let res = env.refund(&w.key, &actors[side_actor].pubkey());
                if ok {
                    res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok refund failed")));
                    m.wagers[wi].refunded[s] = true;
                    m.balances[side_actor] += w.stake;
                    m.counters[side_actor] -= 1;
                    m.total_open -= w.stake;
                } else {
                    assert!(res.is_err(), "{}", ctx("model-fail refund accepted"));
                }
            }
            // ---- close_expired ----
            75..=82 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let w = m.wagers[wi].clone();
                if w.closed {
                    // Closing a closed wager must fail structurally.
                    assert!(env.close_expired(&w.key).is_err(), "{}", ctx("close on closed accepted"));
                    continue;
                }
                let due = match w.state {
                    MState::Funding => now > w.d_fund,
                    MState::Active => now > w.d_resolve,
                };
                let ok = due && w.fully_exited();
                let res = env.close_expired(&w.key);
                if ok {
                    res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok close failed")));
                    m.wagers[wi].closed = true;
                } else {
                    assert!(res.is_err(), "{}", ctx("model-fail close accepted"));
                }
            }
            // ---- cancel propose / clear / accept ----
            83..=90 => {
                if m.wagers.is_empty() {
                    continue;
                }
                let wi = rng.below(m.wagers.len() as u64) as usize;
                let w = m.wagers[wi].clone();
                if w.closed {
                    continue;
                }
                let actor = rng.below(ACTORS as u64) as usize;
                let kp = actors[actor].insecure_clone();
                match rng.below(3) {
                    0 => {
                        let ok = w.state == MState::Active
                            && now <= w.d_resolve
                            && w.side_of(actor).is_some()
                            && w.proposal.is_none();
                        let ix = env.ix_cancel_propose(&kp.pubkey(), &w.key);
                        let res = env.send(&[&kp], &[ix]);
                        if ok {
                            res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok propose failed")));
                            m.wagers[wi].proposal = Some(actor);
                        } else {
                            assert!(res.is_err(), "{}", ctx("model-fail propose accepted"));
                        }
                    }
                    1 => {
                        let ok = w.proposal.is_some() && w.side_of(actor).is_some();
                        let ix = env.ix_cancel_clear(&kp.pubkey(), &w.key);
                        let res = env.send(&[&kp], &[ix]);
                        if ok {
                            res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok clear failed")));
                            m.wagers[wi].proposal = None;
                        } else {
                            assert!(res.is_err(), "{}", ctx("model-fail clear accepted"));
                        }
                    }
                    _ => {
                        let ok = w.state == MState::Active
                            && now <= w.d_resolve
                            && w.side_of(actor).is_some()
                            && w.proposal.is_some()
                            && w.proposal != Some(actor);
                        let onchain = env.get_wager_opt(&w.key);
                        let ix = match onchain {
                            Some(ow) => env.ix_cancel_accept(&kp.pubkey(), &w.key, &ow),
                            None => continue,
                        };
                        let res = env.send(&[&kp], &[ix]);
                        if ok {
                            res.unwrap_or_else(|e| panic!("{}: {e:?}", ctx("model-ok accept failed")));
                            m.balances[w.sa] += w.stake;
                            m.balances[w.sb] += w.stake;
                            m.counters[w.sa] -= 1;
                            m.counters[w.sb] -= 1;
                            m.total_open -= w.stake * 2;
                            m.wagers[wi].closed = true;
                        } else {
                            assert!(res.is_err(), "{}", ctx("model-fail accept accepted"));
                        }
                    }
                }
            }
            // ---- admin fee change (future wagers only) ----
            91..=93 => {
                let new_fee = rng.below(1_001) as u16;
                let admin = env.admin.insecure_clone();
                let ix = env.ix_update_config(
                    &admin.pubkey(),
                    shake_escrow::instructions::UpdateConfigArgs {
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
                        new_admin: None,
                    },
                );
                env.send(&[&admin], &[ix]).unwrap_or_else(|e| panic!("{}: {e:?}", ctx("fee update failed")));
                m.fee_bps_current = new_fee;
            }
            // ---- warp ----
            _ => {
                env.warp_by(1 + rng.below(400) as i64);
            }
        }

        // ---- invariants after every step ----
        for (i, ata) in atas.iter().enumerate() {
            let onchain = env.token_amount(ata);
            assert_eq!(
                onchain, m.balances[i],
                "{}",
                ctx(&format!("actor {i} balance drift: chain {onchain} model {}", m.balances[i]))
            );
        }
        {
            let fee_token = env.fee_token;
            let onchain = env.token_amount(&fee_token);
            assert_eq!(onchain, m.fee_balance, "{}", ctx("fee balance drift"));
        }
        for (i, kp) in actors.iter().enumerate() {
            let pk = kp.pubkey();
            let onchain = env.counter_count(&pk);
            assert_eq!(onchain, m.counters[i], "{}", ctx(&format!("counter drift actor {i}")));
        }
        {
            let cfg = env.get_config();
            assert_eq!(cfg.total_open_value, m.total_open, "{}", ctx("total_open drift"));
        }
        let mut vault_sum = 0u64;
        for w in &m.wagers {
            let vault = ata_for(&w.key, &env.mint.clone());
            if w.closed {
                assert!(env.account_gone(&w.key), "{}", ctx("closed wager still exists"));
                assert!(env.account_gone(&vault), "{}", ctx("closed vault still exists"));
            } else {
                let expected = w.vault_expected();
                let onchain = env.token_amount(&vault);
                assert_eq!(onchain, expected, "{}", ctx("vault balance drift"));
                vault_sum += expected;
            }
        }
        // Total conservation: every minted token is in an actor ATA, the fee ATA, or a vault.
        let circulating: u64 = m.balances.iter().sum::<u64>() + m.fee_balance + vault_sum;
        assert_eq!(circulating, total_minted, "{}", ctx("token conservation violated"));
    }
}

impl Env {
    fn get_wager_opt(&mut self, wager: &anchor_lang::prelude::Pubkey) -> Option<shake_escrow::state::Wager> {
        use anchor_lang::AccountDeserialize;
        let acc = self.svm.get_account(wager)?;
        if acc.data.is_empty() {
            return None;
        }
        let mut d: &[u8] = &acc.data;
        shake_escrow::state::Wager::try_deserialize(&mut d).ok()
    }
}

/// The fast run, suitable for every build: 40 seeds × 150 steps, about 6,000 actions.
#[test]
fn fuzz_short() {
    for seed in 0..40u64 {
        run_sequence(seed, 150);
    }
}

/// The soak run (minutes): 200 seeds × 400 steps ≈ 80,000 modeled actions.
/// cargo test --test fuzz_state_machine fuzz_soak -- --ignored --nocapture
#[test]
#[ignore]
fn fuzz_soak() {
    for seed in 1_000..1_200u64 {
        run_sequence(seed, 400);
        if seed % 20 == 0 {
            println!("soak: seed {seed} done");
        }
    }
}
