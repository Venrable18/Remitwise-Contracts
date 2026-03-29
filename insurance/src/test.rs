#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String, Vec,
};
use testutils::set_ledger_time;

fn setup() -> (Env, InsuranceClient<'static>, Address) {
    let env = Env::default();
    let contract_id = env.register_contract(None, Insurance);
    let client = InsuranceClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    env.mock_all_auths();
    client.initialize(&admin);
    (env, client, admin)
}

fn make_policy(
    env: &Env,
    client: &InsuranceClient<'_>,
    owner: &Address,
    premium: i128,
    coverage: i128,
) -> u32 {
    client.create_policy(
        owner,
        &String::from_str(env, "Health Policy"),
        &CoverageType::Health,
        &premium,
        &coverage,
        &None,
    )
}

#[test]
fn test_initialize_sets_pause_admin() {
    let env = Env::default();
    let contract_id = env.register_contract(None, Insurance);
    let client = InsuranceClient::new(&env, &contract_id);
    let admin = Address::generate(&env);

    env.mock_all_auths();
    assert_eq!(client.initialize(&admin), ());
    assert_eq!(client.set_pause_admin(&admin, &admin), ());
}

#[test]
fn test_create_policy_succeeds() {
    let (env, client, owner) = setup();

    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let policy = client.get_policy(&policy_id).unwrap();

    assert_eq!(policy.id, policy_id);
    assert_eq!(policy.owner, owner);
    assert_eq!(policy.coverage_type, CoverageType::Health);
    assert!(policy.active);
}

#[test]
fn test_create_policy_invalid_amount_fails() {
    let (env, client, owner) = setup();

    let result = client.try_create_policy(
        &owner,
        &String::from_str(&env, "Bad Policy"),
        &CoverageType::Health,
        &0,
        &10_000,
        &None,
    );

    assert_eq!(result, Err(Ok(InsuranceError::InvalidAmount)));
}

#[test]
fn test_pay_premium_updates_next_payment_date() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let before = client.get_policy(&policy_id).unwrap().next_payment_date;

    set_ledger_time(&env, 2, env.ledger().timestamp() + 1_000);
    assert_eq!(client.pay_premium(&owner, &policy_id), ());

    let after = client.get_policy(&policy_id).unwrap().next_payment_date;
    assert!(after > before);
}

#[test]
fn test_pay_premium_unauthorized_fails() {
    let (env, client, owner) = setup();
    let stranger = Address::generate(&env);
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);

    let result = client.try_pay_premium(&stranger, &policy_id);
    assert_eq!(result, Err(Ok(InsuranceError::Unauthorized)));
}

#[test]
fn test_deactivate_policy_success() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);

    assert!(client.deactivate_policy(&owner, &policy_id));
    assert!(!client.get_policy(&policy_id).unwrap().active);
}

#[test]
fn test_deactivate_policy_non_owner_fails() {
    let (env, client, owner) = setup();
    let stranger = Address::generate(&env);
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);

    let result = client.try_deactivate_policy(&stranger, &policy_id);
    assert_eq!(result, Err(Ok(InsuranceError::Unauthorized)));
}

#[test]
fn test_get_active_policies_excludes_deactivated() {
    let (env, client, owner) = setup();
    let policy_one = make_policy(&env, &client, &owner, 100, 10_000);
    let policy_two = client.create_policy(
        &owner,
        &String::from_str(&env, "Life Policy"),
        &CoverageType::Life,
        &500,
        &20_000,
        &None,
    );

    assert!(client.deactivate_policy(&owner, &policy_one));

    let page = client.get_active_policies(&owner, &0, &10);
    assert_eq!(page.count, 1);
    assert_eq!(page.items.get(0).unwrap().id, policy_two);
}

#[test]
fn test_get_total_monthly_premium_sums_active_only() {
    let (env, client, owner) = setup();
    let policy_one = make_policy(&env, &client, &owner, 100, 10_000);
    let _policy_two = client.create_policy(
        &owner,
        &String::from_str(&env, "Property Policy"),
        &CoverageType::Property,
        &200,
        &50_000,
        &None,
    );

    assert_eq!(client.get_total_monthly_premium(&owner), 300);
    assert!(client.deactivate_policy(&owner, &policy_one));
    assert_eq!(client.get_total_monthly_premium(&owner), 200);
}

#[test]
fn test_get_total_monthly_premium_zero_when_no_policies() {
    let (_env, client, owner) = setup();
    assert_eq!(client.get_total_monthly_premium(&owner), 0);
}

#[test]
fn test_add_and_remove_tags() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);

    let mut add_tags = Vec::new(&env);
    add_tags.push_back(String::from_str(&env, "active"));
    add_tags.push_back(String::from_str(&env, "family"));
    client.add_tags_to_policy(&owner, &policy_id, &add_tags);

    let policy = client.get_policy(&policy_id).unwrap();
    assert_eq!(policy.tags.len(), 2);

    let mut remove_tags = Vec::new(&env);
    remove_tags.push_back(String::from_str(&env, "active"));
    client.remove_tags_from_policy(&owner, &policy_id, &remove_tags);

    let updated = client.get_policy(&policy_id).unwrap();
    assert_eq!(updated.tags.len(), 1);
    assert_eq!(
        updated.tags.get(0).unwrap(),
        String::from_str(&env, "family")
    );
}

#[test]
fn test_set_external_ref_updates_policy() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let external_ref = Some(String::from_str(&env, "EXT-123"));

    assert!(client.set_external_ref(&owner, &policy_id, &external_ref));
    assert_eq!(
        client.get_policy(&policy_id).unwrap().external_ref,
        external_ref
    );
}

#[test]
fn test_create_modify_and_cancel_premium_schedule() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let next_due = env.ledger().timestamp() + 5_000;

    let schedule_id = client.create_premium_schedule(&owner, &policy_id, &next_due, &2_592_000);
    let created = client.get_premium_schedule(&schedule_id).unwrap();
    assert!(created.active);
    assert!(created.recurring);

    assert!(client.modify_premium_schedule(&owner, &schedule_id, &(next_due + 10), &100));
    let modified = client.get_premium_schedule(&schedule_id).unwrap();
    assert_eq!(modified.next_due, next_due + 10);
    assert_eq!(modified.interval, 100);

    assert!(client.cancel_premium_schedule(&owner, &schedule_id));
    assert!(!client.get_premium_schedule(&schedule_id).unwrap().active);
}

#[test]
fn test_execute_due_premium_schedules_handles_one_shot() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let next_due = env.ledger().timestamp() + 10;
    let schedule_id = client.create_premium_schedule(&owner, &policy_id, &next_due, &0);

    set_ledger_time(&env, 3, next_due + 1);
    let executed = client.execute_due_premium_schedules();

    assert_eq!(executed.len(), 1);
    assert_eq!(executed.get(0).unwrap(), schedule_id);
    assert!(!client.get_premium_schedule(&schedule_id).unwrap().active);
}

#[test]
fn test_execute_due_premium_schedules_tracks_recurring_misses() {
    let (env, client, owner) = setup();
    let policy_id = make_policy(&env, &client, &owner, 100, 10_000);
    let next_due = env.ledger().timestamp() + 10;
    let schedule_id = client.create_premium_schedule(&owner, &policy_id, &next_due, &100);

    set_ledger_time(&env, 4, next_due + 250);
    let executed = client.execute_due_premium_schedules();

    assert_eq!(executed.len(), 1);
    let schedule = client.get_premium_schedule(&schedule_id).unwrap();
    assert!(schedule.active);
    assert_eq!(schedule.last_executed, Some(next_due + 250));
    assert!(schedule.missed_count >= 2);
    assert!(schedule.next_due > next_due + 250);
}

#[test]
fn test_pause_function_blocks_create_policy() {
    let (env, client, owner) = setup();

    assert_eq!(
        client.pause_function(&owner, &pause_functions::CREATE_POLICY),
        ()
    );

    let result = client.try_create_policy(
        &owner,
        &String::from_str(&env, "Blocked"),
        &CoverageType::Health,
        &100,
        &10_000,
        &None,
    );

    assert_eq!(result, Err(Ok(InsuranceError::FunctionPaused)));
}
