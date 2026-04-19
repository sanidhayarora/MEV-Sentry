use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address([u8; 20]);

impl Address {
    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(self) -> [u8; 20] {
        self.0
    }

    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(42);
        output.push_str("0x");

        for byte in self.0 {
            output.push(nibble_to_hex(byte >> 4));
            output.push(nibble_to_hex(byte & 0x0f));
        }

        output
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolKeyError {
    IdenticalTokens,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolKey {
    pub token0: Address,
    pub token1: Address,
    pub fee_pips: u32,
}

impl PoolKey {
    pub fn new(token_a: Address, token_b: Address, fee_pips: u32) -> Result<Self, PoolKeyError> {
        if token_a == token_b {
            return Err(PoolKeyError::IdenticalTokens);
        }

        let (token0, token1) = if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        };

        Ok(Self {
            token0,
            token1,
            fee_pips,
        })
    }

    pub fn from_swap(
        token_in: Address,
        token_out: Address,
        fee_pips: u32,
    ) -> Result<(Self, SwapDirection), PoolKeyError> {
        let key = Self::new(token_in, token_out, fee_pips)?;
        let direction = if token_in == key.token0 {
            SwapDirection::ZeroForOne
        } else {
            SwapDirection::OneForZero
        };

        Ok((key, direction))
    }

    pub fn id(self) -> String {
        format!("{}:{}:{}", self.token0, self.token1, self.fee_pips)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteHop {
    pub pool: PoolKey,
    pub direction: SwapDirection,
}

impl RouteHop {
    pub fn input_token(self) -> Address {
        match self.direction {
            SwapDirection::ZeroForOne => self.pool.token0,
            SwapDirection::OneForZero => self.pool.token1,
        }
    }

    pub fn output_token(self) -> Address {
        match self.direction {
            SwapDirection::ZeroForOne => self.pool.token1,
            SwapDirection::OneForZero => self.pool.token0,
        }
    }

    pub fn reversed(self) -> Self {
        Self {
            pool: self.pool,
            direction: self.direction.opposite(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VictimTransactionError {
    EmptyRoute,
    DisconnectedRoute,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VictimTransaction {
    pub tx_hash: String,
    pub route: Vec<RouteHop>,
    pub amount_in: u128,
    pub min_amount_out: u128,
}

impl VictimTransaction {
    pub fn validate(&self) -> Result<(), VictimTransactionError> {
        let Some((first, rest)) = self.route.split_first() else {
            return Err(VictimTransactionError::EmptyRoute);
        };

        let mut expected_input = first.output_token();
        for hop in rest {
            if hop.input_token() != expected_input {
                return Err(VictimTransactionError::DisconnectedRoute);
            }
            expected_input = hop.output_token();
        }

        Ok(())
    }

    pub fn touched_pools(&self) -> Vec<String> {
        self.route.iter().map(|hop| hop.pool.id()).collect()
    }

    pub fn reverse_route(&self) -> Vec<RouteHop> {
        self.route.iter().rev().map(|hop| hop.reversed()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTransaction {
    pub tx_hash: String,
    pub from: Address,
    pub nonce: u64,
    pub to: Option<Address>,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub input: Vec<u8>,
}

impl PendingTransaction {
    pub fn identity(&self) -> TxIdentity {
        TxIdentity {
            from: self.from,
            nonce: self.nonce,
        }
    }

    pub fn replacement_rank(&self) -> (u128, u128) {
        (self.max_fee_per_gas, self.max_priority_fee_per_gas)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxIdentity {
    pub from: Address,
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SwapDirection {
    ZeroForOne,
    OneForZero,
}

impl SwapDirection {
    pub fn opposite(self) -> Self {
        match self {
            Self::ZeroForOne => Self::OneForZero,
            Self::OneForZero => Self::ZeroForOne,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StrategyKind {
    Sandwich,
    PressureToRevert,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleCandidate {
    pub strategy: StrategyKind,
    pub attacker_input: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateMetrics {
    pub strategy: StrategyKind,
    pub attacker_input: u128,
    pub attacker_required_capital: u128,
    pub victim_output: u128,
    pub victim_loss: u128,
    pub preventable_loss_bps: u32,
    pub gross_profit: i128,
    pub gas_cost: u128,
    pub net_profit: i128,
    pub break_even_priority_fee: u128,
    pub touched_pools: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskClassification {
    Safe,
    Vulnerable,
    Inconclusive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisReport {
    pub tx_hash: String,
    pub classification: RiskClassification,
    pub confidence_bps: u16,
    pub baseline_output: u128,
    pub max_victim_loss: u128,
    pub preventable_loss_bps: u32,
    pub max_feasible_attacker_profit: i128,
    pub min_attacker_capital: Option<u128>,
    pub break_even_priority_fee: Option<u128>,
    pub revert_threshold_input: Option<u128>,
    pub best_candidate: Option<CandidateMetrics>,
    pub evaluated_candidates: u32,
    pub rejected_candidates: u32,
    pub explanation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    pub min_attacker_input: u128,
    pub max_attacker_input: u128,
    pub attacker_input_step: u128,
    pub min_net_profit: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchConfigError {
    EmptySearchRange,
    ZeroSearchStep,
    InvalidProfitThreshold,
}

impl SearchConfig {
    pub fn validate(self) -> Result<Self, SearchConfigError> {
        if self.min_attacker_input > self.max_attacker_input {
            return Err(SearchConfigError::EmptySearchRange);
        }
        if self.attacker_input_step == 0 {
            return Err(SearchConfigError::ZeroSearchStep);
        }
        if self.min_net_profit < 0 {
            return Err(SearchConfigError::InvalidProfitThreshold);
        }
        Ok(self)
    }
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be less than 16"),
    }
}
