# Shake escrow rail

A peer-to-peer wager escrow program for Solana. Two people stake equal amounts, a named
resolver picks the winner **from among those two people**, and if nothing happens by the
deadline anyone at all can crank the refund.

It exists because the primitive was missing. Job-escrow programs hold one funder's money
against a deliverable; this holds two funders' money against an outcome.

## The guarantees, precisely

1. **Nothing drainable.** No key can move vault funds anywhere except to a participant or
   back to the stakers. There is no withdraw-to-arbitrary-address instruction, at all.
2. **Funds can never strand.** Every state has a permissionless exit after its deadline —
   `refund_side` per side, `close_expired` for the rent. A stranger can free the money and
   cannot take a lamport of it: destinations are pinned at creation.
3. **The resolver's only power** is naming a winner among the two participants, before the
   deadline. A stolen resolver key can misresolve open wagers; it can never exfiltrate. A
   participant can also concede a locked wager, which pays the other participant exactly as
   resolve would; conceding can only ever cost the side that signs it.
4. **The admin can never touch a live wager.** Config changes (caps, fee, allowlist,
   pause) apply to future wagers only; live ones carry a snapshot taken at creation. The
   admin role changes hands in two steps: the current admin proposes a key, and nothing
   changes until that key signs to accept.
5. **Upgradeable, and honest about it.** Whoever holds the upgrade authority can replace
   these bytes. That authority belongs in a multisig, and immutability is a decision to be
   made once the program has earned it — not a claim to make on day one.

## Instructions

`initialize_config` (gated to the program's upgrade authority) · `update_config` ·
`propose_admin` / `accept_admin` / `cancel_admin_transfer` · `create_wager` · `stake_side` ·
`unstake_side` (unilateral, pre-lock) · `resolve` · `concede` (signed by the side that
loses) · `refund_side` (permissionless) · `cancel_propose` / `cancel_clear` /
`cancel_accept` · `close_expired` (permissionless)

## Build and test

```bash
anchor build
cargo test                                                    # the whole suite
cargo test --test fuzz_state_machine fuzz_short               # model-based fuzzing
cargo test --test fuzz_state_machine fuzz_soak -- --ignored   # the long campaign
```

The suite is organised by what it defends: `test_happy` (lifecycle and money math),
`test_boundaries` (deadline semantics at ±1 second, both deadlines, both directions),
`test_create_guards`, `test_stake_guards`, `test_resolve_refund_guards` (including frozen
and closed token accounts), `test_substitution` (every account swapped for a plausible
imposter must fail), `test_concede`, `test_admin` (including the exposure-cap bounds and the
admin handover), and `test_regressions`.

Second fuzzing engine (coverage-guided):

```bash
cd trident-tests && TRIDENT_WITH_EXIT_CODE=1 trident fuzz run fuzz_0
```

> **Read this before trusting a fuzz result.** Trident v0.12.0 catches harness panics and
> reports nothing — a deliberate panic produced a clean pass and exit code 0. This
> harness therefore aborts the process on an invariant violation instead of panicking.
> After any change to it, break something on purpose and confirm the run fails.

## Verified against a live cluster

`tests/devnet_e2e.rs` drives a deployed program end to end: a payout; a wager that nobody
completes being refunded, with the refund cranked by a wallet that has no relationship to
the bet, to demonstrate that the exit needs nobody's permission and pays the cranker
nothing; and a concession. A second test hands the admin role to a fresh key and back.
Point it at your own deployment and watch it.

## Configuration

Nothing about the environment is hardcoded, and nothing defaults. The RPC comes from
`SHAKE_RPC_URL` and the signing key from `SHAKE_PAYER_KEYPAIR`; public Solana endpoints
are refused outright, because rate limits and dropped subscriptions have no place in a
money path. A test that spends real SOL should never guess which key to spend it with.

```bash
SHAKE_RPC_URL=https://... SHAKE_PAYER_KEYPAIR=/path/to/id.json \
  cargo test --test devnet_e2e -- --ignored --nocapture
```

Against a config initialized earlier, the payout needs the key of the resolver that config
allows, in `SHAKE_RESOLVER_KEYPAIR`. The handover test writes its temporary admin key to
`SHAKE_HANDOVER_KEY_OUT` before proposing it, so an interrupted run can still hand the role
back.

## Status and posture

Devnet, pre-launch. The launch posture is deliberately staged: authority held in a
multisig, per-wager and global caps kept small while confidence is earned and raised only
as it is, and an independent audit plus a bug bounty at the point where real volume
exists. Self-review does not stay the last line once strangers' money is large.

The caps are the bridge between "reviewed" and "audited": while they are small, the worst
case for any single failure is small too.

## Licence

Apache License 2.0. See `LICENSE` and `NOTICE`.

Apache rather than a shorter permissive licence for one substantive reason: it grants
patent rights explicitly and withdraws them from anyone who brings a patent suit over the
software. MIT and ISC say nothing about patents at all, which is a gap worth closing under
code that holds other people's money. It also states the terms inbound contributions arrive
under, which stops being a hypothetical the first time a stranger opens a pull request.
