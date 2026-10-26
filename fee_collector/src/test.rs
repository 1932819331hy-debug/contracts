//! Tests for the FeeCollector contract.

#![cfg(test)]

use soroban_sdk::{
    testutils::Address as _,
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

use crate::{FeeCollectorContract, FeeCollectorContractClient, FeeCollectorError};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (
    Env,
    FeeCollectorContractClient<'static>,
    Address, // admin
    Address, // treasury
    Address, // token
    Address, // token_admin
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let contract_id = env.register_contract(None, FeeCollectorContract);
    let client = FeeCollectorContractClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = token_id.address();

    (env, client, admin, treasury, token, token_admin)
}

fn mint(env: &Env, token: &Address, admin: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(to, &amount);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_initialize() {
    let (env, client, admin, treasury, _token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_treasury(), treasury);
}

#[test]
fn test_initialize_already_initialized() {
    let (env, client, admin, treasury, _token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    let result = client.try_initialize(&admin, &treasury);
    assert_eq!(result, Err(Ok(FeeCollectorError::AlreadyInitialized)));
}

#[test]
fn test_collect_fee_updates_total() {
    let (env, client, admin, treasury, token, token_admin) = setup();
    client.initialize(&admin, &treasury);

    let contract_id = client.address.clone();
    // Simulate StellarSend depositing the fee.
    mint(&env, &token, &token_admin, &contract_id, 100);

    client.collect_fee(&token, &100i128);
    assert_eq!(client.get_total_collected(&token), 100);

    mint(&env, &token, &token_admin, &contract_id, 50);
    client.collect_fee(&token, &50i128);
    assert_eq!(client.get_total_collected(&token), 150);
}

#[test]
fn test_get_balance_reflects_token_balance() {
    let (env, client, admin, treasury, token, token_admin) = setup();
    client.initialize(&admin, &treasury);

    let contract_id = client.address.clone();
    mint(&env, &token, &token_admin, &contract_id, 500);

    assert_eq!(client.get_balance(&token), 500);
}

#[test]
fn test_withdraw_sends_tokens_to_recipient() {
    let (env, client, admin, treasury, token, token_admin) = setup();
    client.initialize(&admin, &treasury);

    let contract_id = client.address.clone();
    mint(&env, &token, &token_admin, &contract_id, 300);
    client.collect_fee(&token, &300i128);

    let recipient = Address::generate(&env);
    client.withdraw(&token, &200i128, &recipient);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&recipient), 200);
    assert_eq!(token_client.balance(&contract_id), 100);
}

#[test]
fn test_withdraw_rejects_amount_exceeding_actual_balance() {
    let (env, client, admin, treasury, token, token_admin) = setup();
    client.initialize(&admin, &treasury);

    let contract_id = client.address.clone();
    mint(&env, &token, &token_admin, &contract_id, 100);
    // Reported lifetime fees are not proof that the tokens are available.
    client.collect_fee(&token, &1_000i128);

    let recipient = Address::generate(&env);
    let result = client.try_withdraw(&token, &101i128, &recipient);
    assert_eq!(result, Err(Ok(FeeCollectorError::InsufficientBalance)));

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&recipient), 0);
    assert_eq!(token_client.balance(&contract_id), 100);
    assert_eq!(client.get_total_collected(&token), 1_000);
}

#[test]
fn test_withdraw_can_empty_the_actual_balance() {
    let (env, client, admin, treasury, token, token_admin) = setup();
    client.initialize(&admin, &treasury);

    let contract_id = client.address.clone();
    mint(&env, &token, &token_admin, &contract_id, 100);
    // A direct deposit is withdrawable even without a collect_fee record.
    assert_eq!(client.get_total_collected(&token), 0);

    let recipient = Address::generate(&env);
    client.withdraw(&token, &100i128, &recipient);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&recipient), 100);
    assert_eq!(token_client.balance(&contract_id), 0);
    assert_eq!(client.get_total_collected(&token), 0);

    let result = client.try_withdraw(&token, &1i128, &recipient);
    assert_eq!(result, Err(Ok(FeeCollectorError::InsufficientBalance)));
    assert_eq!(token_client.balance(&recipient), 100);
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_withdraw_requires_admin_auth_before_balance_validation() {
    for balance in [0, 100] {
        let (env, client, admin, treasury, token, token_admin) = setup();
        client.initialize(&admin, &treasury);
        let contract_id = client.address.clone();
        if balance > 0 {
            mint(&env, &token, &token_admin, &contract_id, balance);
        }

        let recipient = Address::generate(&env);
        env.mock_auths(&[]);
        let result = client.try_withdraw(&token, &10i128, &recipient);
        // Missing authorization must not become an observable balance error.
        assert_eq!(result, Err(Err(soroban_sdk::InvokeError::Abort)));

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(token_client.balance(&recipient), 0);
        assert_eq!(token_client.balance(&contract_id), balance);
    }
}

#[test]
fn test_withdraw_invalid_amount() {
    let (env, client, admin, treasury, token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    let recipient = Address::generate(&env);
    let result = client.try_withdraw(&token, &0i128, &recipient);
    assert_eq!(result, Err(Ok(FeeCollectorError::InvalidAmount)));
}

#[test]
fn test_withdraw_rejects_invalid_amount_before_querying_token() {
    let (env, client, admin, treasury, _token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    let unknown_token = Address::generate(&env);
    let recipient = Address::generate(&env);
    for amount in [0, -1] {
        let result = client.try_withdraw(&unknown_token, &amount, &recipient);
        assert_eq!(result, Err(Ok(FeeCollectorError::InvalidAmount)));
    }
}

#[test]
fn test_collect_fee_invalid_amount() {
    let (env, client, admin, treasury, token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    let result = client.try_collect_fee(&token, &0i128);
    assert_eq!(result, Err(Ok(FeeCollectorError::InvalidAmount)));
}

#[test]
fn test_set_treasury() {
    let (env, client, admin, treasury, _token, _token_admin) = setup();
    client.initialize(&admin, &treasury);

    let new_treasury = Address::generate(&env);
    client.set_treasury(&new_treasury);
    assert_eq!(client.get_treasury(), new_treasury);
}

#[test]
fn test_get_total_collected_starts_at_zero() {
    let (env, client, admin, treasury, token, _token_admin) = setup();
    client.initialize(&admin, &treasury);
    assert_eq!(client.get_total_collected(&token), 0);
}

#[test]
fn test_not_initialized_errors() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, FeeCollectorContract);
    let client = FeeCollectorContractClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = token_id.address();

    let result = client.try_collect_fee(&token, &10i128);
    assert_eq!(result, Err(Ok(FeeCollectorError::NotInitialized)));
}
