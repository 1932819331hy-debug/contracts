//! # FeeCollector Contract
//!
//! Receives protocol fees forwarded by the `StellarSend` contract and allows
//! the treasury admin to withdraw accumulated balances.
//!
//! Storage layout
//! ──────────────
//! Instance storage:
//!   KEY_ADMIN    → Address
//!   KEY_TREASURY → Address
//!
//! Persistent storage (keyed by token address):
//!   (KEY_TOTAL, token) → i128   — lifetime total collected
//!
//! The actual token balances are tracked by the token contracts themselves;
//! `get_balance` queries the token contract directly.

#![no_std]

use soroban_sdk::{
    contract, contractimpl, symbol_short, token, Address, Env, Symbol,
};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

const KEY_ADMIN: Symbol = symbol_short!("ADMIN");
const KEY_TREASURY: Symbol = symbol_short!("TREASURY");
const KEY_INIT: Symbol = symbol_short!("INIT");

/// Persistent key prefix for lifetime-total-collected per token.
const KEY_TOTAL: Symbol = symbol_short!("TOTAL");

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum FeeCollectorError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    ArithmeticOverflow = 5,
    InsufficientBalance = 6,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct FeeCollectorContract;

#[contractimpl]
impl FeeCollectorContract {
    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Initialise the fee-collector.
    ///
    /// * `admin`    – Can call `withdraw` and update the treasury.
    /// * `treasury` – Default recipient of withdrawn fees.
    ///
    /// # Security: deploy exclusively through `factory` (#58)
    ///
    /// This function cannot verify *who* is calling it — the pinned
    /// `soroban-sdk` (21.7.7) has neither constructor support nor any API
    /// for a contract to learn who deployed it, so there is nothing here to
    /// check a caller against. Called directly against a raw, independently
    /// deployed instance, `initialize` is front-runnable: anyone watching
    /// the ledger for the deploy can call it first and seize `admin`
    /// permanently — and per #40, this contract has no admin-rotation
    /// mechanism at all, so that seizure has no recovery path short of a
    /// full redeploy. Instances MUST be deployed exclusively through the
    /// `factory` contract's `deploy_fee_collector`, which deploys and calls
    /// this function atomically within a single host invocation, so no
    /// externally-observable deployed-but-uninitialized state ever exists.
    /// See `factory`'s module doc comment for the full rationale.
    pub fn initialize(
        env: Env,
        admin: Address,
        treasury: Address,
    ) -> Result<(), FeeCollectorError> {
        if env.storage().instance().has(&KEY_INIT) {
            return Err(FeeCollectorError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&KEY_ADMIN, &admin);
        env.storage().instance().set(&KEY_TREASURY, &treasury);
        env.storage().instance().set(&KEY_INIT, &true);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Called by StellarSend (or any authorised caller)
    // -----------------------------------------------------------------------

    /// Record that `amount` of `token` has been collected as a fee.
    ///
    /// The actual token transfer must have already occurred (StellarSend
    /// transfers the fee to this contract's address before calling this).
    /// This function merely updates the lifetime accounting counter.
    pub fn collect_fee(
        env: Env,
        token: Address,
        amount: i128,
    ) -> Result<(), FeeCollectorError> {
        Self::assert_initialized(&env)?;

        if amount <= 0 {
            return Err(FeeCollectorError::InvalidAmount);
        }

        // Update lifetime total for this token.
        let total_key = (KEY_TOTAL, token.clone());
        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&total_key)
            .unwrap_or(0i128);
        let new_total = current_total
            .checked_add(amount)
            .ok_or(FeeCollectorError::ArithmeticOverflow)?;
        env.storage().persistent().set(&total_key, &new_total);

        // Emit event.
        env.events().publish(
            (symbol_short!("fee_rcvd"), token),
            amount,
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin operations
    // -----------------------------------------------------------------------

    /// Withdraw `amount` of `token` from this contract to `recipient`.
    /// Only the admin may call this.
    /// Returns `InsufficientBalance` if the actual token balance is too small.
    pub fn withdraw(
        env: Env,
        token: Address,
        amount: i128,
        recipient: Address,
    ) -> Result<(), FeeCollectorError> {
        Self::assert_initialized(&env)?;

        let admin: Address = env
            .storage()
            .instance()
            .get(&KEY_ADMIN)
            .ok_or(FeeCollectorError::NotInitialized)?;
        admin.require_auth();

        if amount <= 0 {
            return Err(FeeCollectorError::InvalidAmount);
        }

        // Transfer from this contract to recipient.
        let token_client = token::Client::new(&env, &token);
        if amount > token_client.balance(&env.current_contract_address()) {
            return Err(FeeCollectorError::InsufficientBalance);
        }
        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        // Emit event.
        env.events().publish(
            (symbol_short!("fee_wdrw"), token, recipient),
            amount,
        );

        Ok(())
    }

    /// Update the treasury address.  Only admin may call this.
    pub fn set_treasury(env: Env, new_treasury: Address) -> Result<(), FeeCollectorError> {
        Self::assert_initialized(&env)?;
        let admin: Address = env
            .storage()
            .instance()
            .get(&KEY_ADMIN)
            .ok_or(FeeCollectorError::NotInitialized)?;
        admin.require_auth();
        env.storage().instance().set(&KEY_TREASURY, &new_treasury);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Return the current token balance held by this contract.
    pub fn get_balance(env: Env, token: Address) -> i128 {
        let token_client = token::Client::new(&env, &token);
        token_client.balance(&env.current_contract_address())
    }

    /// Return the lifetime total amount of `token` ever collected as fees.
    pub fn get_total_collected(env: Env, token: Address) -> i128 {
        let total_key = (KEY_TOTAL, token);
        env.storage()
            .persistent()
            .get(&total_key)
            .unwrap_or(0i128)
    }

    /// Return the admin address.
    pub fn get_admin(env: Env) -> Result<Address, FeeCollectorError> {
        env.storage()
            .instance()
            .get(&KEY_ADMIN)
            .ok_or(FeeCollectorError::NotInitialized)
    }

    /// Return the treasury address.
    pub fn get_treasury(env: Env) -> Result<Address, FeeCollectorError> {
        env.storage()
            .instance()
            .get(&KEY_TREASURY)
            .ok_or(FeeCollectorError::NotInitialized)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn assert_initialized(env: &Env) -> Result<(), FeeCollectorError> {
        if !env.storage().instance().has(&KEY_INIT) {
            return Err(FeeCollectorError::NotInitialized);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test;
