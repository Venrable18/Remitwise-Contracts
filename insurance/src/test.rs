#![cfg(test)]

use super::*;
use crate::InsuranceError;
use remitwise_common::CoverageType;
use soroban_sdk::{
    testutils::{storage::Instance, Address as AddressTrait, Ledger, LedgerInfo},
    Address, Env, String, vec, Map, symbol_short,
};
use proptest::prelude::*;

fn setup() -> (Env, InsuranceClient<'static>, Address) {
    let env = Env::default();
    let contract_id = env.register_contract(None, Insurance);
    let client = InsuranceClient::new(&env, &contract_id);
    let owner = Address::generate(&env);
    client.initialize(&owner);
    env.mock_all_auths();
    (env, client, owner)
}

fn make_policy(env: &Env, client: &InsuranceClient, owner: &Address) -> u32 {
    client.create_policy(
        owner,
        &String::from_str(env, "Test Policy"),
        &CoverageType::Health,
        &100,
        &10000,
        &None,
    )
}

#[test]
fn test_create_policy_succeeds() {
    let (env, client, owner) = setup();

    let name = String::from_str(&env, "Health Policy");
    let coverage_type = CoverageType::Health;

    let policy_id = client.create_policy(
        &owner,
        &name,
        &coverage_type,
        &100,   // monthly_premium
        &10000, // coverage_amount
        &None,
    );

    assert_eq!(policy_id, 1);

    let policy = client.get_policy(&policy_id).unwrap();
    assert_eq!(policy.owner, owner);
    assert_eq!(policy.monthly_premium, 100);
    assert_eq!(policy.coverage_amount, 10000);
    assert!(policy.active);
}

#[test]
fn test_create_policy_invalid_amounts() {
    let (env, client, owner) = setup();
    
    // Invalid premium
    let result = client.try_create_policy(
        &owner,
        &String::from_str(&env, "Bad"),
        &CoverageType::Health,
        &0,
        &10000,
        &None,
    );
    assert_eq!(result, Err(Ok(InsuranceError::InvalidAmount)));

    // Invalid coverage
    let result = client.try_create_policy(
        &owner,
        &String::from_str(&env, "Bad"),
        &CoverageType::Health,
        &100,
        &0,
        &None,
    );
    assert_eq!(result, Err(Ok(InsuranceError::InvalidAmount)));
}

#[test]
fn test_pay_premium_lifecycle() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner);
    let before = client.get_policy(&policy_id).unwrap().next_payment_date;
    
    // Advance ledger time
    env.ledger().with_mut(|li| li.timestamp += 1000);
    
    client.pay_premium(&owner, &policy_id);
    let after = client.get_policy(&policy_id).unwrap().next_payment_date;
    assert!(after > before);
    
    // Unauthorized
    let other = Address::generate(&env);
    let result = client.try_pay_premium(&other, &policy_id);
    assert_eq!(result, Err(Ok(InsuranceError::Unauthorized)));
}

#[test]
fn test_deactivate_policy_hardening() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner);
    
    // Create a schedule for this policy
    let now = env.ledger().timestamp();
    let sch_id = client.create_premium_schedule(&owner, &policy_id, &(now + 1000), &86400);
    
    // Deactivate
    let deactivated = client.deactivate_policy(&owner, &policy_id);
    assert!(deactivated, "First deactivation should return true");
    
    let policy = client.get_policy(&policy_id).unwrap();
    assert!(!policy.active);
    
    // Verify schedule is also deactivated
    let sch = client.get_premium_schedule(&sch_id).unwrap();
    assert!(!sch.active);
    
    // Idempotency: second call returns false
    let second = client.deactivate_policy(&owner, &policy_id);
    assert!(!second, "Second deactivation should return false");
}

#[test]
fn test_get_active_policies_filtering() {
    let (env, client, owner) = setup();
    let p1 = make_policy(&env, &client, &owner);
    let p2 = make_policy(&env, &client, &owner);
    let p3 = make_policy(&env, &client, &owner);
    
    client.deactivate_policy(&owner, &p2);
    
    let active = client.get_active_policies(&owner, &0, &10);
    assert_eq!(active.items.len(), 2);
    assert_eq!(active.items.get(0).unwrap().id, p1);
    assert_eq!(active.items.get(1).unwrap().id, p3);
    
    let all = client.get_all_policies_for_owner(&owner, &0, &10);
    assert_eq!(all.items.len(), 3);
}

#[test]
fn test_premium_totals() {
    let (env, client, owner) = setup();
    let p1 = client.create_policy(&owner, &String::from_str(&env, "P1"), &CoverageType::Health, &100, &1000, &None);
    let p2 = client.create_policy(&owner, &String::from_str(&env, "P2"), &CoverageType::Health, &200, &2000, &None);
    
    assert_eq!(client.get_total_monthly_premium(&owner), 300);
    
    client.deactivate_policy(&owner, &p1);
    assert_eq!(client.get_total_monthly_premium(&owner), 200);
}

#[test]
fn test_premium_schedules_lifecycle() {
    let (env, client, owner) = setup();
    let p_id = make_policy(&env, &client, &owner);
    let now = env.ledger().timestamp();
    
    let sch_id = client.create_premium_schedule(&owner, &p_id, &(now + 1000), &86400);
    assert_eq!(sch_id, 1);
    
    client.modify_premium_schedule(&owner, &sch_id, &(now + 2000), &90000);
    let sch = client.get_premium_schedule(&sch_id).unwrap();
    assert_eq!(sch.next_due, now + 2000);
    assert_eq!(sch.interval, 90000);
    
    client.cancel_premium_schedule(&owner, &sch_id);
    let sch = client.get_premium_schedule(&sch_id).unwrap();
    assert!(!sch.active);
}

#[test]
fn test_execute_due_schedules() {
    let (env, client, owner) = setup();
    let p_id = make_policy(&env, &client, &owner);
    let now = env.ledger().timestamp();
    
    client.create_premium_schedule(&owner, &p_id, &(now + 1000), &86400);
    
    // Execute before due
    let executed = client.execute_due_premium_schedules();
    assert_eq!(executed.len(), 0);
    
    // Execute after due
    env.ledger().with_mut(|li| li.timestamp += 1500);
    let executed = client.execute_due_premium_schedules();
    assert_eq!(executed.len(), 1);
    
    let policy = client.get_policy(&p_id).unwrap();
    assert!(policy.next_payment_date > now + 30 * 86400);
}

#[test]
fn test_instance_ttl_extension() {
    let (env, client, owner) = setup();
    let contract_id = client.address.clone();
    
    // Setup low TTL for THIS contract
    env.as_contract(&contract_id, || {
        env.storage().instance().extend_ttl(100, 200);
    });
    
    // Ledger info for TTL threshold
    env.ledger().set(LedgerInfo {
        timestamp: 12345,
        protocol_version: 21,
        sequence_number: 10,
        network_id: [0u8; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 120,
        max_entry_ttl: 700_000,
    });
    
    client.create_policy(&owner, &String::from_str(&env, "TTL"), &CoverageType::Health, &100, &10000, &None);
    
    let ttl = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(ttl >= 518400, "TTL must be extended to at least 30 days");
}

// Property-based tests
proptest! {
    #[test]
    fn prop_pay_premium_sets_correct_next_date(now in 1_000_000u64..100_000_000u64) {
        let env = Env::default();
        env.ledger().set_timestamp(now);
        env.mock_all_auths();
        let cid = env.register_contract(None, Insurance);
        let client = InsuranceClient::new(&env, &cid);
        let owner = Address::generate(&env);
        client.initialize(&owner);

        let p_id = client.create_policy(&owner, &String::from_str(&env, "P"), &CoverageType::Health, &100, &10000, &None);
        client.pay_premium(&owner, &p_id);

        let policy = client.get_policy(&p_id).unwrap();
        prop_assert_eq!(policy.next_payment_date, now + 30 * 86400);
    }
}