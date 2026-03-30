#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::storage::Instance as StorageInstance,
    testutils::{Address as AddressTrait, Events, Ledger, LedgerInfo},
    token::{StellarAssetClient, TokenClient},
    Address, Env, Symbol, TryFromVal, TryIntoVal,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Register a native Stellar asset (SAC) and return (contract_id, admin).
/// The admin is the issuer; we mint `amount` to `recipient`.
fn setup_token(env: &Env, admin: &Address, recipient: &Address, amount: i128) -> Address {
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = StellarAssetClient::new(env, &token_id);
    sac.mint(recipient, &amount);
    token_id
}

/// Build a fresh AccountGroup with four distinct addresses.
fn make_accounts(env: &Env) -> AccountGroup {
    AccountGroup {
        spending: Address::generate(env),
        savings: Address::generate(env),
        bills: Address::generate(env),
        insurance: Address::generate(env),
    }
}

/// Set a deterministic ledger timestamp for schedule-related tests.
fn set_test_ledger(env: &Env, timestamp: u64) {
    env.ledger().set(LedgerInfo {
        protocol_version: 20,
        sequence_number: 100,
        timestamp,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: 1,
        max_entry_ttl: 100_000,
    });
}

/// Register and initialize a fresh split contract with the default 50/30/15/5 allocation.
fn setup_initialized_split<'a>(
    env: &'a Env,
    initial_balance: i128,
) -> (RemittanceSplitClient<'a>, Address, Address) {
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(env, &contract_id);
    let owner = Address::generate(env);
    let token_admin = Address::generate(env);
    let token_id = setup_token(env, &token_admin, &owner, initial_balance);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    (client, owner, token_id)
}

/// Compute the request hash for a `distribute_usdc` call.
/// Uses the same formula as `RemittanceSplit::compute_request_hash`.
fn distrib_hash(nonce: u64, amount: i128, deadline: u64) -> u64 {
    let op_bits: u64 = symbol_short!("distrib").to_val().get_payload();
    let amt_lo = amount as u64;
    let amt_hi = (amount >> 64) as u64;
    op_bits
        .wrapping_add(nonce)
        .wrapping_add(amt_lo)
        .wrapping_add(amt_hi)
        .wrapping_add(deadline)
        .wrapping_mul(1_000_000_007)
}

/// Default deadline for distribute_usdc tests: 1 hour from ledger time 0.
const TEST_DEADLINE: u64 = 3600;

// ---------------------------------------------------------------------------
// initialize_split
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_split_domain_separated_auth() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Verify that the authorization was captured for the owner address.
    // The contract calls owner.require_auth_for_args(payload) which binds
    // the authorization to the domain-separated SplitAuthPayload.
    let auths = env.auths();
    // At least one auth should have been captured (for the owner)
    assert!(!auths.is_empty());
    // The first auth should be for the owner address
    let (address, _auth_invocation) = auths.get(0).unwrap();
    // Verify the auth was captured for the owner
    assert!(*address == owner);
}

#[test]
fn test_initialize_split_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    let success = client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    assert_eq!(success, true);

    let config = client.get_config().unwrap();
    assert_eq!(config.owner, owner);
    assert_eq!(config.spending_percent, 50);
    assert_eq!(config.savings_percent, 30);
    assert_eq!(config.bills_percent, 15);
    assert_eq!(config.insurance_percent, 5);
    assert_eq!(config.usdc_contract, token_id);
}

#[test]
fn test_initialize_split_invalid_sum() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    let result = client.try_initialize_split(&owner, &0, &token_id, &50, &50, &10, &0);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::InvalidPercentages))
    );
}

#[test]
fn test_initialize_split_already_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let result = client.try_initialize_split(&owner, &1, &token_id, &50, &30, &15, &5);
    assert_eq!(result, Err(Ok(RemittanceSplitError::AlreadyInitialized)));
}

#[test]
#[should_panic]
fn test_initialize_split_requires_auth() {
    let env = Env::default();
    // No mock_all_auths — owner has not authorized
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
}

// ---------------------------------------------------------------------------
// update_split
// ---------------------------------------------------------------------------

#[test]
fn test_update_split() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let success = client.update_split(&owner, &1, &40, &40, &10, &10);
    assert_eq!(success, true);

    let config = client.get_config().unwrap();
    assert_eq!(config.spending_percent, 40);
    assert_eq!(config.savings_percent, 40);
    assert_eq!(config.bills_percent, 10);
    assert_eq!(config.insurance_percent, 10);
}

#[test]
fn test_update_split_nonce_increments_and_replay_is_rejected() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);

    client.update_split(&owner, &1, &40, &40, &10, &10);

    assert_eq!(client.get_nonce(&owner), 2);
    let replay = client.try_update_split(&owner, &1, &25, &25, &25, &25);
    assert_eq!(replay, Err(Ok(RemittanceSplitError::InvalidNonce)));
}

#[test]
fn test_update_split_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let other = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let result = client.try_update_split(&other, &0, &40, &40, &10, &10);
    assert_eq!(result, Err(Ok(RemittanceSplitError::Unauthorized)));
}

#[test]
fn test_update_split_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let result = client.try_update_split(&caller, &0, &25, &25, &25, &25);
    assert_eq!(result, Err(Ok(RemittanceSplitError::NotInitialized)));
}

#[test]
fn test_update_split_percentages_must_sum_to_100() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let result = client.try_update_split(&owner, &1, &60, &30, &15, &5);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::InvalidPercentages))
    );
}

#[test]
fn test_update_split_paused_rejected_and_unpause_restores_access() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);

    client.pause(&owner);
    let paused = client.try_update_split(&owner, &1, &40, &40, &10, &10);
    assert_eq!(paused, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&owner);
    client.update_split(&owner, &1, &40, &40, &10, &10);

    let config = client.get_config().unwrap();
    assert_eq!(config.spending_percent, 40);
    assert_eq!(config.savings_percent, 40);
    assert_eq!(config.bills_percent, 10);
    assert_eq!(config.insurance_percent, 10);
}

// ---------------------------------------------------------------------------
// Pause controls
// ---------------------------------------------------------------------------

#[test]
fn test_pause_rejects_unauthorized_caller() {
    let env = Env::default();
    let (client, _owner, _token_id) = setup_initialized_split(&env, 0);
    let attacker = Address::generate(&env);

    let result = client.try_pause(&attacker);
    assert_eq!(result, Err(Ok(RemittanceSplitError::Unauthorized)));
    assert!(!client.is_paused());
}

#[test]
fn test_pause_admin_transfer_is_blocked_while_paused_and_restored_after_unpause() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);
    let delegated_admin = Address::generate(&env);

    client.set_pause_admin(&owner, &delegated_admin);

    let old_admin_pause = client.try_pause(&owner);
    assert_eq!(old_admin_pause, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.pause(&delegated_admin);
    assert!(client.is_paused());

    let repeated_pause = client.try_pause(&delegated_admin);
    assert_eq!(repeated_pause, Err(Ok(RemittanceSplitError::Unauthorized)));

    let paused_transfer = client.try_set_pause_admin(&owner, &owner);
    assert_eq!(paused_transfer, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&delegated_admin);
    client.set_pause_admin(&owner, &owner);

    client.pause(&owner);
    assert!(client.is_paused());
    client.unpause(&owner);
    assert!(!client.is_paused());
}

#[test]
fn test_calculate_split_remains_available_while_paused() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);

    client.pause(&owner);

    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 500);
    assert_eq!(amounts.get(1).unwrap(), 300);
    assert_eq!(amounts.get(2).unwrap(), 150);
    assert_eq!(amounts.get(3).unwrap(), 50);
}

// ---------------------------------------------------------------------------
// calculate_split
// ---------------------------------------------------------------------------

#[test]
fn test_calculate_split() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 500);
    assert_eq!(amounts.get(1).unwrap(), 300);
    assert_eq!(amounts.get(2).unwrap(), 150);
    assert_eq!(amounts.get(3).unwrap(), 50);
}

#[test]
fn test_calculate_split_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let result = client.try_calculate_split(&0);
    assert_eq!(result, Err(Ok(RemittanceSplitError::InvalidAmount)));
}

#[test]
fn test_calculate_split_rounding() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &33, &33, &33, &1);
    let amounts = client.calculate_split(&100);
    let sum: i128 = amounts.iter().sum();
    assert_eq!(sum, 100);
}

#[test]
fn test_calculate_complex_rounding() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &17, &19, &23, &41);
    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 170);
    assert_eq!(amounts.get(1).unwrap(), 190);
    assert_eq!(amounts.get(2).unwrap(), 230);
    assert_eq!(amounts.get(3).unwrap(), 410);
}

// ---------------------------------------------------------------------------
// distribute_usdc — happy path
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let total = 1_000i128;
    let token_id = setup_token(&env, &token_admin, &owner, total);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let accounts = make_accounts(&env);
    let result = client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, total, TEST_DEADLINE), &accounts, &total);
    assert_eq!(result, true);

    let token = TokenClient::new(&env, &token_id);
    assert_eq!(token.balance(&accounts.spending), 500);
    assert_eq!(token.balance(&accounts.savings), 300);
    assert_eq!(token.balance(&accounts.bills), 150);
    assert_eq!(token.balance(&accounts.insurance), 50);
    assert_eq!(token.balance(&owner), 0);
}

#[test]
fn test_distribute_usdc_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let accounts = make_accounts(&env);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);

    let events = env.events().all();
    let last = events.last().unwrap();
    // Events are emitted via RemitwiseEvents::emit with topics:
    // ("Remitwise", category_u32, priority_u32, action_symbol)
    let topic0: Symbol = Symbol::try_from_val(&env, &last.1.get(0).unwrap()).unwrap();
    assert_eq!(topic0, symbol_short!("Remitwise"));
}

#[test]
fn test_distribute_usdc_nonce_increments() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 2_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    // nonce after init = 1
    let accounts = make_accounts(&env);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);
    // nonce after first distribute = 2
    assert_eq!(client.get_nonce(&owner), 2);
}

// ---------------------------------------------------------------------------
// distribute_usdc — auth must be first (before amount check)
// ---------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_distribute_usdc_requires_auth() {
    // Set up contract state with a mocked env first
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Now call distribute_usdc without mocking auth for `owner` — should panic
    // We create a fresh env that does NOT mock auths
    let env2 = Env::default();
    // Re-register the same contract in env2 (no mock_all_auths)
    let contract_id2 = env2.register_contract(None, RemittanceSplit);
    let client2 = RemittanceSplitClient::new(&env2, &contract_id2);
    let accounts = make_accounts(&env2);
    // This should panic because owner has not authorized in env2
    client2.distribute_usdc(&token_id, &owner, &0, &TEST_DEADLINE, &0u64, &accounts, &1_000);
}

// ---------------------------------------------------------------------------
// distribute_usdc — owner-only enforcement
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_non_owner_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let attacker = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Attacker self-authorizes but is not the config owner
    let accounts = make_accounts(&env);
    let result = client.try_distribute_usdc(&token_id, &attacker, &0, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(result, Err(Ok(RemittanceSplitError::Unauthorized)));
}

// ---------------------------------------------------------------------------
// distribute_usdc — untrusted token contract
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_untrusted_token_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Supply a different (malicious) token contract address
    let evil_token = Address::generate(&env);
    let accounts = make_accounts(&env);
    let result = client.try_distribute_usdc(&evil_token, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::UntrustedTokenContract))
    );
}

// ---------------------------------------------------------------------------
// distribute_usdc — self-transfer guard
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_self_transfer_spending_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // spending destination == owner
    let accounts = AccountGroup {
        spending: owner.clone(),
        savings: Address::generate(&env),
        bills: Address::generate(&env),
        insurance: Address::generate(&env),
    };
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SelfTransferNotAllowed))
    );
}

#[test]
fn test_distribute_usdc_self_transfer_savings_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let accounts = AccountGroup {
        spending: Address::generate(&env),
        savings: owner.clone(),
        bills: Address::generate(&env),
        insurance: Address::generate(&env),
    };
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SelfTransferNotAllowed))
    );
}

#[test]
fn test_distribute_usdc_self_transfer_bills_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let accounts = AccountGroup {
        spending: Address::generate(&env),
        savings: Address::generate(&env),
        bills: owner.clone(),
        insurance: Address::generate(&env),
    };
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SelfTransferNotAllowed))
    );
}

#[test]
fn test_distribute_usdc_self_transfer_insurance_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let accounts = AccountGroup {
        spending: Address::generate(&env),
        savings: Address::generate(&env),
        bills: Address::generate(&env),
        insurance: owner.clone(),
    };
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SelfTransferNotAllowed))
    );
}

// ---------------------------------------------------------------------------
// distribute_usdc — invalid amount
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_zero_amount_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let accounts = make_accounts(&env);
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &0);
    assert_eq!(result, Err(Ok(RemittanceSplitError::InvalidAmount)));
}

#[test]
fn test_distribute_usdc_negative_amount_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let accounts = make_accounts(&env);
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &-1);
    assert_eq!(result, Err(Ok(RemittanceSplitError::InvalidAmount)));
}

// ---------------------------------------------------------------------------
// distribute_usdc — not initialized
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_not_initialized_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);

    let accounts = make_accounts(&env);
    let result = client.try_distribute_usdc(&token_id, &owner, &0, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(result, Err(Ok(RemittanceSplitError::NotInitialized)));
}

// ---------------------------------------------------------------------------
// distribute_usdc — replay protection
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_replay_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 2_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let accounts = make_accounts(&env);
    // First call with nonce=1 succeeds
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);
    // Replaying nonce=1 must fail
    let result = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &500);
    assert_eq!(result, Err(Ok(RemittanceSplitError::InvalidNonce)));
}

// ---------------------------------------------------------------------------
// distribute_usdc — paused contract
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_paused_rejected_and_unpause_restores_access() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    client.pause(&owner);

    let accounts = make_accounts(&env);
    let paused = client.try_distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &0u64, &accounts, &1_000);
    assert_eq!(paused, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&owner);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);

    let token = TokenClient::new(&env, &token_id);
    assert_eq!(token.balance(&accounts.spending), 500);
    assert_eq!(token.balance(&accounts.savings), 300);
    assert_eq!(token.balance(&accounts.bills), 150);
    assert_eq!(token.balance(&accounts.insurance), 50);
}

// ---------------------------------------------------------------------------
// distribute_usdc — correct split math verified end-to-end
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_split_math_25_25_25_25() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &25, &25, &25, &25);
    let accounts = make_accounts(&env);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);

    let token = TokenClient::new(&env, &token_id);
    assert_eq!(token.balance(&accounts.spending), 250);
    assert_eq!(token.balance(&accounts.savings), 250);
    assert_eq!(token.balance(&accounts.bills), 250);
    assert_eq!(token.balance(&accounts.insurance), 250);
}

#[test]
fn test_distribute_usdc_split_math_100_0_0_0() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 1_000);

    client.initialize_split(&owner, &0, &token_id, &100, &0, &0, &0);
    let accounts = make_accounts(&env);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);

    let token = TokenClient::new(&env, &token_id);
    assert_eq!(token.balance(&accounts.spending), 1_000);
    assert_eq!(token.balance(&accounts.savings), 0);
    assert_eq!(token.balance(&accounts.bills), 0);
    assert_eq!(token.balance(&accounts.insurance), 0);
}

#[test]
fn test_distribute_usdc_rounding_remainder_goes_to_insurance() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    // 33/33/33/1 with amount=100: 33+33+33=99, insurance gets remainder=1
    let token_id = setup_token(&env, &token_admin, &owner, 100);

    client.initialize_split(&owner, &0, &token_id, &33, &33, &33, &1);
    let accounts = make_accounts(&env);
    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 100, TEST_DEADLINE), &accounts, &100);

    let token = TokenClient::new(&env, &token_id);
    let total = token.balance(&accounts.spending)
        + token.balance(&accounts.savings)
        + token.balance(&accounts.bills)
        + token.balance(&accounts.insurance);
    assert_eq!(total, 100, "all funds must be distributed");
    assert_eq!(token.balance(&accounts.insurance), 1);
}

// ---------------------------------------------------------------------------
// distribute_usdc — multiple sequential distributions
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_usdc_multiple_rounds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 3_000);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let accounts = make_accounts(&env);

    client.distribute_usdc(&token_id, &owner, &1, &TEST_DEADLINE, &distrib_hash(1, 1_000, TEST_DEADLINE), &accounts, &1_000);
    client.distribute_usdc(&token_id, &owner, &2, &TEST_DEADLINE, &distrib_hash(2, 1_000, TEST_DEADLINE), &accounts, &1_000);
    client.distribute_usdc(&token_id, &owner, &3, &TEST_DEADLINE, &distrib_hash(3, 1_000, TEST_DEADLINE), &accounts, &1_000);

    let token = TokenClient::new(&env, &token_id);
    assert_eq!(token.balance(&accounts.spending), 1_500); // 3 * 500
    assert_eq!(token.balance(&accounts.savings), 900); // 3 * 300
    assert_eq!(token.balance(&accounts.bills), 450); // 3 * 150
    assert_eq!(token.balance(&accounts.insurance), 150); // 3 * 50
    assert_eq!(token.balance(&owner), 0);
}

// ---------------------------------------------------------------------------
// Boundary tests for split percentages
// ---------------------------------------------------------------------------

#[test]
fn test_split_boundary_100_0_0_0() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    let ok = client.initialize_split(&owner, &0, &token_id, &100, &0, &0, &0);
    assert!(ok);
    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 1000);
    assert_eq!(amounts.get(3).unwrap(), 0);
}

#[test]
fn test_split_boundary_0_0_0_100() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    let ok = client.initialize_split(&owner, &0, &token_id, &0, &0, &0, &100);
    assert!(ok);
    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 0);
    assert_eq!(amounts.get(3).unwrap(), 1000);
}

#[test]
fn test_split_boundary_25_25_25_25() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &25, &25, &25, &25);
    let amounts = client.calculate_split(&1000);
    assert_eq!(amounts.get(0).unwrap(), 250);
    assert_eq!(amounts.get(1).unwrap(), 250);
    assert_eq!(amounts.get(2).unwrap(), 250);
    assert_eq!(amounts.get(3).unwrap(), 250);
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_split_events() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic0: Symbol = Symbol::try_from_val(&env, &last_event.1.get(0).unwrap()).unwrap();
    assert_eq!(topic0, symbol_short!("Remitwise"));
}

#[test]
fn test_update_split_events() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    client.update_split(&owner, &1, &40, &40, &10, &10);

    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic1: SplitEvent = SplitEvent::try_from_val(&env, &last_event.1.get(1).unwrap()).unwrap();
    assert_eq!(topic1, SplitEvent::Updated);
}

// ---------------------------------------------------------------------------
// Upgrade and snapshot safety
// ---------------------------------------------------------------------------

#[test]
fn test_upgrade_mutators_paused_rejected_and_unpause_restores_access() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);
    let upgrade_admin = Address::generate(&env);
    let next_upgrade_admin = Address::generate(&env);

    client.set_upgrade_admin(&owner, &upgrade_admin);
    client.pause(&owner);

    let paused_admin_change = client.try_set_upgrade_admin(&owner, &next_upgrade_admin);
    assert_eq!(
        paused_admin_change,
        Err(Ok(RemittanceSplitError::Unauthorized))
    );

    let paused_upgrade = client.try_set_version(&upgrade_admin, &2);
    assert_eq!(paused_upgrade, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&owner);
    client.set_upgrade_admin(&upgrade_admin, &next_upgrade_admin);
    client.set_version(&next_upgrade_admin, &2);

    assert_eq!(client.get_version(), 2);
}

#[test]
fn test_import_snapshot_paused_rejected_and_unpause_restores_access() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);
    let snapshot = client.export_snapshot(&owner).unwrap();

    client.pause(&owner);
    let paused = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(paused, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&owner);
    client.import_snapshot(&owner, &1, &snapshot);

    assert_eq!(client.get_nonce(&owner), 2);
}

// ---------------------------------------------------------------------------
// Remittance schedules
// ---------------------------------------------------------------------------

#[test]
fn test_create_remittance_schedule_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        protocol_version: 20,
        sequence_number: 100,
        timestamp: 1000,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: 1,
        max_entry_ttl: 100_000,
    });

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let schedule_id = client.create_remittance_schedule(&owner, &10000, &3000, &86400);
    assert_eq!(schedule_id, 1);

    let schedule = client.get_remittance_schedule(&schedule_id).unwrap();
    assert_eq!(schedule.amount, 10000);
    assert_eq!(schedule.next_due, 3000);
    assert!(schedule.active);
}

#[test]
fn test_cancel_remittance_schedule() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        protocol_version: 20,
        sequence_number: 100,
        timestamp: 1000,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: 1,
        max_entry_ttl: 100_000,
    });

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let schedule_id = client.create_remittance_schedule(&owner, &10000, &3000, &86400);
    client.cancel_remittance_schedule(&owner, &schedule_id);

    let schedule = client.get_remittance_schedule(&schedule_id).unwrap();
    assert!(!schedule.active);
}

#[test]
fn test_schedule_mutators_paused_rejected_and_unpause_restores_access() {
    let env = Env::default();
    set_test_ledger(&env, 1000);
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);

    let existing_schedule_id = client.create_remittance_schedule(&owner, &10000, &3000, &86400);
    client.pause(&owner);

    let paused_create = client.try_create_remittance_schedule(&owner, &5000, &4000, &0);
    assert_eq!(paused_create, Err(Ok(RemittanceSplitError::Unauthorized)));

    let paused_modify =
        client.try_modify_remittance_schedule(&owner, &existing_schedule_id, &12000, &5000, &0);
    assert_eq!(paused_modify, Err(Ok(RemittanceSplitError::Unauthorized)));

    let paused_cancel = client.try_cancel_remittance_schedule(&owner, &existing_schedule_id);
    assert_eq!(paused_cancel, Err(Ok(RemittanceSplitError::Unauthorized)));

    client.unpause(&owner);

    let new_schedule_id = client.create_remittance_schedule(&owner, &5000, &4000, &0);
    client.modify_remittance_schedule(&owner, &new_schedule_id, &6000, &5000, &172800);
    client.cancel_remittance_schedule(&owner, &existing_schedule_id);

    let new_schedule = client.get_remittance_schedule(&new_schedule_id).unwrap();
    assert_eq!(new_schedule.amount, 6000);
    assert_eq!(new_schedule.next_due, 5000);
    assert_eq!(new_schedule.interval, 172800);
    assert!(new_schedule.active);

    let cancelled_schedule = client
        .get_remittance_schedule(&existing_schedule_id)
        .unwrap();
    assert!(!cancelled_schedule.active);
}

// ---------------------------------------------------------------------------
// TTL extension
// ---------------------------------------------------------------------------

#[test]
fn test_instance_ttl_extended_on_initialize_split() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        protocol_version: 20,
        sequence_number: 100,
        timestamp: 1000,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 100,
        min_persistent_entry_ttl: 100,
        max_entry_ttl: 700_000,
    });

    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = setup_token(&env, &token_admin, &owner, 0);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);
    let ttl = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(
        ttl >= 518_400,
        "TTL must be >= INSTANCE_BUMP_AMOUNT after init"
    );
}

// ============================================================================
// Snapshot schema version tests
//
// These tests verify that:
//  1. export_snapshot embeds the correct schema_version tag.
//  2. import_snapshot accepts any version in MIN_SUPPORTED_SCHEMA_VERSION..=SCHEMA_VERSION.
//  3. import_snapshot rejects a future (too-new) schema version.
//  4. import_snapshot rejects a past (too-old, below min) schema version.
//  5. import_snapshot rejects a tampered checksum regardless of version.
// ============================================================================

#[test]
fn test_export_snapshot_contains_correct_schema_version() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let snapshot = client.export_snapshot(&owner).unwrap();
    assert_eq!(
        snapshot.schema_version, 2,
        "schema_version must equal SCHEMA_VERSION (2)"
    );
}

#[test]
fn test_import_snapshot_current_schema_version_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let snapshot = client.export_snapshot(&owner).unwrap();
    assert_eq!(snapshot.schema_version, 2);

    let ok = client.import_snapshot(&owner, &1, &snapshot);
    assert!(ok, "import with current schema version must succeed");
}

#[test]
fn test_import_snapshot_future_schema_version_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let mut snapshot = client.export_snapshot(&owner).unwrap();
    // Simulate a snapshot produced by a newer contract version.
    snapshot.schema_version = 999;

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::UnsupportedVersion)),
        "future schema_version must be rejected"
    );
}

#[test]
fn test_import_snapshot_too_old_schema_version_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let mut snapshot = client.export_snapshot(&owner).unwrap();
    // Simulate a snapshot too old to import (schema_version 0 < MIN_SUPPORTED_SCHEMA_VERSION 2).
    snapshot.schema_version = 0;

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::UnsupportedVersion)),
        "schema_version below minimum must be rejected"
    );
}

#[test]
fn test_import_snapshot_tampered_checksum_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let mut snapshot = client.export_snapshot(&owner).unwrap();
    snapshot.checksum = snapshot.checksum.wrapping_add(1);

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::ChecksumMismatch)),
        "tampered checksum must be rejected"
    );
}

#[test]
fn test_snapshot_export_import_roundtrip_restores_config() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Update so there is something interesting to round-trip.
    client.update_split(&owner, &1, &40, &40, &10, &10);

    let snapshot = client.export_snapshot(&owner).unwrap();
    assert_eq!(snapshot.schema_version, 2);

    // Nonce is 2 after initialize_split followed by update_split.
    let ok = client.import_snapshot(&owner, &2, &snapshot);
    assert!(ok);

    let config = client.get_config().unwrap();
    assert_eq!(config.spending_percent, 40);
    assert_eq!(config.savings_percent, 40);
    assert_eq!(config.bills_percent, 10);
    assert_eq!(config.insurance_percent, 10);
}

#[test]
fn test_import_snapshot_unauthorized_caller_rejected() {
    let env = Env::default();
    let (client, owner, _token_id) = setup_initialized_split(&env, 0);
    let other = Address::generate(&env);

    let snapshot = client.export_snapshot(&owner).unwrap();

    let result = client.try_import_snapshot(&other, &0, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::Unauthorized)),
        "non-owner must not import snapshot"
    );
}

// ---------------------------------------------------------------------------
// Audit log pagination
// ---------------------------------------------------------------------------

/// Helper: initialize + update N times to seed the audit log with entries.
/// Each initialize produces 1 entry, each update produces 1 entry.
/// Returns (client, owner) for further assertions.
fn seed_audit_log(
    env: &Env,
    count: u32,
) -> (RemittanceSplitClient<'_>, Address) {
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(env, &contract_id);
    let owner = Address::generate(env);
    let token_admin = Address::generate(env);
    let token_id = setup_token(env, &token_admin, &owner, 0);

    // initialize_split appends 1 audit entry on success (nonce 0 → 1)
    client.initialize_split(&owner, &0, &token_id, &25, &25, &25, &25);

    // import_snapshot appends 1 audit entry on success and increments nonce.
    // Use repeated import_snapshot calls to seed additional entries.
    for nonce in 1..count as u64 {
        let snapshot = client.export_snapshot(&owner).unwrap();
        client.import_snapshot(&owner, &nonce, &snapshot);
    }

    (client, owner)
}

/// Collect every audit entry by following next_cursor until it returns 0.
fn collect_all_pages(client: &RemittanceSplitClient, page_size: u32) -> soroban_sdk::Vec<AuditEntry> {
    let env = client.env.clone();
    let mut all = soroban_sdk::Vec::new(&env);
    let mut cursor: u32 = 0;
    let mut first = true;
    loop {
        let page = client.get_audit_log(&cursor, &page_size);
        if page.count == 0 {
            break;
        }
        for i in 0..page.items.len() {
            if let Some(entry) = page.items.get(i) {
                all.push_back(entry);
            }
        }
        if page.next_cursor == 0 {
            break;
        }
        if !first && cursor == page.next_cursor {
            panic!("cursor did not advance — infinite loop detected");
        }
        first = false;
        cursor = page.next_cursor;
    }
    all
}

#[test]
fn test_get_audit_log_empty_returns_zero_cursor() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);

    let page = client.get_audit_log(&0, &10);
    assert_eq!(page.count, 0);
    assert_eq!(page.next_cursor, 0);
    assert_eq!(page.items.len(), 0);
}

#[test]
fn test_get_audit_log_single_page() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _owner) = seed_audit_log(&env, 3);

    // Request all 3 with a large limit
    let page = client.get_audit_log(&0, &50);
    assert_eq!(page.count, 3);
    assert_eq!(page.next_cursor, 0, "no more pages");
}

#[test]
fn test_get_audit_log_multi_page_no_gaps_no_duplicates() {
    let env = Env::default();
    env.mock_all_auths();
    let entry_count: u32 = 15;
    let (client, _owner) = seed_audit_log(&env, entry_count);

    // Paginate with page_size = 4 → expect 4 pages (4+4+4+3)
    let all = collect_all_pages(&client, 4);
    assert_eq!(
        all.len(),
        entry_count,
        "total entries collected must equal entries seeded"
    );

    // Verify strict timestamp ordering (no duplicates, no gaps)
    for i in 1..all.len() {
        let prev = all.get(i - 1).unwrap();
        let curr = all.get(i).unwrap();
        assert!(
            curr.timestamp >= prev.timestamp,
            "entries must be ordered by timestamp"
        );
    }
}

#[test]
fn test_get_audit_log_cursor_boundaries_and_limits() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _owner) = seed_audit_log(&env, 10);

    // First page: 5 items
    let p1 = client.get_audit_log(&0, &5);
    assert_eq!(p1.count, 5);
    assert_eq!(p1.next_cursor, 5);

    // Second page: 5 items
    let p2 = client.get_audit_log(&p1.next_cursor, &5);
    assert_eq!(p2.count, 5);
    assert_eq!(p2.next_cursor, 0, "exactly at end → no more pages");

    // Out-of-range cursor
    let p3 = client.get_audit_log(&100, &5);
    assert_eq!(p3.count, 0);
    assert_eq!(p3.next_cursor, 0);
}

#[test]
fn test_get_audit_log_limit_zero_uses_default() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _owner) = seed_audit_log(&env, 5);

    // limit=0 should clamp to DEFAULT_PAGE_LIMIT (20), returning all 5
    let page = client.get_audit_log(&0, &0);
    assert_eq!(page.count, 5);
    assert_eq!(page.next_cursor, 0);
}

#[test]
fn test_get_audit_log_large_cursor_does_not_overflow_or_duplicate() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _owner) = seed_audit_log(&env, 5);

    // u32::MAX cursor must not panic from overflow
    let page = client.get_audit_log(&u32::MAX, &50);
    assert_eq!(page.count, 0);
    assert_eq!(page.next_cursor, 0);
}

#[test]
fn test_get_audit_log_limit_clamped_to_max_page_limit() {
    let env = Env::default();
    env.mock_all_auths();
    // Seed 30 entries; request with limit > MAX_PAGE_LIMIT (50)
    let (client, _owner) = seed_audit_log(&env, 30);

    // limit=200 should clamp to MAX_PAGE_LIMIT=50, but we only have 30
    let page = client.get_audit_log(&0, &200);
    assert_eq!(page.count, 30);
    assert_eq!(page.next_cursor, 0, "all entries fit in one clamped page");

    // Verify clamping with a smaller set: request 5, get 5, more remain
    let p1 = client.get_audit_log(&0, &5);
    assert_eq!(p1.count, 5);
    assert!(p1.next_cursor > 0, "more pages remain");
}

#[test]
fn test_get_audit_log_deterministic_replay() {
    let env = Env::default();
    env.mock_all_auths();
    let entry_count: u32 = 10;
    let (client, _owner) = seed_audit_log(&env, entry_count);

    let all = collect_all_pages(&client, 3);
    assert_eq!(all.len(), entry_count);

    // Verify deterministic replay: same query returns same results
    let replay = collect_all_pages(&client, 3);
    assert_eq!(all.len(), replay.len());
    for i in 0..all.len() {
        let a = all.get(i).unwrap();
        let b = replay.get(i).unwrap();
        assert_eq!(a.timestamp, b.timestamp);
        assert_eq!(a.operation, b.operation);
        assert_eq!(a.caller, b.caller);
        assert_eq!(a.success, b.success);
    }
}

#[test]
fn test_get_audit_log_page_size_one_walks_entire_log() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _owner) = seed_audit_log(&env, 8);

    // Walk with page_size=1 to stress cursor advancement
    let all = collect_all_pages(&client, 1);
    assert_eq!(all.len(), 8);
}

// ============================================================================
// Task 5.1 — import_snapshot rejects snapshots with initialized = false
// ============================================================================

#[test]
fn test_import_snapshot_not_initialized_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Export a valid snapshot, then mark it as uninitialized.
    // compute_checksum does not cover the initialized flag, so the checksum
    // check (step 3) still passes and the initialized guard (step 4) fires.
    let mut snapshot = client.export_snapshot(&owner).unwrap();
    snapshot.config.initialized = false;

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SnapshotNotInitialized)),
        "snapshot with initialized=false must be rejected with SnapshotNotInitialized"
    );
}

// ============================================================================
// Task 5.2 — import_snapshot rejects snapshots with a per-field % > 100
// ============================================================================

#[test]
fn test_import_snapshot_per_field_out_of_range_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let mut snapshot = client.export_snapshot(&owner).unwrap();

    // Set spending_percent to an out-of-range value.
    snapshot.config.spending_percent = 101;

    // Recompute the checksum to match the modified snapshot so the checksum
    // check (step 3) passes and the per-field range guard (step 5) fires.
    // Formula: (version + spending + savings + bills + insurance + schedule_count) * 31
    let version = snapshot.schema_version as u64;
    let spending = snapshot.config.spending_percent as u64;
    let savings = snapshot.config.savings_percent as u64;
    let bills = snapshot.config.bills_percent as u64;
    let insurance = snapshot.config.insurance_percent as u64;
    let sc_count = snapshot.schedules.len() as u64;
    snapshot.checksum = version
        .wrapping_add(spending)
        .wrapping_add(savings)
        .wrapping_add(bills)
        .wrapping_add(insurance)
        .wrapping_add(sc_count)
        .wrapping_mul(31);

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::InvalidPercentageRange)),
        "spending_percent = 101 must be rejected with InvalidPercentageRange"
    );
}

// ============================================================================
// Task 5.3 — import_snapshot rejects snapshots where percentages sum to != 100
// ============================================================================

#[test]
fn test_import_snapshot_sum_not_100_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    let mut snapshot = client.export_snapshot(&owner).unwrap();

    // Set percentages so each field is ≤ 100 but the sum is 101 (50+50+1+0).
    // This must pass the per-field range guard (step 5) and fail the sum guard (step 6).
    snapshot.config.spending_percent = 50;
    snapshot.config.savings_percent = 50;
    snapshot.config.bills_percent = 1;
    snapshot.config.insurance_percent = 0;

    // Recompute the checksum to match the modified snapshot so the checksum
    // check (step 3) passes and the sum guard (step 6) fires.
    // Formula: (version + spending + savings + bills + insurance + schedule_count) * 31
    let version = snapshot.schema_version as u64;
    let spending = snapshot.config.spending_percent as u64;
    let savings = snapshot.config.savings_percent as u64;
    let bills = snapshot.config.bills_percent as u64;
    let insurance = snapshot.config.insurance_percent as u64;
    let sc_count = snapshot.schedules.len() as u64;
    snapshot.checksum = version
        .wrapping_add(spending)
        .wrapping_add(savings)
        .wrapping_add(bills)
        .wrapping_add(insurance)
        .wrapping_add(sc_count)
        .wrapping_mul(31);

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::InvalidPercentages)),
        "spending=50, savings=50, bills=1, insurance=0 (sum=101) must be rejected with InvalidPercentages"
    );
}

// ============================================================================
// Task 5.4 — import_snapshot rejects snapshots with a future config.timestamp
// Validates: Requirements 6.1, 12.2
// ============================================================================

#[test]
fn test_import_snapshot_future_config_timestamp_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);

    // Set a known ledger timestamp before initializing so config.timestamp is
    // recorded as 1000 (initialize_split stores env.ledger().timestamp()).
    let ledger_timestamp: u64 = 1000;
    set_test_ledger(&env, ledger_timestamp);

    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Export a valid snapshot — config.timestamp will be 1000.
    let mut snapshot = client.export_snapshot(&owner).unwrap();

    // Advance the snapshot's config.timestamp to one second in the future.
    // The ledger is still at 1000, so this must be rejected.
    snapshot.config.timestamp = ledger_timestamp + 1; // 1001

    // Recompute the checksum to match the modified snapshot so the checksum
    // check (step 3) passes and the future-timestamp guard (step 7) fires.
    // Formula: (version + spending + savings + bills + insurance + schedule_count) * 31
    let version = snapshot.schema_version as u64;
    let spending = snapshot.config.spending_percent as u64;
    let savings = snapshot.config.savings_percent as u64;
    let bills = snapshot.config.bills_percent as u64;
    let insurance = snapshot.config.insurance_percent as u64;
    let sc_count = snapshot.schedules.len() as u64;
    snapshot.checksum = version
        .wrapping_add(spending)
        .wrapping_add(savings)
        .wrapping_add(bills)
        .wrapping_add(insurance)
        .wrapping_add(sc_count)
        .wrapping_mul(31);

    // Nonce is 1 after initialize_split.
    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::InvalidAmount)),
        "config.timestamp = ledger_timestamp + 1 must be rejected with InvalidAmount"
    );
}

// ============================================================================
// Task 5.5 — import_snapshot rejects snapshots where config.owner != caller
// Validates: Requirements 7.1, 12.2
// ============================================================================

#[test]
fn test_import_snapshot_owner_mismatch_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Export a valid snapshot — config.owner will be `owner`.
    let mut snapshot = client.export_snapshot(&owner).unwrap();

    // Replace the embedded owner with a different address.
    // The caller remains `owner`, so snapshot.config.owner != caller, which
    // must trigger the OwnerMismatch guard (step 9 in the pipeline).
    let different_address = Address::generate(&env);
    snapshot.config.owner = different_address;

    // Recompute the checksum to match the modified snapshot so the checksum
    // check (step 3) passes and the ownership guard (step 9) fires.
    // Formula: (version + spending + savings + bills + insurance + schedule_count) * 31
    let version = snapshot.schema_version as u64;
    let spending = snapshot.config.spending_percent as u64;
    let savings = snapshot.config.savings_percent as u64;
    let bills = snapshot.config.bills_percent as u64;
    let insurance = snapshot.config.insurance_percent as u64;
    let sc_count = snapshot.schedules.len() as u64;
    snapshot.checksum = version
        .wrapping_add(spending)
        .wrapping_add(savings)
        .wrapping_add(bills)
        .wrapping_add(insurance)
        .wrapping_add(sc_count)
        .wrapping_mul(31);

    // Nonce is 1 after initialize_split. Caller is `owner` (the on-chain owner),
    // but snapshot.config.owner is now `different_address`.
    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::OwnerMismatch)),
        "snapshot.config.owner != caller must be rejected with OwnerMismatch"
    );
}

// ============================================================================
// Task 5.6 — import_snapshot does not mutate state on failure
// Validates: Requirements 8.4, 12.3
// ============================================================================

#[test]
fn test_import_snapshot_does_not_mutate_state_on_failure() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Record the on-chain config before the failed import attempt.
    let config_before = client.get_config();

    // Build a malformed snapshot: export a valid one, then set initialized=false.
    // The checksum check (step 3) still passes because compute_checksum does not
    // cover the initialized flag, so the SnapshotNotInitialized guard (step 4) fires
    // and the function returns an error before touching any storage key.
    let mut snapshot = client.export_snapshot(&owner).unwrap();
    snapshot.config.initialized = false;

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SnapshotNotInitialized)),
        "malformed snapshot must be rejected"
    );

    // Verify the on-chain config is identical to what it was before the failed call.
    let config_before = config_before.unwrap();
    let config_after = client.get_config().unwrap();
    assert_eq!(config_before.owner, config_after.owner, "owner must be unchanged");
    assert_eq!(config_before.spending_percent, config_after.spending_percent, "spending_percent must be unchanged");
    assert_eq!(config_before.savings_percent, config_after.savings_percent, "savings_percent must be unchanged");
    assert_eq!(config_before.bills_percent, config_after.bills_percent, "bills_percent must be unchanged");
    assert_eq!(config_before.insurance_percent, config_after.insurance_percent, "insurance_percent must be unchanged");
    assert_eq!(config_before.timestamp, config_after.timestamp, "timestamp must be unchanged");
    assert_eq!(config_before.initialized, config_after.initialized, "initialized must be unchanged");
    assert_eq!(config_before.usdc_contract, config_after.usdc_contract, "usdc_contract must be unchanged");
}

// ============================================================================
// Task 5.7 — import_snapshot writes a success=true audit entry on success
// Validates: Requirements 9.2
// ============================================================================

#[test]
fn test_import_snapshot_audit_entry_on_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);

    // initialize_split appends one audit entry (nonce 0 → 1).
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Capture the audit log length before the import.
    let before = collect_all_pages(&client, 50);
    let count_before = before.len();

    // Perform a valid import — nonce is 1 after initialize_split.
    let snapshot = client.export_snapshot(&owner).unwrap();
    let ok = client.import_snapshot(&owner, &1, &snapshot);
    assert!(ok, "import must succeed");

    // The audit log must have grown by exactly one entry.
    let after = collect_all_pages(&client, 50);
    assert_eq!(
        after.len(),
        count_before + 1,
        "audit log must grow by exactly one entry after a successful import"
    );

    // The new entry must have operation="import" and success=true.
    let last = after.get(after.len() - 1).unwrap();
    assert_eq!(
        last.operation,
        symbol_short!("import"),
        "audit entry operation must be \"import\""
    );
    assert!(last.success, "audit entry success must be true after a successful import");
}

// ============================================================================
// Task 5.8 — import_snapshot does NOT write an audit entry on failure
// Validates: Requirements 9.1
//
// NOTE: In Soroban, when a contract function returns Err(...), all storage
// changes — including any append_audit calls made before the return — are
// rolled back atomically. Therefore a failed import_snapshot call does not
// persist an audit entry. This test documents that actual behavior.
// ============================================================================

#[test]
fn test_import_snapshot_audit_entry_on_failure() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);

    // initialize_split appends one audit entry (nonce 0 → 1).
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Capture the audit log length before the failed import.
    let before = collect_all_pages(&client, 50);
    let count_before = before.len();

    // Build a malformed snapshot: export a valid one, then set initialized=false.
    // The checksum check passes (compute_checksum does not cover the initialized flag),
    // so the SnapshotNotInitialized guard fires and the function returns an error.
    let mut snapshot = client.export_snapshot(&owner).unwrap();
    snapshot.config.initialized = false;

    let result = client.try_import_snapshot(&owner, &1, &snapshot);
    assert_eq!(
        result,
        Err(Ok(RemittanceSplitError::SnapshotNotInitialized)),
        "malformed snapshot must be rejected"
    );

    // Because Soroban rolls back all storage writes on Err, the audit log must
    // NOT have grown — the append_audit call inside the error path is reverted.
    let after = collect_all_pages(&client, 50);
    assert_eq!(
        after.len(),
        count_before,
        "audit log must NOT grow after a failed import (Soroban rolls back storage on Err)"
    );
}

// ============================================================================
// Task 5.9 — import_snapshot restores schedules (round-trip)
// Validates: Requirements 8.3, 12.1
// ============================================================================

#[test]
fn test_import_snapshot_with_schedules_round_trip() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RemittanceSplit);
    let client = RemittanceSplitClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    let token_id = Address::generate(&env);

    // Set a known ledger timestamp so next_due can be in the future.
    set_test_ledger(&env, 1000);

    // initialize_split uses nonce 0 → nonce becomes 1.
    client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

    // Create two schedules. create_remittance_schedule does NOT consume a nonce.
    let id1 = client.create_remittance_schedule(&owner, &10000, &3000, &86400);
    let id2 = client.create_remittance_schedule(&owner, &5000, &4000, &0);

    // Capture the schedules before export so we can compare after import.
    let schedules_before = client.get_remittance_schedules(&owner);
    assert_eq!(schedules_before.len(), 2, "two schedules must exist before export");

    // Export the snapshot — it includes both schedules.
    let snapshot = client.export_snapshot(&owner).unwrap();
    assert_eq!(snapshot.schedules.len(), 2, "snapshot must carry both schedules");

    // Import the snapshot. Nonce is 1 (only initialize_split consumed a nonce).
    let ok = client.import_snapshot(&owner, &1, &snapshot);
    assert!(ok, "import must succeed");

    // Verify get_remittance_schedules returns the same schedules.
    let schedules_after = client.get_remittance_schedules(&owner);
    assert_eq!(
        schedules_after.len(),
        schedules_before.len(),
        "schedule count must be the same after import"
    );

    // Compare each schedule field-by-field.
    for i in 0..schedules_before.len() {
        let before = schedules_before.get(i).unwrap();
        let after = schedules_after.get(i).unwrap();
        assert_eq!(before.id, after.id, "schedule id must match at index {}", i);
        assert_eq!(before.owner, after.owner, "schedule owner must match at index {}", i);
        assert_eq!(before.amount, after.amount, "schedule amount must match at index {}", i);
        assert_eq!(before.next_due, after.next_due, "schedule next_due must match at index {}", i);
        assert_eq!(before.interval, after.interval, "schedule interval must match at index {}", i);
        assert_eq!(before.recurring, after.recurring, "schedule recurring must match at index {}", i);
        assert_eq!(before.active, after.active, "schedule active must match at index {}", i);
        assert_eq!(before.created_at, after.created_at, "schedule created_at must match at index {}", i);
        assert_eq!(before.missed_count, after.missed_count, "schedule missed_count must match at index {}", i);
    }

    // Also verify the individual schedule IDs are still accessible.
    let s1 = client.get_remittance_schedule(&id1).unwrap();
    let s2 = client.get_remittance_schedule(&id2).unwrap();
    assert_eq!(s1.amount, 10000);
    assert_eq!(s2.amount, 5000);
}

// ============================================================================
// Task 5.10 — verify_snapshot returns the same error as import_snapshot
// Validates: Requirements 10.1, 10.2, 12.4
// ============================================================================

#[test]
fn test_verify_snapshot_parity_with_import() {
    // Helper to recompute the checksum after mutating percentage fields.
    // Formula: (version + spending + savings + bills + insurance + schedule_count) * 31
    fn recompute_checksum(snapshot: &ExportSnapshot) -> u64 {
        let version = snapshot.schema_version as u64;
        let spending = snapshot.config.spending_percent as u64;
        let savings = snapshot.config.savings_percent as u64;
        let bills = snapshot.config.bills_percent as u64;
        let insurance = snapshot.config.insurance_percent as u64;
        let sc_count = snapshot.schedules.len() as u64;
        version
            .wrapping_add(spending)
            .wrapping_add(savings)
            .wrapping_add(bills)
            .wrapping_add(insurance)
            .wrapping_add(sc_count)
            .wrapping_mul(31)
    }

    // ── Case 1: initialized = false ──────────────────────────────────────────
    {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RemittanceSplit);
        let client = RemittanceSplitClient::new(&env, &contract_id);
        let owner = Address::generate(&env);
        let token_id = Address::generate(&env);
        client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

        let mut snapshot = client.export_snapshot(&owner).unwrap();
        snapshot.config.initialized = false;
        // checksum does not cover initialized flag — no recompute needed.

        let import_err = client.try_import_snapshot(&owner, &1, &snapshot)
            .expect_err("import must fail for uninitialized snapshot");
        let verify_err = client.try_verify_snapshot(&snapshot)
            .expect_err("verify must fail for uninitialized snapshot");

        assert_eq!(
            import_err, verify_err,
            "verify_snapshot must return the same error as import_snapshot for initialized=false"
        );
        assert_eq!(import_err, Ok(RemittanceSplitError::SnapshotNotInitialized));
    }

    // ── Case 2: per-field percentage out of range (spending_percent = 101) ───
    {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RemittanceSplit);
        let client = RemittanceSplitClient::new(&env, &contract_id);
        let owner = Address::generate(&env);
        let token_id = Address::generate(&env);
        client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

        let mut snapshot = client.export_snapshot(&owner).unwrap();
        snapshot.config.spending_percent = 101;
        snapshot.checksum = recompute_checksum(&snapshot);

        let import_err = client.try_import_snapshot(&owner, &1, &snapshot)
            .expect_err("import must fail for spending_percent=101");
        let verify_err = client.try_verify_snapshot(&snapshot)
            .expect_err("verify must fail for spending_percent=101");

        assert_eq!(
            import_err, verify_err,
            "verify_snapshot must return the same error as import_snapshot for per-field range violation"
        );
        assert_eq!(import_err, Ok(RemittanceSplitError::InvalidPercentageRange));
    }

    // ── Case 3: sum != 100 (50 + 50 + 1 + 0 = 101) ──────────────────────────
    {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RemittanceSplit);
        let client = RemittanceSplitClient::new(&env, &contract_id);
        let owner = Address::generate(&env);
        let token_id = Address::generate(&env);
        client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

        let mut snapshot = client.export_snapshot(&owner).unwrap();
        snapshot.config.spending_percent = 50;
        snapshot.config.savings_percent = 50;
        snapshot.config.bills_percent = 1;
        snapshot.config.insurance_percent = 0;
        snapshot.checksum = recompute_checksum(&snapshot);

        let import_err = client.try_import_snapshot(&owner, &1, &snapshot)
            .expect_err("import must fail for sum=101");
        let verify_err = client.try_verify_snapshot(&snapshot)
            .expect_err("verify must fail for sum=101");

        assert_eq!(
            import_err, verify_err,
            "verify_snapshot must return the same error as import_snapshot for sum != 100"
        );
        assert_eq!(import_err, Ok(RemittanceSplitError::InvalidPercentages));
    }

    // ── Case 4: future config.timestamp ──────────────────────────────────────
    {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, RemittanceSplit);
        let client = RemittanceSplitClient::new(&env, &contract_id);
        let owner = Address::generate(&env);
        let token_id = Address::generate(&env);

        let ledger_timestamp: u64 = 1000;
        set_test_ledger(&env, ledger_timestamp);
        client.initialize_split(&owner, &0, &token_id, &50, &30, &15, &5);

        let mut snapshot = client.export_snapshot(&owner).unwrap();
        snapshot.config.timestamp = ledger_timestamp + 1;
        snapshot.checksum = recompute_checksum(&snapshot);

        let import_err = client.try_import_snapshot(&owner, &1, &snapshot)
            .expect_err("import must fail for future timestamp");
        let verify_err = client.try_verify_snapshot(&snapshot)
            .expect_err("verify must fail for future timestamp");

        assert_eq!(
            import_err, verify_err,
            "verify_snapshot must return the same error as import_snapshot for future timestamp"
        );
        assert_eq!(import_err, Ok(RemittanceSplitError::InvalidAmount));
    }
}
