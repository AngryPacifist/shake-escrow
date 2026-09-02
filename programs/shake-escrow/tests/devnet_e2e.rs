// End-to-end against a deployed program on devnet, in both directions.
//
//   cargo test --test devnet_e2e -- --ignored --nocapture
//
// Direction 1 (payout): create → both stake → resolver resolves → winner paid, fee taken,
//                       vault + wager closed, rent home to the pinned collector.
// Direction 2 (refund):  create → ONE side stakes → funding deadline passes → a STRANGER
//                       wallet cranks refund_side (proving permissionlessness) →
//                       close_expired reclaims rent.
//
// Deadlines are 60s and 45s, computed from the chain clock rather than wall time, because
// devnet's Clock sysvar can lag. They are polled rather than blind-slept for the same
// reason. Every signature prints with an explorer link so a reader can check the run.
#![cfg(test)]

use {
    anchor_lang::{
        prelude::Pubkey,
        solana_program::{instruction::AccountMeta, instruction::Instruction, system_program},
        AccountDeserialize, InstructionData, ToAccountMetas,
    },
    base64::{engine::general_purpose::STANDARD as B64, Engine},
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
    std::{str::FromStr, thread::sleep, time::Duration},
};

const USDC: u64 = 1_000_000;

/// A dedicated RPC endpoint, from the environment, with no default. Public endpoints are
/// refused outright: rate limits and dropped subscriptions have no business anywhere near
/// a money path.
///   SHAKE_RPC_URL=https://... cargo test --test devnet_e2e -- --ignored --nocapture
fn rpc_url() -> String {
    let url = std::env::var("SHAKE_RPC_URL").unwrap_or_else(|_| {
        panic!("SHAKE_RPC_URL is not set — point it at the project's dedicated RPC (Helius)")
    });
    let host = url
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    for public in [
        "api.mainnet-beta.solana.com",
        "api.devnet.solana.com",
        "api.testnet.solana.com",
    ] {
        if host == public {
            panic!("SHAKE_RPC_URL points at the public endpoint {host}; use the dedicated RPC");
        }
    }
    url
}

fn token_program_id() -> Pubkey {
    Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
}
fn ata_program_id() -> Pubkey {
    Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap()
}
fn clock_sysvar() -> Pubkey {
    Pubkey::from_str("SysvarC1ock11111111111111111111111111111111").unwrap()
}
fn ata_for(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program_id().as_ref(), mint.as_ref()],
        &ata_program_id(),
    )
    .0
}
fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[shake_escrow::constants::CONFIG_SEED], &shake_escrow::id()).0
}
fn wager_pda(side_a: &Pubkey, nonce: u64) -> Pubkey {
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
fn counter_pda(w: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[shake_escrow::constants::COUNTER_SEED, w.as_ref()],
        &shake_escrow::id(),
    )
    .0
}
fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &shake_escrow::id()).0
}

struct Rpc;

impl Rpc {
    fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        for attempt in 0..5 {
            let resp = ureq::post(&rpc_url())
                .set("Content-Type", "application/json")
                .send_json(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": method, "params": params
                }));
            match resp {
                Ok(r) => {
                    let v: serde_json::Value = r.into_json().expect("json");
                    if let Some(err) = v.get("error") {
                        // 429s and transient node errors: back off and retry.
                        if attempt < 4 {
                            sleep(Duration::from_millis(800 * (attempt + 1)));
                            continue;
                        }
                        panic!("RPC {method} error: {err}");
                    }
                    return v["result"].clone();
                }
                Err(e) => {
                    if attempt < 4 {
                        sleep(Duration::from_millis(800 * (attempt + 1)));
                        continue;
                    }
                    panic!("RPC {method} transport error: {e}");
                }
            }
        }
        unreachable!()
    }

    fn account_data(&self, key: &Pubkey) -> Option<Vec<u8>> {
        let r = self.call(
            "getAccountInfo",
            serde_json::json!([key.to_string(), {"encoding": "base64", "commitment": "confirmed"}]),
        );
        let val = r.get("value")?;
        if val.is_null() {
            return None;
        }
        let b64 = val["data"][0].as_str()?;
        Some(B64.decode(b64).ok()?)
    }

    fn lamports(&self, key: &Pubkey) -> u64 {
        let r = self.call(
            "getAccountInfo",
            serde_json::json!([key.to_string(), {"encoding": "base64", "commitment": "confirmed"}]),
        );
        r["value"]["lamports"].as_u64().unwrap_or(0)
    }

    /// Chain time from the Clock sysvar (unix_timestamp at byte offset 32).
    fn chain_now(&self) -> i64 {
        let d = self.account_data(&clock_sysvar()).expect("clock sysvar");
        i64::from_le_bytes(d[32..40].try_into().unwrap())
    }

    fn token_amount(&self, ata: &Pubkey) -> u64 {
        match self.account_data(ata) {
            Some(d) if d.len() >= 72 => u64::from_le_bytes(d[64..72].try_into().unwrap()),
            _ => 0,
        }
    }

    fn min_rent(&self, len: usize) -> u64 {
        self.call("getMinimumBalanceForRentExemption", serde_json::json!([len]))
            .as_u64()
            .unwrap()
    }

    fn blockhash(&self) -> solana_message::Hash {
        let r = self.call(
            "getLatestBlockhash",
            serde_json::json!([{"commitment": "confirmed"}]),
        );
        let s = r["value"]["blockhash"].as_str().unwrap();
        solana_message::Hash::from_str(s).unwrap()
    }

    fn send(&self, signers: &[&Keypair], ixs: &[Instruction], label: &str) -> String {
        let payer = signers[0];
        let bh = self.blockhash();
        let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &bh);
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
            .expect("sign");
        let wire = bincode::serialize(&tx).expect("serialize");
        let sig = self.call(
            "sendTransaction",
            serde_json::json!([
                B64.encode(&wire),
                {"encoding": "base64", "preflightCommitment": "confirmed", "maxRetries": 5}
            ]),
        );
        let sig = sig.as_str().expect("signature").to_string();
        self.confirm(&sig, label);
        println!("  ✓ {label}\n    https://explorer.solana.com/tx/{sig}?cluster=devnet");
        sig
    }

    fn confirm(&self, sig: &str, label: &str) {
        for _ in 0..60 {
            let r = self.call(
                "getSignatureStatuses",
                serde_json::json!([[sig], {"searchTransactionHistory": true}]),
            );
            let st = &r["value"][0];
            if !st.is_null() {
                if let Some(err) = st.get("err") {
                    if !err.is_null() {
                        panic!("{label} failed on-chain: {err}");
                    }
                }
                let c = st["confirmationStatus"].as_str().unwrap_or("");
                if c == "confirmed" || c == "finalized" {
                    return;
                }
            }
            sleep(Duration::from_millis(700));
        }
        panic!("{label}: not confirmed in time (sig {sig})");
    }

    /// Poll the chain clock until it passes `ts`.
    fn wait_until(&self, ts: i64, what: &str) {
        loop {
            let now = self.chain_now();
            if now > ts {
                println!("  · chain clock {now} > {what} {ts}");
                return;
            }
            println!("  · waiting for {what}: {}s to go", ts - now + 1);
            sleep(Duration::from_secs((ts - now + 1).min(20) as u64));
        }
    }
}

// --- raw SPL / system instruction builders (fixed, frozen layouts) ---

fn sys_create_account(from: &Pubkey, to: &Pubkey, lamports: u64, space: u64, owner: &Pubkey) -> Instruction {
    let mut data = Vec::with_capacity(52);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&space.to_le_bytes());
    data.extend_from_slice(owner.as_ref());
    Instruction {
        program_id: system_program::ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, true)],
        data,
    }
}

fn init_mint2(mint: &Pubkey, decimals: u8, authority: &Pubkey, freeze: &Pubkey) -> Instruction {
    let mut data = vec![20u8, decimals];
    data.extend_from_slice(authority.as_ref());
    data.push(1); // freeze authority present (mirrors USDC)
    data.extend_from_slice(freeze.as_ref());
    Instruction {
        program_id: token_program_id(),
        accounts: vec![AccountMeta::new(*mint, false)],
        data,
    }
}

fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ata_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(ata_for(owner, mint), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(token_program_id(), false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

fn mint_to(mint: &Pubkey, dest: &Pubkey, authority: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![7u8];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: token_program_id(),
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

fn sys_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: system_program::ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

/// The deploying key, from the environment with no default. This test spends real SOL and
/// signs as the program's upgrade authority, so it should never guess which key to use.
fn load_payer() -> Keypair {
    let path = std::env::var("SHAKE_PAYER_KEYPAIR").unwrap_or_else(|_| {
        panic!("SHAKE_PAYER_KEYPAIR is not set — point it at the deployer's keypair json")
    });
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read the keypair at {path}: {e}"));
    let bytes: Vec<u8> = serde_json::from_str(&raw).expect("keypair json");
    Keypair::try_from(&bytes[..]).expect("keypair bytes")
}

#[test]
#[ignore]
fn devnet_e2e_both_directions() {
    let rpc = Rpc;
    // The deployer is also the upgrade authority, the rent payer and the rent collector
    // in this run, which keeps the fixture small.
    let payer = load_payer();
    let a = Keypair::new();
    let b = Keypair::new();
    let resolver = Keypair::new();
    let agent = Keypair::new();
    // Cranks the refund later, with no relationship to the wager beyond gas money.
    let stranger = Keypair::new();

    println!("\n=== SHAKE devnet e2e — program {} ===", shake_escrow::id());
    println!("payer/ops   {}", payer.pubkey());
    println!("side A      {}", a.pubkey());
    println!("side B      {}", b.pubkey());
    println!("resolver    {}", resolver.pubkey());
    println!("stranger    {}\n", stranger.pubkey());

    // --- fixtures: mint, ATAs, funding ---
    println!("[setup]");
    let mint = Keypair::new();
    let rent = rpc.min_rent(82);
    rpc.send(
        &[&payer, &mint],
        &[
            sys_create_account(&payer.pubkey(), &mint.pubkey(), rent, 82, &token_program_id()),
            init_mint2(&mint.pubkey(), 6, &payer.pubkey(), &payer.pubkey()),
        ],
        "create test mint (6dp, freeze authority set — mirrors USDC)",
    );
    let m = mint.pubkey();
    let (a_ata, b_ata, fee_ata) = (
        ata_for(&a.pubkey(), &m),
        ata_for(&b.pubkey(), &m),
        ata_for(&agent.pubkey(), &m),
    );
    rpc.send(
        &[&payer],
        &[
            create_ata_idempotent(&payer.pubkey(), &a.pubkey(), &m),
            create_ata_idempotent(&payer.pubkey(), &b.pubkey(), &m),
            create_ata_idempotent(&payer.pubkey(), &agent.pubkey(), &m),
            mint_to(&m, &a_ata, &payer.pubkey(), 100 * USDC),
            mint_to(&m, &b_ata, &payer.pubkey(), 100 * USDC),
            // The stranger needs lamports to pay its own crank fee — nothing else.
            sys_transfer(&payer.pubkey(), &stranger.pubkey(), 20_000_000),
        ],
        "ATAs + fund A/B with 100 test-USDC each + fund stranger's gas",
    );

    // --- config (idempotent: reuse if a previous run initialized it) ---
    let cfg_key = config_pda();
    let existing = rpc.account_data(&cfg_key);
    let (cfg_mint, cfg_fee_ata, cfg_resolver, cfg_rent_collector) = match &existing {
        Some(d) => {
            let mut s: &[u8] = d;
            let c = shake_escrow::state::Config::try_deserialize(&mut s).expect("config decode");
            println!("[config] reusing existing devnet config (mint {})", c.mint);
            (c.mint, c.fee_destination, c.resolver_allowlist[0], c.rent_collector)
        }
        None => {
            let program_data =
                Pubkey::find_program_address(&[shake_escrow::id().as_ref()], &Pubkey::from_str("BPFLoaderUpgradeab1e11111111111111111111111").unwrap()).0;
            let args = shake_escrow::instructions::InitializeConfigArgs {
                mint: m,
                fee_bps: 300,
                fee_destination: fee_ata,
                rent_collector: payer.pubkey(),
                min_stake: USDC,
                max_stake: 100 * USDC,
                max_open_per_wallet: 5,
                resolvers: vec![resolver.pubkey()],
                max_window: 30 * 86_400,
                max_total_open: 10_000 * USDC,
            };
            let ix = Instruction::new_with_bytes(
                shake_escrow::id(),
                &shake_escrow::instruction::InitializeConfig { args }.data(),
                shake_escrow::accounts::InitializeConfig {
                    authority: payer.pubkey(),
                    config: cfg_key,
                    program: shake_escrow::id(),
                    program_data,
                    system_program: system_program::ID,
                }
                .to_account_metas(None),
            );
            println!("[config]");
            rpc.send(&[&payer], &[ix], "initialize_config (gated to the upgrade authority)");
            (m, fee_ata, resolver.pubkey(), payer.pubkey())
        }
    };
    // A reused config pins its own mint/resolver; rebuild the fixtures to match it.
    let (m, fee_ata) = (cfg_mint, cfg_fee_ata);
    let (a_ata, b_ata) = (ata_for(&a.pubkey(), &m), ata_for(&b.pubkey(), &m));
    if existing.is_some() {
        rpc.send(
            &[&payer],
            &[
                create_ata_idempotent(&payer.pubkey(), &a.pubkey(), &m),
                create_ata_idempotent(&payer.pubkey(), &b.pubkey(), &m),
                mint_to(&m, &a_ata, &payer.pubkey(), 100 * USDC),
                mint_to(&m, &b_ata, &payer.pubkey(), 100 * USDC),
            ],
            "re-fund A/B on the config's pinned mint",
        );
    }
    let resolver_key = cfg_resolver;
    let resolver_signer = if resolver_key == resolver.pubkey() {
        resolver.insecure_clone()
    } else {
        panic!("existing devnet config pins resolver {resolver_key}; rerun needs that key");
    };

    let ix_stake = |staker: &Pubkey, wager: &Pubkey| {
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::StakeSide {}.data(),
            shake_escrow::accounts::StakeSide {
                staker: *staker,
                payer: payer.pubkey(),
                config: cfg_key,
                wager: *wager,
                counter: counter_pda(staker),
                staker_token: ata_for(staker, &m),
                vault: ata_for(wager, &m),
                token_program: token_program_id(),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    };
    let ix_create = |nonce: u64, side_a: &Pubkey, side_b: &Pubkey, stake: u64, d_fund: i64, d_res: i64| {
        let wager = wager_pda(side_a, nonce);
        Instruction::new_with_bytes(
            shake_escrow::id(),
            &shake_escrow::instruction::CreateWager {
                args: shake_escrow::instructions::CreateWagerArgs {
                    nonce,
                    side_a: *side_a,
                    side_b: *side_b,
                    stake,
                    terms_hash: [0xE2; 32],
                    deadline_fund: d_fund,
                    deadline_resolve: d_res,
                    resolver: resolver_key,
                },
            }
            .data(),
            shake_escrow::accounts::CreateWager {
                payer: payer.pubkey(),
                config: cfg_key,
                wager,
                mint: m,
                vault: ata_for(&wager, &m),
                token_program: token_program_id(),
                associated_token_program: ata_program_id(),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: shake_escrow::id(),
            }
            .to_account_metas(None),
        )
    };

    let nonce_base = rpc.chain_now() as u64;

    // ============ DIRECTION 1: the payout ============
    println!("\n[direction 1 — payout] 60s funding window, 120s resolve window");
    let n1 = nonce_base;
    let w1 = wager_pda(&a.pubkey(), n1);
    let now = rpc.chain_now();
    let a_before = rpc.token_amount(&a_ata);
    let fee_before = rpc.token_amount(&fee_ata);
    rpc.send(
        &[&payer],
        &[ix_create(n1, &a.pubkey(), &b.pubkey(), 10 * USDC, now + 60, now + 120)],
        &format!("create_wager (10 USDC/side) — wager {w1}"),
    );
    rpc.send(&[&payer, &a], &[ix_stake(&a.pubkey(), &w1)], "stake_side — A");
    rpc.send(&[&payer, &b], &[ix_stake(&b.pubkey(), &w1)], "stake_side — B (wager now Active)");
    let vault1 = ata_for(&w1, &m);
    assert_eq!(rpc.token_amount(&vault1), 20 * USDC, "vault should hold both stakes");
    println!("  · vault {vault1} holds 20 USDC");

    let ix_resolve = Instruction::new_with_bytes(
        shake_escrow::id(),
        &shake_escrow::instruction::Resolve { winner: a.pubkey() }.data(),
        shake_escrow::accounts::Resolve {
            resolver: resolver_key,
            config: cfg_key,
            wager: w1,
            winner: a.pubkey(),
            winner_token: a_ata,
            fee_token: fee_ata,
            vault: vault1,
            counter_a: counter_pda(&a.pubkey()),
            counter_b: counter_pda(&b.pubkey()),
            rent_collector: cfg_rent_collector,
            token_program: token_program_id(),
            event_authority: event_authority(),
            program: shake_escrow::id(),
        }
        .to_account_metas(None),
    );
    let sig_resolve = rpc.send(
        &[&payer, &resolver_signer],
        &[ix_resolve],
        "resolve → A wins",
    );

    let a_after = rpc.token_amount(&a_ata);
    let fee_after = rpc.token_amount(&fee_ata);
    // staked 10, won 20 minus 3% fee (0.6) = 19.4 → net +9.4
    assert_eq!(a_after, a_before - 10 * USDC + 19_400_000, "winner payout wrong");
    assert_eq!(fee_after - fee_before, 600_000, "fee wrong");
    assert!(rpc.account_data(&w1).is_none(), "wager should be closed");
    assert!(rpc.account_data(&vault1).is_none(), "vault should be closed");
    println!("  · A net +9.4 USDC, fee 0.6 USDC, wager+vault closed ✓");

    // ============ DIRECTION 2: the timeout refund ============
    println!("\n[direction 2 — timeout refund] 45s funding window, one side stakes, stranger cranks");
    let n2 = nonce_base + 1;
    let w2 = wager_pda(&a.pubkey(), n2);
    let now = rpc.chain_now();
    let d_fund = now + 45;
    let a_before2 = rpc.token_amount(&a_ata);
    rpc.send(
        &[&payer],
        &[ix_create(n2, &a.pubkey(), &b.pubkey(), 5 * USDC, d_fund, now + 300)],
        &format!("create_wager (5 USDC/side) — wager {w2}"),
    );
    rpc.send(&[&payer, &a], &[ix_stake(&a.pubkey(), &w2)], "stake_side — A only (B never shows)");
    let vault2 = ata_for(&w2, &m);
    assert_eq!(rpc.token_amount(&vault2), 5 * USDC);

    rpc.wait_until(d_fund, "funding deadline");

    // The stranger — no relationship to this wager — cranks the refund and pays its own fee.
    let ix_refund = Instruction::new_with_bytes(
        shake_escrow::id(),
        &shake_escrow::instruction::RefundSide {}.data(),
        shake_escrow::accounts::RefundSide {
            cranker: stranger.pubkey(),
            config: cfg_key,
            wager: w2,
            side: a.pubkey(),
            side_token: a_ata,
            counter: counter_pda(&a.pubkey()),
            vault: vault2,
            token_program: token_program_id(),
            event_authority: event_authority(),
            program: shake_escrow::id(),
        }
        .to_account_metas(None),
    );
    let sig_refund = rpc.send(
        &[&stranger],
        &[ix_refund],
        "refund_side — cranked by a STRANGER wallet (permissionless exit)",
    );
    assert_eq!(rpc.token_amount(&a_ata), a_before2, "A should be made whole");
    println!("  · A's 5 USDC returned in full, no fee ✓");

    let collector_before = rpc.lamports(&cfg_rent_collector);
    let ix_close = Instruction::new_with_bytes(
        shake_escrow::id(),
        &shake_escrow::instruction::CloseExpired {}.data(),
        shake_escrow::accounts::CloseExpired {
            cranker: stranger.pubkey(),
            wager: w2,
            vault: vault2,
            fee_token: Some(fee_ata),
            rent_collector: cfg_rent_collector,
            token_program: token_program_id(),
            event_authority: event_authority(),
            program: shake_escrow::id(),
        }
        .to_account_metas(None),
    );
    let sig_close = rpc.send(
        &[&stranger],
        &[ix_close],
        "close_expired — stranger reclaims rent TO THE PINNED COLLECTOR (not itself)",
    );
    assert!(rpc.account_data(&w2).is_none(), "wager should be closed");
    assert!(rpc.account_data(&vault2).is_none(), "vault should be closed");
    let collector_after = rpc.lamports(&cfg_rent_collector);
    assert!(
        collector_after > collector_before,
        "rent must land with the pinned collector, not the cranker (E21)"
    );
    println!(
        "  · rent {} lamports → pinned collector, cranker got nothing ✓",
        collector_after - collector_before
    );

    println!("\n=== PROOF ARTIFACTS (devnet) ===");
    println!("program   https://explorer.solana.com/address/{}?cluster=devnet", shake_escrow::id());
    println!("payout    https://explorer.solana.com/tx/{sig_resolve}?cluster=devnet");
    println!("refund    https://explorer.solana.com/tx/{sig_refund}?cluster=devnet");
    println!("close     https://explorer.solana.com/tx/{sig_close}?cluster=devnet");
    println!("wager 1   https://explorer.solana.com/address/{w1}?cluster=devnet");
    println!("wager 2   https://explorer.solana.com/address/{w2}?cluster=devnet");
    println!("=== both directions verified on live devnet ===\n");
}
