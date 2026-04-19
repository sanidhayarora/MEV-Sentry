use crate::model::{
    AnalysisReport, BundleCandidate, CandidateMetrics, RiskClassification, SearchConfig,
    SearchConfigError, StrategyKind, VictimTransaction,
};
use crate::simulator::{BundleSimulation, BundleSimulator, CandidateStatus};

const STRATEGIES: [StrategyKind; 2] = [StrategyKind::Sandwich, StrategyKind::PressureToRevert];

#[derive(Debug)]
pub enum EngineError {
    InvalidConfig(SearchConfigError),
}

impl From<SearchConfigError> for EngineError {
    fn from(value: SearchConfigError) -> Self {
        Self::InvalidConfig(value)
    }
}

pub struct BundleSearchEngine<S> {
    simulator: S,
    config: SearchConfig,
}

impl<S> BundleSearchEngine<S>
where
    S: BundleSimulator,
{
    pub fn new(simulator: S, config: SearchConfig) -> Result<Self, EngineError> {
        let config = config.validate()?;
        Ok(Self { simulator, config })
    }

    pub fn analyze(&self, victim: &VictimTransaction) -> AnalysisReport {
        let baseline = match self.simulator.simulate_baseline(victim) {
            Ok(result) => result,
            Err(error) => {
                return AnalysisReport {
                    tx_hash: victim.tx_hash.clone(),
                    classification: RiskClassification::Inconclusive,
                    confidence_bps: 0,
                    baseline_output: 0,
                    max_victim_loss: 0,
                    preventable_loss_bps: 0,
                    max_feasible_attacker_profit: 0,
                    min_attacker_capital: None,
                    break_even_priority_fee: None,
                    revert_threshold_input: None,
                    best_candidate: None,
                    evaluated_candidates: 0,
                    rejected_candidates: 0,
                    explanation: format!("baseline simulation failed: {error:?}"),
                };
            }
        };

        let mut best_candidate: Option<CandidateMetrics> = None;
        let mut min_attacker_capital: Option<u128> = None;
        let mut break_even_priority_fee: Option<u128> = None;
        let mut revert_threshold_input: Option<u128> = None;
        let mut evaluated_candidates = 0_u32;
        let mut rejected_candidates = 0_u32;

        for attacker_input in self.attacker_inputs() {
            for strategy in STRATEGIES {
                let candidate = BundleCandidate {
                    strategy,
                    attacker_input,
                };

                match self.simulator.simulate_candidate(victim, &candidate) {
                    Ok(outcome) => {
                        evaluated_candidates += 1;
                        self.track_outcome(
                            &baseline,
                            &candidate,
                            outcome,
                            &mut best_candidate,
                            &mut min_attacker_capital,
                            &mut break_even_priority_fee,
                            &mut revert_threshold_input,
                        );
                    }
                    Err(_) => {
                        rejected_candidates += 1;
                    }
                }
            }
        }

        self.build_report(
            victim,
            baseline.victim_output,
            best_candidate,
            min_attacker_capital,
            break_even_priority_fee,
            revert_threshold_input,
            evaluated_candidates,
            rejected_candidates,
        )
    }

    fn attacker_inputs(&self) -> impl Iterator<Item = u128> {
        let config = self.config;
        std::iter::successors(Some(config.min_attacker_input), move |current| {
            let next = current.saturating_add(config.attacker_input_step);
            (next <= config.max_attacker_input).then_some(next)
        })
    }

    fn track_outcome(
        &self,
        baseline: &crate::simulator::BaselineSimulation,
        candidate: &BundleCandidate,
        outcome: BundleSimulation,
        best_candidate: &mut Option<CandidateMetrics>,
        min_attacker_capital: &mut Option<u128>,
        break_even_priority_fee: &mut Option<u128>,
        revert_threshold_input: &mut Option<u128>,
    ) {
        if outcome.status == CandidateStatus::VictimReverted {
            *revert_threshold_input = Some(match *revert_threshold_input {
                Some(existing) => existing.min(candidate.attacker_input),
                None => candidate.attacker_input,
            });
            return;
        }

        let victim_output = outcome.victim_output.unwrap_or(0);
        let victim_loss = baseline.victim_output.saturating_sub(victim_output);
        let net_profit = outcome.net_profit();

        if net_profit < self.config.min_net_profit {
            return;
        }

        let metrics = CandidateMetrics {
            strategy: candidate.strategy,
            attacker_input: candidate.attacker_input,
            attacker_required_capital: outcome.attacker_required_capital,
            victim_output,
            victim_loss,
            preventable_loss_bps: compute_bps(victim_loss, baseline.victim_output),
            gross_profit: outcome.attacker_gross_profit,
            gas_cost: outcome.gas_cost,
            net_profit,
            break_even_priority_fee: outcome.break_even_priority_fee(),
            touched_pools: outcome.touched_pools,
        };

        *min_attacker_capital = Some(match *min_attacker_capital {
            Some(existing) => existing.min(metrics.attacker_required_capital),
            None => metrics.attacker_required_capital,
        });

        *break_even_priority_fee = Some(match *break_even_priority_fee {
            Some(existing) => existing.max(metrics.break_even_priority_fee),
            None => metrics.break_even_priority_fee,
        });

        let should_replace = best_candidate
            .as_ref()
            .map(|current| {
                metrics.net_profit > current.net_profit
                    || (metrics.net_profit == current.net_profit
                        && metrics.attacker_required_capital < current.attacker_required_capital)
            })
            .unwrap_or(true);

        if should_replace {
            *best_candidate = Some(metrics);
        }
    }

    fn build_report(
        &self,
        victim: &VictimTransaction,
        baseline_output: u128,
        best_candidate: Option<CandidateMetrics>,
        min_attacker_capital: Option<u128>,
        break_even_priority_fee: Option<u128>,
        revert_threshold_input: Option<u128>,
        evaluated_candidates: u32,
        rejected_candidates: u32,
    ) -> AnalysisReport {
        let classification = if best_candidate.is_some() {
            RiskClassification::Vulnerable
        } else if evaluated_candidates > 0 {
            RiskClassification::Safe
        } else {
            RiskClassification::Inconclusive
        };

        let confidence_bps = match classification {
            RiskClassification::Vulnerable if rejected_candidates == 0 => 10_000,
            RiskClassification::Vulnerable => 8_500,
            RiskClassification::Safe if rejected_candidates == 0 => 9_500,
            RiskClassification::Safe => 7_500,
            RiskClassification::Inconclusive => 2_500,
        };

        let (max_victim_loss, preventable_loss_bps, max_feasible_attacker_profit, explanation) =
            match &best_candidate {
                Some(candidate) => (
                    candidate.victim_loss,
                    candidate.preventable_loss_bps,
                    candidate.net_profit,
                    format!(
                        "best {:?} candidate uses attacker input {} with net profit {}",
                        candidate.strategy, candidate.attacker_input, candidate.net_profit
                    ),
                ),
                None if evaluated_candidates > 0 => (
                    0,
                    0,
                    0,
                    "candidate search completed but found no profitable bundle".to_string(),
                ),
                None => (
                    0,
                    0,
                    0,
                    "candidate search could not evaluate any bundle successfully".to_string(),
                ),
            };

        AnalysisReport {
            tx_hash: victim.tx_hash.clone(),
            classification,
            confidence_bps,
            baseline_output,
            max_victim_loss,
            preventable_loss_bps,
            max_feasible_attacker_profit,
            min_attacker_capital,
            break_even_priority_fee,
            revert_threshold_input,
            best_candidate,
            evaluated_candidates,
            rejected_candidates,
            explanation,
        }
    }
}

fn compute_bps(numerator: u128, denominator: u128) -> u32 {
    if denominator == 0 {
        return 0;
    }

    let scaled = numerator.saturating_mul(10_000);
    let value = scaled / denominator;
    value.min(u32::MAX as u128) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{Address, PoolKey, RouteHop, SwapDirection};
    use crate::simulator::{BaselineSimulation, SimulationError};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct BundleKey {
        strategy: StrategyKind,
        attacker_input: u128,
    }

    struct TableSimulator {
        baseline: Result<BaselineSimulation, SimulationError>,
        candidates: BTreeMap<BundleKey, Result<BundleSimulation, SimulationError>>,
    }

    impl BundleSimulator for TableSimulator {
        fn simulate_baseline(
            &self,
            _victim: &VictimTransaction,
        ) -> Result<BaselineSimulation, SimulationError> {
            self.baseline.clone()
        }

        fn simulate_candidate(
            &self,
            _victim: &VictimTransaction,
            candidate: &BundleCandidate,
        ) -> Result<BundleSimulation, SimulationError> {
            self.candidates
                .get(&BundleKey {
                    strategy: candidate.strategy,
                    attacker_input: candidate.attacker_input,
                })
                .cloned()
                .unwrap_or(Err(SimulationError::Unsupported))
        }
    }

    fn victim() -> VictimTransaction {
        VictimTransaction {
            tx_hash: "0xabc".to_string(),
            route: vec![RouteHop {
                pool: PoolKey::new(Address::new([0x10; 20]), Address::new([0x20; 20]), 3_000)
                    .unwrap(),
                direction: SwapDirection::OneForZero,
            }],
            amount_in: 1_000,
            min_amount_out: 900,
        }
    }

    fn config() -> SearchConfig {
        SearchConfig {
            min_attacker_input: 5,
            max_attacker_input: 20,
            attacker_input_step: 5,
            min_net_profit: 1,
        }
    }

    #[test]
    fn rejects_invalid_config() {
        let zero_step = SearchConfig {
            attacker_input_step: 0,
            ..config()
        };
        let invalid_profit = SearchConfig {
            min_net_profit: -1,
            ..config()
        };

        assert_eq!(zero_step.validate(), Err(SearchConfigError::ZeroSearchStep));
        assert_eq!(
            invalid_profit.validate(),
            Err(SearchConfigError::InvalidProfitThreshold)
        );
    }

    #[test]
    fn returns_inconclusive_when_baseline_fails() {
        let simulator = TableSimulator {
            baseline: Err(SimulationError::StaleState),
            candidates: BTreeMap::new(),
        };
        let engine = BundleSearchEngine::new(simulator, config()).unwrap();

        let report = engine.analyze(&victim());

        assert_eq!(report.classification, RiskClassification::Inconclusive);
        assert_eq!(report.evaluated_candidates, 0);
        assert!(report.explanation.contains("baseline simulation failed"));
    }

    #[test]
    fn finds_best_profitable_candidate_and_revert_threshold() {
        let mut candidates = BTreeMap::new();
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::PressureToRevert,
                attacker_input: 5,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::VictimReverted,
                victim_output: None,
                attacker_required_capital: 5,
                attacker_gross_profit: 0,
                gas_cost: 0,
                touched_pools: vec!["pool-a".to_string()],
            }),
        );
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::Sandwich,
                attacker_input: 10,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::Feasible,
                victim_output: Some(970),
                attacker_required_capital: 12,
                attacker_gross_profit: 25,
                gas_cost: 5,
                touched_pools: vec!["pool-a".to_string()],
            }),
        );
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::Sandwich,
                attacker_input: 20,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::Feasible,
                victim_output: Some(940),
                attacker_required_capital: 30,
                attacker_gross_profit: 60,
                gas_cost: 10,
                touched_pools: vec!["pool-a".to_string(), "pool-b".to_string()],
            }),
        );

        let simulator = TableSimulator {
            baseline: Ok(BaselineSimulation {
                victim_output: 1_000,
                touched_pools: vec!["pool-a".to_string()],
            }),
            candidates,
        };
        let engine = BundleSearchEngine::new(simulator, config()).unwrap();

        let report = engine.analyze(&victim());

        assert_eq!(report.classification, RiskClassification::Vulnerable);
        assert_eq!(report.revert_threshold_input, Some(5));
        assert_eq!(report.min_attacker_capital, Some(12));
        assert_eq!(report.max_feasible_attacker_profit, 50);
        assert_eq!(report.max_victim_loss, 60);
        assert_eq!(report.preventable_loss_bps, 600);
        assert_eq!(report.break_even_priority_fee, Some(50));
        assert_eq!(
            report
                .best_candidate
                .as_ref()
                .map(|candidate| candidate.attacker_input),
            Some(20)
        );
    }

    #[test]
    fn safe_when_candidates_evaluate_but_none_are_profitable() {
        let mut candidates = BTreeMap::new();
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::Sandwich,
                attacker_input: 10,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::Feasible,
                victim_output: Some(990),
                attacker_required_capital: 10,
                attacker_gross_profit: 3,
                gas_cost: 5,
                touched_pools: vec!["pool-a".to_string()],
            }),
        );

        let simulator = TableSimulator {
            baseline: Ok(BaselineSimulation {
                victim_output: 1_000,
                touched_pools: vec!["pool-a".to_string()],
            }),
            candidates,
        };
        let engine = BundleSearchEngine::new(simulator, config()).unwrap();

        let report = engine.analyze(&victim());

        assert_eq!(report.classification, RiskClassification::Safe);
        assert_eq!(report.best_candidate, None);
        assert_eq!(report.min_attacker_capital, None);
        assert_eq!(report.max_feasible_attacker_profit, 0);
        assert!(report.evaluated_candidates > 0);
    }

    #[test]
    fn prefers_lower_capital_when_net_profit_ties() {
        let mut candidates = BTreeMap::new();
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::Sandwich,
                attacker_input: 10,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::Feasible,
                victim_output: Some(980),
                attacker_required_capital: 11,
                attacker_gross_profit: 22,
                gas_cost: 2,
                touched_pools: vec!["pool-a".to_string()],
            }),
        );
        candidates.insert(
            BundleKey {
                strategy: StrategyKind::PressureToRevert,
                attacker_input: 15,
            },
            Ok(BundleSimulation {
                status: CandidateStatus::Feasible,
                victim_output: Some(980),
                attacker_required_capital: 15,
                attacker_gross_profit: 22,
                gas_cost: 2,
                touched_pools: vec!["pool-a".to_string()],
            }),
        );

        let simulator = TableSimulator {
            baseline: Ok(BaselineSimulation {
                victim_output: 1_000,
                touched_pools: vec!["pool-a".to_string()],
            }),
            candidates,
        };
        let engine = BundleSearchEngine::new(simulator, config()).unwrap();

        let report = engine.analyze(&victim());
        let best = report.best_candidate.expect("best candidate");

        assert_eq!(best.strategy, StrategyKind::Sandwich);
        assert_eq!(best.attacker_required_capital, 11);
    }
}
