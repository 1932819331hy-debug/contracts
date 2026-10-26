# StellarSend Contracts

Soroban smart contracts powering the [StellarSend](https://github.com/StellarSend) global money transfer platform. All contracts are written in Rust and deployed on the Stellar network.

---

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Contracts](#contracts)
  - [stellar\_send](#stellar_send)
  - [fee\_collector](#fee_collector)
  - [token\_bridge](#token_bridge)
  - [escrow](#escrow)
- [Differentiator Features](#differentiator-features)
  - [Scheduled & recurring payments](#scheduled--recurring-payments)
  - [Split / batch payments](#split--batch-payments)
  - [Payment requests / invoicing](#payment-requests--invoicing)
  - [Escrow / conditional transfers](#escrow--conditional-transfers)
- [Getting Started](#getting-started)
- [Building](#building)
- [Testing](#testing)
- [Deployment](#deployment)
- [Contract API Reference](#contract-api-reference)
- [Events](#events)
- [Security](#security)
- [Contributing](#contributing)
- [License](#license)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                      User / dApp                        │
└──────────────────────┬──────────────────────────────────┘
                       │ send_payment / send_path_payment
                       ▼
┌─────────────────────────────────────────────────────────┐
│               stellar_send  (Entry Point)               │
│                                                         │
│  • Validates sender + amount                            │
│  • Deducts protocol fee (bps)                           │
│  • Routes payment to recipient                          │
│  • Emits PaymentSent / PathPaymentSent events           │
└─────────┬───────────────────────────┬───────────────────┘
          │ forward fee               │ DEX path swap
          ▼                           ▼
┌──────────────────────┐   ┌─────────────────────────────┐
│    fee_collector     │   │       token_bridge           │
│                      │   │                              │
│  • Accumulates fees  │   │  • wrap / unwrap assets      │
│  • Admin withdrawal  │   │  • Enables cross-asset       │
│  • Per-token totals  │   │    path payments             │
└──────────────────────┘   └─────────────────────────────┘
```

The system is intentionally modular: `stellar_send` is the only contract that end users interact with directly. `fee_collector` and `token_bridge` are infrastructure contracts managed by the protocol admin.

---

## Contracts

### stellar\_send

**Path:** `contracts/stellar_send/`

The main entry point for the StellarSend protocol. Supports direct token transfers and DEX path payments between any Stellar addresses.

| Feature | Description |
|---|---|
| Direct payments | Transfer any SEP-41 token from sender to recipient |
| Path payments | Route through the Stellar DEX for cross-asset transfers |
| Fee deduction | Protocol fee in basis points (bps), deducted at source |
| Pause / unpause | Admin can halt new payments in an emergency |
| Payment records | Every transfer is stored on-ledger for auditability |
| Admin rotation | Two-step admin handover (propose → accept) |

**Key data types:**

```rust
pub struct ContractConfig {
    pub admin: Address,
    pub fee_bps: u32,          // 100 bps = 1%
    pub fee_collector: Address,
    pub active: bool,
    pub min_amount: i128,
    pub max_amount: i128,
}

pub struct PaymentRecord {
    pub from: Address,
    pub to: Address,
    pub token: Address,
    pub net_amount: i128,
    pub fee_amount: i128,
    pub memo: String,
    pub ledger: u32,
}
```

---

### fee\_collector

**Path:** `contracts/fee_collector/`

Receives protocol fees forwarded by `stellar_send` and allows the treasury admin to withdraw accumulated balances.

| Feature | Description |
|---|---|
| Fee accounting | Tracks lifetime totals collected per token |
| Withdrawal | Admin-only; supports arbitrary recipient |
| Treasury management | Treasury address updatable by admin |
| Period limits | Configurable withdrawal limit per epoch |
| Multi-token | Operates on any SEP-41 token |

---

### token\_bridge

**Path:** `contracts/token_bridge/`

A lightweight wrap / unwrap bridge for non-native Stellar assets. Users deposit an underlying token and receive an equal amount of wrapped tokens, enabling cross-asset path payments through the StellarSend DEX flow.

| Feature | Description |
|---|---|
| wrap | Deposit underlying → receive wrapped balance |
| unwrap | Burn wrapped balance → receive underlying |
| Bridge fee | Configurable bps fee on wrap operations |
| Supply cap | Configurable maximum wrapped supply |
| Pause / unpause | Admin can halt wrapping in an emergency |
| Admin rotation | Two-step admin handover |

---

### escrow

**Path:** `contracts/escrow/`

A standalone contract holding funds in on-chain custody until a time or arbiter condition releases them — deposits, marketplace holdbacks, milestone payments. Split out as its own crate (like `fee_collector`/`token_bridge`) since it has its own fund-custody lifecycle independent of `stellar_send`.

| Feature | Description |
|---|---|
| Time-locked release | Beneficiary can claim once `unlock_time` has passed |
| Optional arbiter | Can release to the beneficiary or refund to the depositor at any time |
| Anti-griefing refund | Depositor can only self-refund after `unlock_time` + a 1-week grace period |
| Escrow lookup | `get_escrow` for status/UI polling |

---

### factory

**Path:** `contracts/factory/`

Deploys and atomically initializes `fee_collector`, `token_bridge`, and
`stellar_send` instances, closing the front-runnable `initialize()` window
described in #58. See [Deployment](#deployment) and the crate's module doc
comment for the full rationale.

| Feature | Description |
|---|---|
| Atomic deploy + init | `deploy_fee_collector` / `deploy_token_bridge` / `deploy_stellar_send` deploy the child contract and call its `initialize` within one host invocation |
| Admin-gated | Every `deploy_*` call requires this factory's own `admin` to authorize |

---

## Differentiator Features

StellarSend isn't just a wallet — these on-chain building blocks are what any plain Stellar wallet can't do. All four are implemented directly in the contracts in this repo (`stellar_send` for subscriptions/batch/requests, a new `escrow` crate for conditional transfers), with unit tests covering the happy path and at least one failure case each.

### Scheduled & recurring payments

`stellar_send/src/subscription.rs` — a payer authorizes a recurring transfer once; a keeper (anyone, including an unprivileged bot) can then trigger each due payment without the payer being online.

```rust
create_subscription(env, payer, recipient, token, amount, interval_seconds, start_time) -> Result<u64, StellarSendError>
cancel_subscription(env, id) -> Result<(), StellarSendError>
execute_subscription(env, id) -> Result<i128, StellarSendError>
get_subscription(env, id) -> Result<Subscription, StellarSendError>
```

The payer grants the contract a standard SEP-41 token allowance (`token.approve`) when creating the subscription; execution pulls funds via `transfer_from` rather than requiring the payer's live signature, which is what makes unattended, scheduled execution possible while staying non-custodial — the contract enforces the authorization, not a trusted backend. Re-execution before `next_execution_time` is rejected.

### Split / batch payments

`stellar_send/src/batch.rs` — one sender, many recipients, one call.

```rust
send_batch_payment(env, from, token, payments: Vec<(Address, i128)>) -> Result<Vec<PaymentRecord>, StellarSendError>
```

Reuses the same fee-splitting logic as `send_payment`. Batches are **all-or-nothing**: every leg is validated up front, and since a Soroban host transaction is atomic, any transfer failure reverts the whole call — there's no partial batch state to reconcile.

### Payment requests / invoicing

`stellar_send/src/payment_request.rs` — turns StellarSend into a two-sided payment tool: a requester creates a shareable "invoice," and a payer fulfills it.

```rust
create_payment_request(env, requester, payer: Option<Address>, token, amount, memo, expiry) -> Result<u64, StellarSendError>
fulfill_payment_request(env, request_id, payer) -> Result<i128, StellarSendError>
cancel_payment_request(env, request_id) -> Result<(), StellarSendError>
get_payment_request(env, request_id) -> Result<PaymentRequest, StellarSendError>
```

`payer` is optional — `None` means anyone can fulfill it. Expired or already-fulfilled/cancelled requests are rejected; the protocol fee is deducted exactly as in `send_payment`. The fee rate is locked in when the request is created, so an admin `set_fee` after creation never retroactively changes what the requester nets — the stored `PaymentRequest.fee_bps` (exposed via `get_payment_request`) is what fulfillment applies.

### Escrow / conditional transfers

`escrow/src/lib.rs` — funds locked in-contract until a time or arbiter condition releases them.

```rust
create_escrow(env, depositor, beneficiary, token, amount, unlock_time, arbiter: Option<Address>) -> Result<u64, EscrowError>
release_escrow(env, escrow_id, caller) -> Result<(), EscrowError>
refund_escrow(env, escrow_id, caller) -> Result<(), EscrowError>
get_escrow(env, escrow_id) -> Result<Escrow, EscrowError>
```

`caller` must authorize the call itself (`caller.require_auth()`) — the beneficiary can release after `unlock_time`, and an optional arbiter can release **or** refund at any time. The depositor can only self-refund after `unlock_time` plus a one-week grace period, so the beneficiary always gets a fair window to claim before the depositor can walk away with the funds.

> **Note on automation:** because release/refund require the caller's own signature, they can't be executed by an unattended keeper the way subscriptions can — unless the keeper *is* the configured arbiter. The [backend](https://github.com/StellarSend/backend)'s escrow endpoints build an unsigned transaction for whichever party is acting to sign client-side, the same pattern used for a regular payment.

---

## Getting Started

### Prerequisites

| Tool | Version | Install |
|---|---|---|
| Rust | stable (see `rust-toolchain.toml`) | `curl https://sh.rustup.rs -sSf \| sh` |
| Soroban CLI | ≥ 21.0.0 | `cargo install --locked soroban-cli` |
| wasm32 target | — | `rustup target add wasm32-unknown-unknown` |

### Clone and install

```bash
git clone https://github.com/StellarSend/contracts.git
cd contracts
```

The workspace `Cargo.toml` pins all dependency versions. No additional install step is required.

---

## Building

Build all contracts to WASM:

```bash
make build
# or directly:
bash scripts/build.sh
```

Individual contract:

```bash
soroban contract build --package stellar_send
soroban contract build --package fee_collector
soroban contract build --package token_bridge
```

Compiled WASM files are written to `target/wasm32-unknown-unknown/release/`.

---

## Testing

Run the full test suite:

```bash
make test
# or:
bash scripts/test.sh
```

Individual contract tests:

```bash
cargo test -p stellar_send
cargo test -p fee_collector
cargo test -p token_bridge
```

Run with output:

```bash
cargo test -p stellar_send -- --nocapture
```

---

## Deployment

`fee_collector`, `token_bridge`, and `stellar_send` are deployed and
initialized exclusively through the **`factory`** contract (`contracts/factory/`).
Deploying and initializing a Soroban contract are two independently-authorized
steps; calling any of these three contracts' own `initialize` directly against
a raw, independently deployed instance is front-runnable — anyone watching the
ledger for the deploy can call `initialize` first and permanently seize
`admin` (#58). `factory` closes this by deploying and initializing each
instance atomically within a single host invocation, so no
deployed-but-uninitialized state is ever observable on-chain. See
`contracts/factory/src/lib.rs`'s module doc comment for the full rationale,
and each contract's own `initialize` doc comment for why it can't defend
itself against this directly (the pinned `soroban-sdk` gives it no way to).

### Testnet

```bash
# Set environment
export STELLAR_NETWORK=testnet
export STELLAR_RPC_URL=https://soroban-testnet.stellar.org
export STELLAR_NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
export STELLAR_ACCOUNT=<your-secret-key-or-identity>

# Deploys factory, then fee_collector, token_bridge, and stellar_send
# through it — see scripts/deploy.sh for the exact factory calls made.
bash scripts/deploy.sh [treasury_address] [underlying_token_address]
```

### Mainnet

Same steps as testnet; update `STELLAR_NETWORK=mainnet`, `STELLAR_RPC_URL`,
and `STELLAR_NETWORK_PASSPHRASE` for mainnet.

> **Note:** Mainnet deployments require a multi-sig admin key. Refer to the [Security](#security) section. Deploying and initializing the `factory` contract itself remains two separate steps — see its module doc comment for why that residual window is accepted.

---

## Contract API Reference

### stellar\_send

#### `initialize(admin, fee_bps, fee_collector) → Result<(), StellarSendError>`

Initialise the contract. Must be called exactly once by `admin`.

- `fee_bps`: Protocol fee in basis points. Range: `0..=MAX_FEE_BPS` (currently 1,000, i.e. 10% max).
- `fee_collector`: Address of the deployed `fee_collector` contract.

> **Deploy via `factory` only** — see [Deployment](#deployment). Calling this directly against a raw deployed instance is front-runnable (#58).

#### `send_payment(from, to, token, amount, memo) → Result<PaymentRecord, StellarSendError>`

Transfer `amount` of `token` from `from` to `to`. The protocol fee is deducted from `amount` before transfer.

**Requires auth from:** `from`

#### `send_path_payment(from, to, send_token, send_amount, dest_token, min_dest_amount, path) → Result<i128, StellarSendError>`

Route a payment through the Stellar DEX. The fee is deducted from `send_amount` before the swap is attempted.

**Requires auth from:** `from`

#### `set_fee(new_fee_bps) → Result<(), StellarSendError>`

Update the protocol fee. **Admin only.**

#### `pause() / unpause() → Result<(), StellarSendError>`

Halt or resume new payments. **Admin only.**

#### `transfer_admin(new_admin) → Result<(), StellarSendError>`

Propose a new admin (step 1 of 2). **Current admin only.**

#### `accept_admin() → Result<(), StellarSendError>`

Complete the admin rotation (step 2 of 2). **Proposed admin only.**

#### `get_config() → Result<ContractConfig, StellarSendError>`

Return current configuration.

#### `get_payment_record(from, seq) → Result<PaymentRecord, StellarSendError>`

Retrieve a stored payment record by sender address and sequence number.
Returns `NotInitialized` if the contract itself has never been
initialized, or `PaymentRecordNotFound` if it has, but no record exists
for the given `(from, seq)` pair.

---

### fee\_collector

#### `initialize(admin, treasury) → Result<(), FeeCollectorError>`

> **Deploy via `factory` only** — see [Deployment](#deployment). Calling this directly against a raw deployed instance is front-runnable (#58), and this contract has no admin-rotation mechanism (#40), so that has no recovery path short of a full redeploy.

#### `collect_fee(token, amount) → Result<(), FeeCollectorError>`

Record fee receipt. The token transfer must already have been made by `stellar_send`.

#### `withdraw(token, amount, recipient) → Result<(), FeeCollectorError>`

Withdraw fees to `recipient`. **Admin only.**
Amounts above the contract's actual balance return
`FeeCollectorError::InsufficientBalance`; lifetime collected-fee totals are
not used as the available balance.

#### `get_balance(token) → i128`

Return current token balance held by the contract.

#### `get_total_collected(token) → i128`

Return the lifetime total of fees collected in `token`.

---

### token\_bridge

#### `initialize(admin, underlying_token) → Result<(), TokenBridgeError>`

> **Deploy via `factory` only** — see [Deployment](#deployment). Calling this directly against a raw deployed instance is front-runnable (#58).

#### `wrap(from, amount) → Result<i128, TokenBridgeError>`

Deposit `amount` of underlying token; credit wrapped balance.

Measures the contract's actual underlying-token balance gain around the
transfer rather than trusting `amount` — a fee-on-transfer or deflationary
underlying token would otherwise silently under-fund the single shared
underlying pool every wrapper's `unwrap` draws from. If the measured
delta doesn't match `amount`, the call is rejected with
`UnderlyingTransferShortfall` (and, since a `Result::Err` from a contract
entry point rolls back the whole invocation, the already-executed
transfer is reverted too — the caller keeps their original balance).
`underlying_token` is fully caller-chosen at `initialize`; this bridge
does not attempt to support fee-on-transfer/deflationary tokens, it only
refuses to let one wrapper's misbehaving-token deposit create a shortfall
a different, honest wrapper's `unwrap` would later fall into.

#### `unwrap(from, amount) → Result<i128, TokenBridgeError>`

Burn `amount` of wrapped balance; return underlying tokens.

#### `get_wrapped_balance(holder) → i128`

#### `get_underlying_token() → Result<Address, TokenBridgeError>`

---

## Events

All contracts emit structured events consumable by the Horizon API or Soroban event subscriptions.

| Contract | Topic | Data |
|---|---|---|
| stellar_send | `payment_sent` | `(from, to, token, net_amount, fee_amount, memo)` |
| stellar_send | `path_payment_sent` | `(from, to, send_token, send_amount, dest_token, dest_amount, fee_amount)` |
| stellar_send | `fee_updated` | `(old_bps, new_bps)` |
| stellar_send | `paused / unpaused` | `ledger` |
| fee_collector | `fee_received` | `(token, amount)` |
| fee_collector | `fee_withdrawn` | `(token, recipient, amount)` |
| token_bridge | `wrapped` | `(underlying, amount)` |
| token_bridge | `unwrapped` | `(underlying, amount)` |

Subscribe to events via Horizon:

```bash
soroban events --contract-id <CONTRACT_ID> --network testnet
```

---

## Security

### Admin key management

- All admin keys should be multi-sig accounts on mainnet.
- Admin rotation uses a two-step propose/accept pattern to prevent key loss.
- The `fee_collector` treasury address is separate from the admin key.
- `fee_collector`, `token_bridge`, and `stellar_send` must be deployed
  exclusively through the `factory` contract — see [Deployment](#deployment).
  Deploying any of them any other way reopens the front-runnable
  `initialize()` window described in #58.

### Audit status

| Contract | Auditor | Status |
|---|---|---|
| stellar\_send | — | In progress |
| fee\_collector | — | In progress |
| token\_bridge | — | In progress |
| factory | — | In progress |

### Reporting vulnerabilities

Please report security issues privately via email to **security@stellarsend.io**. Do **not** open a public issue for a security vulnerability. See [SECURITY.md](SECURITY.md) for our responsible disclosure policy.

---

## Contributing

We welcome contributions! Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

1. Fork the repo
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes and add tests
4. Run `make test` — all tests must pass
5. Open a PR against `main`

---

## License

MIT © 2026 StellarSend. See [LICENSE](LICENSE) for full text.
