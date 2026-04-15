#![no_std]

use core::cmp::min;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, Env,
    MuxedAddress, String,
};
use stellar_access::ownable::{self as ownable, Ownable};
use stellar_macros::only_owner;
use stellar_tokens::fungible::{burnable::FungibleBurnable, Base, FungibleToken};

const BPS_DENOMINATOR: i128 = 10_000;
const MAX_FEE_BPS: u32 = 300;

#[contract]
pub struct LyraSwap;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolState {
    pub token_0: Address,
    pub token_1: Address,
    pub reserve_0: i128,
    pub reserve_1: i128,
    pub fee_bps: u32,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    State,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    OwnerNotSet = 3,
    IdenticalTokens = 4,
    InvalidFee = 7,
    InvalidAmount = 8,
    SlippageExceeded = 9,
    InsufficientLiquidity = 10,
    InsufficientLpBalance = 11,
    ArithmeticOverflow = 12,
    ZeroLiquidity = 14,
}

#[contractevent(topics = ["lyraswap", "fee_updated"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeUpdatedEvent {
    #[topic]
    pub owner: Address,
    pub old_fee_bps: u32,
    pub new_fee_bps: u32,
}

#[contractevent(topics = ["lyraswap", "liquidity_added"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityAddedEvent {
    #[topic]
    pub provider: Address,
    pub amount_0: i128,
    pub amount_1: i128,
    pub lp_minted: i128,
    pub total_lp: i128,
}

#[contractevent(topics = ["lyraswap", "liquidity_removed"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiquidityRemovedEvent {
    #[topic]
    pub provider: Address,
    pub amount_0: i128,
    pub amount_1: i128,
    pub lp_burned: i128,
    pub total_lp: i128,
}

#[contractevent(topics = ["lyraswap", "swap_exact_in"])]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapExactInEvent {
    #[topic]
    pub token_in: Address,
    #[topic]
    pub token_out: Address,
    #[topic]
    pub trader: Address,
    pub amount_in: i128,
    pub amount_out: i128,
    pub fee_bps: u32,
}

#[contractimpl]
impl LyraSwap {
    pub fn __constructor(
        env: Env,
        token_a: Address,
        token_b: Address,
        fee_bps: u32,
        owner: Address,
        token_name: String,
        token_symbol: String,
    ) {
        if env.storage().instance().has(&DataKey::State) {
            env.panic_with_error(Error::AlreadyInitialized);
        }
        if token_a == token_b {
            env.panic_with_error(Error::IdenticalTokens);
        }
        if fee_bps > MAX_FEE_BPS {
            env.panic_with_error(Error::InvalidFee);
        }

        // Canonicalize tokens
        let (token_0, token_1) = if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        };

        let state = PoolState {
            token_0,
            token_1,
            reserve_0: 0,
            reserve_1: 0,
            fee_bps,
        };

        env.storage().instance().set(&DataKey::State, &state);

        // OpenZeppelin integrations
        Base::set_metadata(&env, 7, token_name, token_symbol);
        ownable::set_owner(&env, &owner);
    }

    #[only_owner]
    pub fn set_fee(env: Env, caller: Address, new_fee_bps: u32) -> Result<(), Error> {
        let mut state = read_state(&env)?;
        if new_fee_bps > MAX_FEE_BPS {
            return Err(Error::InvalidFee);
        }

        let old_fee_bps = state.fee_bps;
        state.fee_bps = new_fee_bps;
        env.storage().instance().set(&DataKey::State, &state);

        FeeUpdatedEvent {
            owner: caller,
            old_fee_bps,
            new_fee_bps,
        }
        .publish(&env);

        Ok(())
    }

    pub fn add_liquidity(
        env: Env,
        provider: Address,
        amount_0_opt: i128,
        amount_1_opt: i128,
    ) -> Result<(i128, i128, i128), Error> {
        provider.require_auth();
        let mut state = read_state(&env)?;

        if amount_0_opt <= 0 || amount_1_opt <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_0_client = token::Client::new(&env, &state.token_0);
        let token_1_client = token::Client::new(&env, &state.token_1);
        let total_lp = Base::total_supply(&env);

        let (amount_0, amount_1, lp_minted);
        if total_lp == 0 {
            amount_0 = amount_0_opt;
            amount_1 = amount_1_opt;
            // Uses standard AMM total_lp initial arithmetic
            let lp_product = amount_0
                .checked_mul(amount_1)
                .ok_or(Error::ArithmeticOverflow)?;
            lp_minted = integer_sqrt(lp_product)?;
            if lp_minted == 0 {
                return Err(Error::ZeroLiquidity);
            }
            Base::mint(&env, &provider, lp_minted);
        } else {
            // Optimal proportion
            let optimal_1 = amount_0_opt
                .checked_mul(state.reserve_1)
                .ok_or(Error::ArithmeticOverflow)?
                .checked_div(state.reserve_0)
                .ok_or(Error::ArithmeticOverflow)?;

            if amount_1_opt >= optimal_1 {
                amount_0 = amount_0_opt;
                amount_1 = optimal_1;
            } else {
                let optimal_0 = amount_1_opt
                    .checked_mul(state.reserve_0)
                    .ok_or(Error::ArithmeticOverflow)?
                    .checked_div(state.reserve_1)
                    .ok_or(Error::ArithmeticOverflow)?;
                amount_0 = optimal_0;
                amount_1 = amount_1_opt;
            }

            let lp_minted_0 = amount_0
                .checked_mul(total_lp)
                .ok_or(Error::ArithmeticOverflow)?
                .checked_div(state.reserve_0)
                .ok_or(Error::ArithmeticOverflow)?;
            let lp_minted_1 = amount_1
                .checked_mul(total_lp)
                .ok_or(Error::ArithmeticOverflow)?
                .checked_div(state.reserve_1)
                .ok_or(Error::ArithmeticOverflow)?;

            lp_minted = min(lp_minted_0, lp_minted_1);
            if lp_minted <= 0 {
                return Err(Error::ZeroLiquidity);
            }
            Base::mint(&env, &provider, lp_minted);
        }

        token_0_client.transfer(&provider, &env.current_contract_address(), &amount_0);
        token_1_client.transfer(&provider, &env.current_contract_address(), &amount_1);

        state.reserve_0 = state
            .reserve_0
            .checked_add(amount_0)
            .ok_or(Error::ArithmeticOverflow)?;
        state.reserve_1 = state
            .reserve_1
            .checked_add(amount_1)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage().instance().set(&DataKey::State, &state);

        LiquidityAddedEvent {
            provider,
            amount_0,
            amount_1,
            lp_minted,
            total_lp: Base::total_supply(&env),
        }
        .publish(&env);

        Ok((amount_0, amount_1, lp_minted))
    }

    pub fn remove_liquidity(
        env: Env,
        provider: Address,
        lp_amount: i128,
        min_amount_0: i128,
        min_amount_1: i128,
    ) -> Result<(i128, i128), Error> {
        let mut state = read_state(&env)?;

        if lp_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let user_lp = Base::balance(&env, &provider);
        if user_lp < lp_amount {
            return Err(Error::InsufficientLpBalance);
        }

        let total_lp = Base::total_supply(&env);
        let amount_0 = lp_amount
            .checked_mul(state.reserve_0)
            .ok_or(Error::ArithmeticOverflow)?
            .checked_div(total_lp)
            .ok_or(Error::ArithmeticOverflow)?;
        let amount_1 = lp_amount
            .checked_mul(state.reserve_1)
            .ok_or(Error::ArithmeticOverflow)?
            .checked_div(total_lp)
            .ok_or(Error::ArithmeticOverflow)?;

        if amount_0 < min_amount_0 || amount_1 < min_amount_1 {
            return Err(Error::SlippageExceeded);
        }

        Base::burn(&env, &provider, lp_amount);

        let token_0_client = token::Client::new(&env, &state.token_0);
        let token_1_client = token::Client::new(&env, &state.token_1);

        token_0_client.transfer(&env.current_contract_address(), &provider, &amount_0);
        token_1_client.transfer(&env.current_contract_address(), &provider, &amount_1);

        state.reserve_0 = state
            .reserve_0
            .checked_sub(amount_0)
            .ok_or(Error::ArithmeticOverflow)?;
        state.reserve_1 = state
            .reserve_1
            .checked_sub(amount_1)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage().instance().set(&DataKey::State, &state);

        LiquidityRemovedEvent {
            provider,
            amount_0,
            amount_1,
            lp_burned: lp_amount,
            total_lp: Base::total_supply(&env),
        }
        .publish(&env);

        Ok((amount_0, amount_1))
    }

    pub fn swap_exact_in(
        env: Env,
        trader: Address,
        token_in: Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, Error> {
        trader.require_auth();
        let mut state = read_state(&env)?;

        if amount_in <= 0 {
            return Err(Error::InvalidAmount);
        }

        let is_token_0 = if token_in == state.token_0 {
            true
        } else if token_in == state.token_1 {
            false
        } else {
            return Err(Error::InvalidAmount); // Should be a diff error technically, using existing
        };

        let (reserve_in, reserve_out) = if is_token_0 {
            (state.reserve_0, state.reserve_1)
        } else {
            (state.reserve_1, state.reserve_0)
        };

        if reserve_out <= 0 {
            return Err(Error::InsufficientLiquidity);
        }

        let fee_multiplier = BPS_DENOMINATOR
            .checked_sub(state.fee_bps as i128)
            .ok_or(Error::ArithmeticOverflow)?;
        let amount_in_with_fee = amount_in
            .checked_mul(fee_multiplier)
            .ok_or(Error::ArithmeticOverflow)?;
        let numerator = amount_in_with_fee
            .checked_mul(reserve_out)
            .ok_or(Error::ArithmeticOverflow)?;
        let denominator = reserve_in
            .checked_mul(BPS_DENOMINATOR)
            .ok_or(Error::ArithmeticOverflow)?
            .checked_add(amount_in_with_fee)
            .ok_or(Error::ArithmeticOverflow)?;

        let amount_out = numerator
            .checked_div(denominator)
            .ok_or(Error::ArithmeticOverflow)?;

        if amount_out < min_amount_out {
            return Err(Error::SlippageExceeded);
        }

        let token_in_client = token::Client::new(&env, &token_in);
        let token_out_client = token::Client::new(
            &env,
            if is_token_0 {
                &state.token_1
            } else {
                &state.token_0
            },
        );

        token_in_client.transfer(&trader, &env.current_contract_address(), &amount_in);
        token_out_client.transfer(&env.current_contract_address(), &trader, &amount_out);

        if is_token_0 {
            state.reserve_0 = state
                .reserve_0
                .checked_add(amount_in)
                .ok_or(Error::ArithmeticOverflow)?;
            state.reserve_1 = state
                .reserve_1
                .checked_sub(amount_out)
                .ok_or(Error::ArithmeticOverflow)?;
        } else {
            state.reserve_1 = state
                .reserve_1
                .checked_add(amount_in)
                .ok_or(Error::ArithmeticOverflow)?;
            state.reserve_0 = state
                .reserve_0
                .checked_sub(amount_out)
                .ok_or(Error::ArithmeticOverflow)?;
        }

        env.storage().instance().set(&DataKey::State, &state);

        SwapExactInEvent {
            token_in,
            token_out: if is_token_0 {
                state.token_1.clone()
            } else {
                state.token_0.clone()
            },
            trader,
            amount_in,
            amount_out,
            fee_bps: state.fee_bps,
        }
        .publish(&env);

        Ok(amount_out)
    }

    pub fn get_state(env: Env) -> Result<PoolState, Error> {
        read_state(&env)
    }
}

// OpenZeppelin Token and Burnable implementations for LyraSwap
#[contractimpl]
impl FungibleToken for LyraSwap {
    type ContractType = Base;

    fn total_supply(e: &Env) -> i128 {
        Self::ContractType::total_supply(e)
    }

    fn balance(e: &Env, account: Address) -> i128 {
        Self::ContractType::balance(e, &account)
    }

    fn allowance(e: &Env, owner: Address, spender: Address) -> i128 {
        Self::ContractType::allowance(e, &owner, &spender)
    }

    fn transfer(e: &Env, from: Address, to: MuxedAddress, amount: i128) {
        Self::ContractType::transfer(e, &from, &to, amount);
    }

    fn transfer_from(e: &Env, spender: Address, from: Address, to: Address, amount: i128) {
        Self::ContractType::transfer_from(e, &spender, &from, &to, amount);
    }

    fn approve(e: &Env, owner: Address, spender: Address, amount: i128, live_until_ledger: u32) {
        Self::ContractType::approve(e, &owner, &spender, amount, live_until_ledger);
    }

    fn decimals(e: &Env) -> u32 {
        Self::ContractType::decimals(e)
    }

    fn name(e: &Env) -> String {
        Self::ContractType::name(e)
    }

    fn symbol(e: &Env) -> String {
        Self::ContractType::symbol(e)
    }
}

#[contractimpl]
impl FungibleBurnable for LyraSwap {
    fn burn(e: &Env, from: Address, amount: i128) {
        Base::burn(e, &from, amount);
    }

    fn burn_from(e: &Env, spender: Address, from: Address, amount: i128) {
        Base::burn_from(e, &spender, &from, amount);
    }
}

// OpenZeppelin Ownable implementation for LyraSwap
#[contractimpl]
impl Ownable for LyraSwap {
    fn get_owner(e: &Env) -> Option<Address> {
        ownable::get_owner(e)
    }

    fn transfer_ownership(e: &Env, new_owner: Address, live_until_ledger: u32) {
        ownable::transfer_ownership(e, &new_owner, live_until_ledger);
    }

    fn accept_ownership(e: &Env) {
        ownable::accept_ownership(e);
    }

    fn renounce_ownership(e: &Env) {
        ownable::renounce_ownership(e);
    }
}

fn read_state(env: &Env) -> Result<PoolState, Error> {
    env.storage()
        .instance()
        .get(&DataKey::State)
        .ok_or(Error::NotInitialized)
}

fn integer_sqrt(value: i128) -> Result<i128, Error> {
    if value < 0 {
        return Err(Error::ArithmeticOverflow);
    }

    let n = value as u128;
    if n == 0 {
        return Ok(0);
    }

    // Integer Newton iteration for floor(sqrt(n)) without floating point.
    let mut x0 = n;
    let mut x1 = (x0 + n / x0) / 2;

    while x1 < x0 {
        x0 = x1;
        x1 = (x0 + n / x0) / 2;
    }

    i128::try_from(x0).map_err(|_| Error::ArithmeticOverflow)
}
mod test;
