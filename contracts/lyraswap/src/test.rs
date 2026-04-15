#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, token, Address, Env, String};

fn deploy<'a>(
    env: &'a Env,
    token_a: &Address,
    token_b: &Address,
    fee_bps: u32,
    owner: &Address,
) -> LyraSwapClient<'a> {
    let contract_id = env.register(
        LyraSwap,
        (
            token_a.clone(),
            token_b.clone(),
            fee_bps,
            owner.clone(),
            String::from_str(env, "Lyra LP Token"),
            String::from_str(env, "LYRA-LP"),
        ),
    );
    LyraSwapClient::new(env, &contract_id)
}

fn create_token(env: &Env, admin: &Address) -> Address {
    env.register_stellar_asset_contract_v2(admin.clone())
        .address()
}

fn mint(env: &Env, token_id: &Address, to: &Address, amount: i128) {
    token::StellarAssetClient::new(env, token_id).mint(to, &amount);
}

#[test]
fn test_pair_constructor() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    let token_a = create_token(&env, &admin);
    let token_b = create_token(&env, &admin);

    let client = deploy(&env, &token_a, &token_b, 30, &owner);
    let state = client.get_state();

    assert_eq!(state.fee_bps, 30);
    assert_eq!(state.reserve_0, 0);
    assert_eq!(state.reserve_1, 0);

    if token_a < token_b {
        assert_eq!(state.token_0, token_a);
        assert_eq!(state.token_1, token_b);
    } else {
        assert_eq!(state.token_0, token_b);
        assert_eq!(state.token_1, token_a);
    }
}

#[test]
#[should_panic]
fn test_identical_tokens_fail() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    let token_a = create_token(&env, &admin);

    deploy(&env, &token_a, &token_a, 30, &owner);
}

#[test]
fn test_add_liquidity_initial() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    let token_a = create_token(&env, &admin);
    let token_b = create_token(&env, &admin);
    let client = deploy(&env, &token_a, &token_b, 30, &owner);

    mint(&env, &token_a, &provider, 10_000);
    mint(&env, &token_b, &provider, 40_000);

    let (amount_0, amount_1, lp_minted) = client.add_liquidity(&provider, &10_000, &40_000);

    assert_eq!(amount_0, 10_000);
    assert_eq!(amount_1, 40_000);
    assert_eq!(lp_minted, 20_000);

    let state = client.get_state();
    assert_eq!(state.reserve_0, amount_0);
    assert_eq!(state.reserve_1, amount_1);
}

#[test]
fn test_remove_liquidity() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    let token_a = create_token(&env, &admin);
    let token_b = create_token(&env, &admin);
    let client = deploy(&env, &token_a, &token_b, 30, &owner);

    mint(&env, &token_a, &provider, 10_000);
    mint(&env, &token_b, &provider, 40_000);
    client.add_liquidity(&provider, &10_000, &40_000);

    let lp_amount = 10_000;
    let (withdrawn_0, withdrawn_1) =
        client.remove_liquidity(&provider, &lp_amount, &4_000, &19_000);

    assert_eq!(withdrawn_0, 5_000);
    assert_eq!(withdrawn_1, 20_000);

    let state = client.get_state();
    assert_eq!(state.reserve_0, 5_000);
    assert_eq!(state.reserve_1, 20_000);
}

#[test]
fn test_swap_exact_in() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let trader = Address::generate(&env);

    let token_a = create_token(&env, &admin);
    let token_b = create_token(&env, &admin);
    let client = deploy(&env, &token_a, &token_b, 30, &owner);

    mint(&env, &token_a, &provider, 100_000);
    mint(&env, &token_b, &provider, 100_000);
    client.add_liquidity(&provider, &100_000, &100_000);

    mint(&env, &token_a, &trader, 1_000);

    let amount_out = client.swap_exact_in(&trader, &token_a, &1_000, &900);
    assert!(amount_out >= 900);

    let state = client.get_state();
    let expected_reserve_a = 101_000;
    let expected_reserve_b = 100_000 - amount_out;

    if state.token_0 == token_a {
        assert_eq!(state.reserve_0, expected_reserve_a);
        assert_eq!(state.reserve_1, expected_reserve_b);
    } else {
        assert_eq!(state.reserve_1, expected_reserve_a);
        assert_eq!(state.reserve_0, expected_reserve_b);
    }
}
