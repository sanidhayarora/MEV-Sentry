pub mod state;

pub use state::{ConfiguredPoolSeed, StateError, UniswapV3StateLoader};

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use ethnum::U256;

use crate::model::{
    BundleCandidate, PoolKey, RouteHop, StrategyKind, SwapDirection, VictimTransaction,
    VictimTransactionError,
};
use crate::simulator::{
    BaselineSimulation, BundleSimulation, BundleSimulator, CandidateStatus, SimulationError,
};

const FEE_DENOMINATOR: u128 = 1_000_000;
const Q96_SHIFT: u32 = 96;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitializedTick {
    pub index: i32,
    pub sqrt_price_x96: U256,
    pub liquidity_net: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniswapV3Pool {
    pub pool: PoolKey,
    pub sqrt_price_x96: U256,
    pub current_tick: i32,
    pub liquidity: u128,
    pub initialized_ticks: Vec<InitializedTick>,
}

impl UniswapV3Pool {
    pub fn validate(&self) -> Result<(), SimulationError> {
        if self.initialized_ticks.len() < 2 {
            return Err(SimulationError::InvalidInput(
                "at least two initialized ticks are required",
            ));
        }

        validate_ticks(&self.initialized_ticks)?;

        let (lower, upper) = active_boundaries(
            &self.initialized_ticks,
            self.current_tick,
            self.sqrt_price_x96,
        )?;

        if self.sqrt_price_x96 < lower.sqrt_price_x96 || self.sqrt_price_x96 > upper.sqrt_price_x96
        {
            return Err(SimulationError::InvalidInput(
                "current sqrt price must sit within the active initialized range",
            ));
        }

        Ok(())
    }

    fn fee_pips(&self) -> u32 {
        self.pool.fee_pips
    }

    fn next_initialized_tick(&self, direction: SwapDirection) -> Option<&InitializedTick> {
        let upper_index = self
            .initialized_ticks
            .partition_point(|tick| tick.index <= self.current_tick);

        match direction {
            SwapDirection::OneForZero => self.initialized_ticks.get(upper_index),
            SwapDirection::ZeroForOne => upper_index
                .checked_sub(1)
                .and_then(|index| self.initialized_ticks.get(index)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SwapOutcome {
    next_pool: UniswapV3Pool,
    amount_out: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteSimulation {
    amount_out: u128,
    touched_pools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SwapStep {
    sqrt_price_next_x96: U256,
    amount_in: U256,
    amount_out: U256,
    fee_amount: U256,
}

#[derive(Clone, Debug)]
pub struct UniswapV3SinglePoolSimulator {
    pools: Arc<RwLock<BTreeMap<PoolKey, UniswapV3Pool>>>,
}

impl UniswapV3SinglePoolSimulator {
    pub fn new<I>(pools: I) -> Result<Self, SimulationError>
    where
        I: IntoIterator<Item = UniswapV3Pool>,
    {
        let mut registry = BTreeMap::new();

        for pool in pools {
            pool.validate()?;
            if registry.insert(pool.pool, pool).is_some() {
                return Err(SimulationError::InvalidInput("duplicate pool"));
            }
        }

        Ok(Self {
            pools: Arc::new(RwLock::new(registry)),
        })
    }

    pub fn replace_pools<I>(&self, pools: I) -> Result<(), SimulationError>
    where
        I: IntoIterator<Item = UniswapV3Pool>,
    {
        let mut registry = BTreeMap::new();

        for pool in pools {
            pool.validate()?;
            if registry.insert(pool.pool, pool).is_some() {
                return Err(SimulationError::InvalidInput("duplicate pool"));
            }
        }

        let mut current = self
            .pools
            .write()
            .map_err(|_| SimulationError::InvalidInput("pool registry lock poisoned"))?;
        *current = registry;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Vec<UniswapV3Pool>, SimulationError> {
        let registry = self
            .pools
            .read()
            .map_err(|_| SimulationError::InvalidInput("pool registry lock poisoned"))?;
        Ok(registry.values().cloned().collect())
    }

    fn simulate_route(
        &self,
        pools: &mut BTreeMap<PoolKey, UniswapV3Pool>,
        route: &[RouteHop],
        amount_in: u128,
    ) -> Result<RouteSimulation, SimulationError> {
        if route.is_empty() {
            return Err(SimulationError::InvalidInput(
                "victim route must not be empty",
            ));
        }

        let mut amount = amount_in;
        let mut touched_pools = Vec::with_capacity(route.len());

        for hop in route {
            let pool = pools
                .get(&hop.pool)
                .cloned()
                .ok_or(SimulationError::PoolNotFound)?;
            let outcome = self.swap_exact_input(&pool, hop.direction, amount)?;
            pools.insert(hop.pool, outcome.next_pool);
            amount = outcome.amount_out;
            touched_pools.push(hop.pool.id());
        }

        Ok(RouteSimulation {
            amount_out: amount,
            touched_pools,
        })
    }

    fn swap_exact_input(
        &self,
        pool: &UniswapV3Pool,
        direction: SwapDirection,
        amount_in: u128,
    ) -> Result<SwapOutcome, SimulationError> {
        if amount_in == 0 {
            return Err(SimulationError::InvalidInput("amount in must be positive"));
        }

        let mut state = pool.clone();
        let mut amount_remaining = U256::from(amount_in);
        let mut amount_out_total = U256::ZERO;

        while amount_remaining > U256::ZERO {
            let boundary = state
                .next_initialized_tick(direction)
                .cloned()
                .ok_or(SimulationError::Unsupported)?;

            let step = compute_swap_step(
                state.sqrt_price_x96,
                boundary.sqrt_price_x96,
                state.liquidity,
                amount_remaining,
                state.fee_pips(),
                direction,
            )?;

            let amount_consumed = step
                .amount_in
                .checked_add(step.fee_amount)
                .ok_or(SimulationError::ArithmeticOverflow)?;
            amount_remaining = amount_remaining
                .checked_sub(amount_consumed)
                .ok_or(SimulationError::ArithmeticOverflow)?;
            amount_out_total = amount_out_total
                .checked_add(step.amount_out)
                .ok_or(SimulationError::ArithmeticOverflow)?;
            state.sqrt_price_x96 = step.sqrt_price_next_x96;

            if state.sqrt_price_x96 == boundary.sqrt_price_x96 {
                let liquidity_delta = match direction {
                    SwapDirection::OneForZero => boundary.liquidity_net,
                    SwapDirection::ZeroForOne => boundary
                        .liquidity_net
                        .checked_neg()
                        .ok_or(SimulationError::ArithmeticOverflow)?,
                };

                state.liquidity = add_liquidity_delta(state.liquidity, liquidity_delta)?;
                state.current_tick = match direction {
                    SwapDirection::OneForZero => boundary.index,
                    SwapDirection::ZeroForOne => boundary
                        .index
                        .checked_sub(1)
                        .ok_or(SimulationError::ArithmeticOverflow)?,
                };
            } else {
                break;
            }
        }

        Ok(SwapOutcome {
            next_pool: state,
            amount_out: to_u128(amount_out_total)?,
        })
    }
}

impl BundleSimulator for UniswapV3SinglePoolSimulator {
    fn simulate_baseline(
        &self,
        victim: &VictimTransaction,
    ) -> Result<BaselineSimulation, SimulationError> {
        validate_victim(victim)?;
        let mut pools = self
            .pools
            .read()
            .map_err(|_| SimulationError::InvalidInput("pool registry lock poisoned"))?
            .clone();
        let outcome = self.simulate_route(&mut pools, &victim.route, victim.amount_in)?;

        if outcome.amount_out < victim.min_amount_out {
            return Err(SimulationError::InvalidInput(
                "victim does not satisfy min amount out at baseline",
            ));
        }

        Ok(BaselineSimulation {
            victim_output: outcome.amount_out,
            touched_pools: outcome.touched_pools,
        })
    }

    fn simulate_candidate(
        &self,
        victim: &VictimTransaction,
        candidate: &BundleCandidate,
    ) -> Result<BundleSimulation, SimulationError> {
        validate_victim(victim)?;
        let mut pools = self
            .pools
            .read()
            .map_err(|_| SimulationError::InvalidInput("pool registry lock poisoned"))?
            .clone();
        let attacker_front =
            self.simulate_route(&mut pools, &victim.route, candidate.attacker_input)?;
        let victim_outcome = self.simulate_route(&mut pools, &victim.route, victim.amount_in)?;
        let mut touched_pools = attacker_front.touched_pools.clone();
        append_unique(&mut touched_pools, &victim_outcome.touched_pools);

        if victim_outcome.amount_out < victim.min_amount_out {
            return Ok(BundleSimulation {
                status: CandidateStatus::VictimReverted,
                victim_output: None,
                attacker_required_capital: candidate.attacker_input,
                attacker_gross_profit: 0,
                gas_cost: 0,
                touched_pools,
            });
        }

        let attacker_gross_profit = match candidate.strategy {
            StrategyKind::Sandwich => {
                let attacker_back = self.simulate_route(
                    &mut pools,
                    &victim.reverse_route(),
                    attacker_front.amount_out,
                )?;
                append_unique(&mut touched_pools, &attacker_back.touched_pools);
                let unwind_output = i128::try_from(attacker_back.amount_out)
                    .map_err(|_| SimulationError::ArithmeticOverflow)?;
                let capital_in = i128::try_from(candidate.attacker_input)
                    .map_err(|_| SimulationError::ArithmeticOverflow)?;
                unwind_output - capital_in
            }
            StrategyKind::PressureToRevert => 0,
        };

        Ok(BundleSimulation {
            status: CandidateStatus::Feasible,
            victim_output: Some(victim_outcome.amount_out),
            attacker_required_capital: candidate.attacker_input,
            attacker_gross_profit,
            gas_cost: 0,
            touched_pools,
        })
    }
}

fn validate_victim(victim: &VictimTransaction) -> Result<(), SimulationError> {
    victim.validate().map_err(|error| match error {
        VictimTransactionError::EmptyRoute => {
            SimulationError::InvalidInput("victim route must not be empty")
        }
        VictimTransactionError::DisconnectedRoute => {
            SimulationError::InvalidInput("victim route must be token-contiguous")
        }
    })
}

fn append_unique(target: &mut Vec<String>, additional: &[String]) {
    for pool_id in additional {
        if !target.contains(pool_id) {
            target.push(pool_id.clone());
        }
    }
}

fn validate_ticks(ticks: &[InitializedTick]) -> Result<(), SimulationError> {
    for window in ticks.windows(2) {
        let current = &window[0];
        let next = &window[1];

        if current.sqrt_price_x96 == U256::ZERO || next.sqrt_price_x96 == U256::ZERO {
            return Err(SimulationError::InvalidInput(
                "initialized tick sqrt prices must be positive",
            ));
        }
        if current.index >= next.index {
            return Err(SimulationError::InvalidInput(
                "initialized ticks must be strictly increasing by index",
            ));
        }
        if current.sqrt_price_x96 >= next.sqrt_price_x96 {
            return Err(SimulationError::InvalidInput(
                "initialized ticks must be strictly increasing by sqrt price",
            ));
        }
    }

    Ok(())
}

fn active_boundaries(
    ticks: &[InitializedTick],
    current_tick: i32,
    _sqrt_price_x96: U256,
) -> Result<(&InitializedTick, &InitializedTick), SimulationError> {
    let upper_index = ticks.partition_point(|tick| tick.index <= current_tick);
    let lower = upper_index
        .checked_sub(1)
        .and_then(|index| ticks.get(index))
        .ok_or(SimulationError::InvalidInput(
            "current tick must be bracketed by initialized ticks",
        ))?;
    let upper = ticks.get(upper_index).ok_or(SimulationError::InvalidInput(
        "current tick must be bracketed by initialized ticks",
    ))?;

    Ok((lower, upper))
}

fn compute_swap_step(
    sqrt_price_current_x96: U256,
    sqrt_price_target_x96: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee_pips: u32,
    direction: SwapDirection,
) -> Result<SwapStep, SimulationError> {
    let amount_remaining_less_fee = mul_div_u256(
        amount_remaining,
        U256::from(FEE_DENOMINATOR - fee_pips as u128),
        U256::from(FEE_DENOMINATOR),
    )?;

    let amount_in_to_target = match direction {
        SwapDirection::ZeroForOne => amount0_delta(
            sqrt_price_target_x96,
            sqrt_price_current_x96,
            liquidity,
            true,
        )?,
        SwapDirection::OneForZero => amount1_delta(
            sqrt_price_current_x96,
            sqrt_price_target_x96,
            liquidity,
            true,
        )?,
    };

    let sqrt_price_next_x96 = if amount_remaining_less_fee >= amount_in_to_target {
        sqrt_price_target_x96
    } else {
        get_next_sqrt_price_from_input(
            sqrt_price_current_x96,
            liquidity,
            amount_remaining_less_fee,
            direction,
        )?
    };

    let reached_target = sqrt_price_next_x96 == sqrt_price_target_x96;

    let (amount_in, amount_out) = match direction {
        SwapDirection::ZeroForOne => (
            if reached_target {
                amount_in_to_target
            } else {
                amount0_delta(sqrt_price_next_x96, sqrt_price_current_x96, liquidity, true)?
            },
            amount1_delta(
                sqrt_price_next_x96,
                sqrt_price_current_x96,
                liquidity,
                false,
            )?,
        ),
        SwapDirection::OneForZero => (
            if reached_target {
                amount_in_to_target
            } else {
                amount1_delta(sqrt_price_current_x96, sqrt_price_next_x96, liquidity, true)?
            },
            amount0_delta(
                sqrt_price_current_x96,
                sqrt_price_next_x96,
                liquidity,
                false,
            )?,
        ),
    };

    let fee_amount = if sqrt_price_next_x96 != sqrt_price_target_x96 {
        amount_remaining
            .checked_sub(amount_in)
            .ok_or(SimulationError::ArithmeticOverflow)?
    } else {
        mul_div_rounding_up_u256(
            amount_in,
            U256::from(fee_pips as u128),
            U256::from(FEE_DENOMINATOR - fee_pips as u128),
        )?
    };

    Ok(SwapStep {
        sqrt_price_next_x96,
        amount_in,
        amount_out,
        fee_amount,
    })
}

fn get_next_sqrt_price_from_input(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in_less_fee: U256,
    direction: SwapDirection,
) -> Result<U256, SimulationError> {
    match direction {
        SwapDirection::ZeroForOne => get_next_sqrt_price_from_amount0_rounding_up(
            sqrt_price_x96,
            liquidity,
            amount_in_less_fee,
            true,
        ),
        SwapDirection::OneForZero => get_next_sqrt_price_from_amount1_rounding_down(
            sqrt_price_x96,
            liquidity,
            amount_in_less_fee,
            true,
        ),
    }
}

fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> Result<U256, SimulationError> {
    if amount == U256::ZERO {
        return Ok(sqrt_price_x96);
    }

    let numerator1 = U256::from(liquidity) << Q96_SHIFT;
    let product = amount
        .checked_mul(sqrt_price_x96)
        .ok_or(SimulationError::ArithmeticOverflow)?;

    let denominator = if add {
        numerator1
            .checked_add(product)
            .ok_or(SimulationError::ArithmeticOverflow)?
    } else {
        numerator1
            .checked_sub(product)
            .ok_or(SimulationError::ArithmeticOverflow)?
    };

    div_rounding_up(
        numerator1
            .checked_mul(sqrt_price_x96)
            .ok_or(SimulationError::ArithmeticOverflow)?,
        denominator,
    )
}

fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount: U256,
    add: bool,
) -> Result<U256, SimulationError> {
    let liquidity_u256 = U256::from(liquidity);
    let quotient = if add {
        mul_div_u256(amount, q96(), liquidity_u256)?
    } else {
        mul_div_rounding_up_u256(amount, q96(), liquidity_u256)?
    };

    if add {
        sqrt_price_x96
            .checked_add(quotient)
            .ok_or(SimulationError::ArithmeticOverflow)
    } else {
        sqrt_price_x96
            .checked_sub(quotient)
            .ok_or(SimulationError::ArithmeticOverflow)
    }
}

fn amount0_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, SimulationError> {
    let (sqrt_lower, sqrt_upper) = sort_prices(sqrt_ratio_a_x96, sqrt_ratio_b_x96);
    if sqrt_lower == U256::ZERO {
        return Err(SimulationError::InvalidInput("sqrt price must be positive"));
    }

    let numerator1 = U256::from(liquidity) << Q96_SHIFT;
    let numerator2 = sqrt_upper
        .checked_sub(sqrt_lower)
        .ok_or(SimulationError::ArithmeticOverflow)?;

    if round_up {
        let inner = mul_div_rounding_up_u256(numerator1, numerator2, sqrt_upper)?;
        div_rounding_up(inner, sqrt_lower)
    } else {
        let inner = mul_div_u256(numerator1, numerator2, sqrt_upper)?;
        Ok(inner / sqrt_lower)
    }
}

fn amount1_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, SimulationError> {
    let (sqrt_lower, sqrt_upper) = sort_prices(sqrt_ratio_a_x96, sqrt_ratio_b_x96);
    let diff = sqrt_upper
        .checked_sub(sqrt_lower)
        .ok_or(SimulationError::ArithmeticOverflow)?;

    if round_up {
        mul_div_rounding_up_u256(U256::from(liquidity), diff, q96())
    } else {
        mul_div_u256(U256::from(liquidity), diff, q96())
    }
}

fn add_liquidity_delta(liquidity: u128, delta: i128) -> Result<u128, SimulationError> {
    if delta >= 0 {
        liquidity
            .checked_add(delta as u128)
            .ok_or(SimulationError::ArithmeticOverflow)
    } else {
        liquidity
            .checked_sub(delta.unsigned_abs())
            .ok_or(SimulationError::ArithmeticOverflow)
    }
}

fn q96() -> U256 {
    U256::from(1u8) << Q96_SHIFT
}

fn sort_prices(a: U256, b: U256) -> (U256, U256) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn mul_div_u256(a: U256, b: U256, denominator: U256) -> Result<U256, SimulationError> {
    if denominator == U256::ZERO {
        return Err(SimulationError::InvalidInput("division by zero"));
    }

    Ok(a.checked_mul(b)
        .ok_or(SimulationError::ArithmeticOverflow)?
        / denominator)
}

fn mul_div_rounding_up_u256(a: U256, b: U256, denominator: U256) -> Result<U256, SimulationError> {
    if denominator == U256::ZERO {
        return Err(SimulationError::InvalidInput("division by zero"));
    }

    let product = a
        .checked_mul(b)
        .ok_or(SimulationError::ArithmeticOverflow)?;
    let quotient = product / denominator;
    let remainder = product % denominator;

    if remainder == U256::ZERO {
        Ok(quotient)
    } else {
        quotient
            .checked_add(U256::ONE)
            .ok_or(SimulationError::ArithmeticOverflow)
    }
}

fn div_rounding_up(numerator: U256, denominator: U256) -> Result<U256, SimulationError> {
    if denominator == U256::ZERO {
        return Err(SimulationError::InvalidInput("division by zero"));
    }

    let quotient = numerator / denominator;
    let remainder = numerator % denominator;

    if remainder == U256::ZERO {
        Ok(quotient)
    } else {
        quotient
            .checked_add(U256::ONE)
            .ok_or(SimulationError::ArithmeticOverflow)
    }
}

fn to_u128(value: U256) -> Result<u128, SimulationError> {
    u128::try_from(value).map_err(|_| SimulationError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q96() -> U256 {
        U256::from(1u8) << Q96_SHIFT
    }

    fn sample_ticks() -> Vec<InitializedTick> {
        let q96 = q96();
        vec![
            InitializedTick {
                index: -100,
                sqrt_price_x96: q96 / U256::from(2u8),
                liquidity_net: 1_000_000,
            },
            InitializedTick {
                index: 100,
                sqrt_price_x96: q96 * U256::from(2u8),
                liquidity_net: 500_000,
            },
            InitializedTick {
                index: 200,
                sqrt_price_x96: q96 * U256::from(4u8),
                liquidity_net: -1_500_000,
            },
        ]
    }

    fn sample_pool() -> UniswapV3Pool {
        let token0 = crate::model::Address::new([0x22; 20]);
        let token1 = crate::model::Address::new([0x33; 20]);
        UniswapV3Pool {
            pool: crate::model::PoolKey::new(token0, token1, 3_000).unwrap(),
            sqrt_price_x96: q96(),
            current_tick: 0,
            liquidity: 1_000_000,
            initialized_ticks: sample_ticks(),
        }
    }

    fn upper_range_pool() -> UniswapV3Pool {
        let mut pool = sample_pool();
        pool.sqrt_price_x96 = q96() * U256::from(2u8);
        pool.current_tick = 100;
        pool.liquidity = 1_500_000;
        pool
    }

    fn second_pool() -> UniswapV3Pool {
        let token0 = crate::model::Address::new([0x33; 20]);
        let token1 = crate::model::Address::new([0x44; 20]);
        UniswapV3Pool {
            pool: crate::model::PoolKey::new(token0, token1, 500).unwrap(),
            sqrt_price_x96: q96(),
            current_tick: 0,
            liquidity: 2_000_000,
            initialized_ticks: vec![
                InitializedTick {
                    index: -100,
                    sqrt_price_x96: q96() / U256::from(2u8),
                    liquidity_net: 2_000_000,
                },
                InitializedTick {
                    index: 100,
                    sqrt_price_x96: q96() * U256::from(2u8),
                    liquidity_net: -2_000_000,
                },
            ],
        }
    }

    fn sample_victim(min_amount_out: u128) -> VictimTransaction {
        VictimTransaction {
            tx_hash: "0xvictim".to_string(),
            route: vec![RouteHop {
                pool: sample_pool().pool,
                direction: SwapDirection::OneForZero,
            }],
            amount_in: 1_000,
            min_amount_out,
        }
    }

    fn multi_hop_victim(min_amount_out: u128) -> VictimTransaction {
        VictimTransaction {
            tx_hash: "0xmultihop".to_string(),
            route: vec![
                RouteHop {
                    pool: sample_pool().pool,
                    direction: SwapDirection::ZeroForOne,
                },
                RouteHop {
                    pool: second_pool().pool,
                    direction: SwapDirection::ZeroForOne,
                },
            ],
            amount_in: 1_000,
            min_amount_out,
        }
    }

    #[test]
    fn validates_pool_shape() {
        let mut invalid = sample_pool();
        invalid.initialized_ticks = vec![invalid.initialized_ticks[0].clone()];

        assert_eq!(
            invalid.validate(),
            Err(SimulationError::InvalidInput(
                "at least two initialized ticks are required"
            ))
        );
    }

    #[test]
    fn baseline_simulation_matches_expected_active_range_output() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let baseline = simulator.simulate_baseline(&sample_victim(996)).unwrap();

        assert_eq!(baseline.victim_output, 996);
        assert_eq!(baseline.touched_pools, vec![sample_pool().pool.id()]);
    }

    #[test]
    fn baseline_simulation_composes_multi_hop_route() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool(), second_pool()]).unwrap();
        let baseline = simulator.simulate_baseline(&multi_hop_victim(990)).unwrap();

        assert_eq!(baseline.victim_output, 994);
        assert_eq!(
            baseline.touched_pools,
            vec![sample_pool().pool.id(), second_pool().pool.id()]
        );
    }

    #[test]
    fn crossing_into_next_liquidity_range_updates_tick_and_liquidity() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let outcome = simulator
            .swap_exact_input(&sample_pool(), SwapDirection::OneForZero, 1_500_000)
            .unwrap();

        assert_eq!(outcome.next_pool.current_tick, 100);
        assert_eq!(outcome.next_pool.liquidity, 1_500_000);
        assert!(outcome.next_pool.sqrt_price_x96 > q96() * U256::from(2u8));
        assert!(outcome.amount_out > 333_333);
    }

    #[test]
    fn supports_swap_that_would_previously_have_been_rejected_at_range_boundary() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let victim = VictimTransaction {
            amount_in: 1_500_000,
            min_amount_out: 400_000,
            ..sample_victim(1)
        };

        let baseline = simulator.simulate_baseline(&victim).unwrap();

        assert!(baseline.victim_output >= 462_000);
    }

    #[test]
    fn zero_for_one_crossing_applies_inverse_liquidity_net() {
        let simulator = UniswapV3SinglePoolSimulator::new([upper_range_pool()]).unwrap();
        let outcome = simulator
            .swap_exact_input(&upper_range_pool(), SwapDirection::ZeroForOne, 1_000)
            .unwrap();

        assert_eq!(outcome.next_pool.current_tick, 99);
        assert_eq!(outcome.next_pool.liquidity, 1_000_000);
        assert!(outcome.next_pool.sqrt_price_x96 < q96() * U256::from(2u8));
        assert!(outcome.amount_out > 0);
    }

    #[test]
    fn zero_liquidity_gap_can_cross_without_consuming_input() {
        let token0 = crate::model::Address::new([0x22; 20]);
        let token1 = crate::model::Address::new([0x33; 20]);
        let pool = UniswapV3Pool {
            pool: crate::model::PoolKey::new(token0, token1, 3_000).unwrap(),
            sqrt_price_x96: q96() * U256::from(3u8),
            current_tick: 150,
            liquidity: 0,
            initialized_ticks: vec![
                InitializedTick {
                    index: -100,
                    sqrt_price_x96: q96() / U256::from(2u8),
                    liquidity_net: 1_000_000,
                },
                InitializedTick {
                    index: 100,
                    sqrt_price_x96: q96() * U256::from(2u8),
                    liquidity_net: -1_000_000,
                },
                InitializedTick {
                    index: 200,
                    sqrt_price_x96: q96() * U256::from(4u8),
                    liquidity_net: 2_000_000,
                },
                InitializedTick {
                    index: 300,
                    sqrt_price_x96: q96() * U256::from(8u8),
                    liquidity_net: -2_000_000,
                },
            ],
        };
        let simulator = UniswapV3SinglePoolSimulator::new([pool.clone()]).unwrap();

        let outcome = simulator
            .swap_exact_input(&pool, SwapDirection::OneForZero, 1_000)
            .unwrap();

        assert_eq!(outcome.next_pool.current_tick, 200);
        assert_eq!(outcome.next_pool.liquidity, 2_000_000);
        assert!(outcome.next_pool.sqrt_price_x96 > q96() * U256::from(4u8));
        assert!(outcome.amount_out > 0);
    }

    #[test]
    fn rejects_when_running_out_of_initialized_ticks() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let oversized = VictimTransaction {
            amount_in: 5_000_000,
            ..sample_victim(1)
        };

        let error = simulator.simulate_baseline(&oversized).unwrap_err();
        assert_eq!(error, SimulationError::Unsupported);
    }

    #[test]
    fn sandwich_candidate_tracks_victim_harm_and_realized_pnl() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let candidate = BundleCandidate {
            strategy: StrategyKind::Sandwich,
            attacker_input: 1_000,
        };

        let outcome = simulator
            .simulate_candidate(&sample_victim(990), &candidate)
            .unwrap();

        assert_eq!(outcome.status, CandidateStatus::Feasible);
        assert_eq!(outcome.victim_output, Some(994));
        assert_eq!(outcome.attacker_required_capital, 1_000);
        assert_eq!(outcome.attacker_gross_profit, -5);
    }

    #[test]
    fn multi_hop_sandwich_reports_all_touched_pools() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool(), second_pool()]).unwrap();
        let candidate = BundleCandidate {
            strategy: StrategyKind::Sandwich,
            attacker_input: 1_000,
        };

        let outcome = simulator
            .simulate_candidate(&multi_hop_victim(990), &candidate)
            .unwrap();

        assert_eq!(outcome.status, CandidateStatus::Feasible);
        assert_eq!(outcome.victim_output, Some(991));
        assert_eq!(
            outcome.touched_pools,
            vec![sample_pool().pool.id(), second_pool().pool.id()]
        );
        assert!(outcome.attacker_gross_profit < 0);
    }

    #[test]
    fn pressure_strategy_detects_revert_threshold() {
        let simulator = UniswapV3SinglePoolSimulator::new([sample_pool()]).unwrap();
        let candidate = BundleCandidate {
            strategy: StrategyKind::PressureToRevert,
            attacker_input: 1_000,
        };

        let outcome = simulator
            .simulate_candidate(&sample_victim(995), &candidate)
            .unwrap();

        assert_eq!(outcome.status, CandidateStatus::VictimReverted);
        assert_eq!(outcome.victim_output, None);
        assert_eq!(outcome.attacker_required_capital, 1_000);
    }
}
