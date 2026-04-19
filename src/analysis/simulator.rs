use crate::model::{BundleCandidate, VictimTransaction};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineSimulation {
    pub victim_output: u128,
    pub touched_pools: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateStatus {
    Feasible,
    VictimReverted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleSimulation {
    pub status: CandidateStatus,
    pub victim_output: Option<u128>,
    pub attacker_required_capital: u128,
    pub attacker_gross_profit: i128,
    pub gas_cost: u128,
    pub touched_pools: Vec<String>,
}

impl BundleSimulation {
    pub fn net_profit(&self) -> i128 {
        self.attacker_gross_profit - self.gas_cost as i128
    }

    pub fn break_even_priority_fee(&self) -> u128 {
        self.net_profit().max(0) as u128
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimulationError {
    Unsupported,
    StaleState,
    PoolNotFound,
    ArithmeticOverflow,
    InvalidInput(&'static str),
}

pub trait BundleSimulator {
    fn simulate_baseline(
        &self,
        victim: &VictimTransaction,
    ) -> Result<BaselineSimulation, SimulationError>;

    fn simulate_candidate(
        &self,
        victim: &VictimTransaction,
        candidate: &BundleCandidate,
    ) -> Result<BundleSimulation, SimulationError>;
}
