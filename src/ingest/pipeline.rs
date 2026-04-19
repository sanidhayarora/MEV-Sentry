use crate::decoder::{DecodeError, PendingTxDecoder};
use crate::engine::BundleSearchEngine;
use crate::mempool::{MempoolTracker, ObserveOutcome};
use crate::model::{AnalysisReport, PendingTransaction};
use crate::simulator::BundleSimulator;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineEvent {
    Observed(PendingTransaction),
    Included { tx_hash: String, block_number: u64 },
    Dropped { tx_hash: String },
    NewHead { block_number: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineEffect {
    TrackingUpdated(ObserveOutcome),
    DecodeFailed {
        tx_hash: String,
        error: DecodeError,
    },
    Analyzed(AnalysisReport),
    Included {
        tx_hash: String,
        block_number: u64,
    },
    Dropped {
        tx_hash: String,
    },
    HeadAdvanced {
        block_number: u64,
        active_transactions: usize,
    },
}

pub struct AnalysisPipeline<D, S> {
    tracker: MempoolTracker,
    decoder: D,
    engine: BundleSearchEngine<S>,
    latest_block: Option<u64>,
}

impl<D, S> AnalysisPipeline<D, S>
where
    D: PendingTxDecoder,
    S: BundleSimulator,
{
    pub fn new(decoder: D, engine: BundleSearchEngine<S>) -> Self {
        Self {
            tracker: MempoolTracker::new(),
            decoder,
            engine,
            latest_block: None,
        }
    }

    pub fn tracker(&self) -> &MempoolTracker {
        &self.tracker
    }

    pub fn latest_block(&self) -> Option<u64> {
        self.latest_block
    }

    pub fn handle_event(&mut self, event: PipelineEvent) -> Vec<PipelineEffect> {
        match event {
            PipelineEvent::Observed(tx) => self.handle_observed(tx),
            PipelineEvent::Included {
                tx_hash,
                block_number,
            } => self.handle_included(tx_hash, block_number),
            PipelineEvent::Dropped { tx_hash } => self.handle_dropped(tx_hash),
            PipelineEvent::NewHead { block_number } => self.handle_new_head(block_number),
        }
    }

    fn handle_observed(&mut self, tx: PendingTransaction) -> Vec<PipelineEffect> {
        let observe_outcome = self.tracker.observe(tx.clone());
        let mut effects = vec![PipelineEffect::TrackingUpdated(observe_outcome.clone())];

        if !matches!(
            observe_outcome,
            ObserveOutcome::NewActive { .. } | ObserveOutcome::Replaced { .. }
        ) {
            return effects;
        }

        match self.decoder.decode(&tx) {
            Ok(Some(victim)) => {
                effects.push(PipelineEffect::Analyzed(self.engine.analyze(&victim)))
            }
            Ok(None) => {}
            Err(error) => effects.push(PipelineEffect::DecodeFailed {
                tx_hash: tx.tx_hash.clone(),
                error,
            }),
        }

        effects
    }

    fn handle_included(&mut self, tx_hash: String, block_number: u64) -> Vec<PipelineEffect> {
        if self.tracker.mark_included(&tx_hash, block_number) {
            vec![PipelineEffect::Included {
                tx_hash,
                block_number,
            }]
        } else {
            Vec::new()
        }
    }

    fn handle_dropped(&mut self, tx_hash: String) -> Vec<PipelineEffect> {
        if self.tracker.mark_dropped(&tx_hash) {
            vec![PipelineEffect::Dropped { tx_hash }]
        } else {
            Vec::new()
        }
    }

    fn handle_new_head(&mut self, block_number: u64) -> Vec<PipelineEffect> {
        if self
            .latest_block
            .is_some_and(|current| block_number <= current)
        {
            return Vec::new();
        }

        self.latest_block = Some(block_number);
        vec![PipelineEffect::HeadAdvanced {
            block_number,
            active_transactions: self.tracker.active_len(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::UniswapV3RouterDecoder;
    use crate::engine::BundleSearchEngine;
    use crate::model::{Address, PendingTransaction, SearchConfig};
    use crate::uniswap_v3::{InitializedTick, UniswapV3Pool, UniswapV3SinglePoolSimulator};
    use ethnum::U256;

    const EXACT_INPUT_SINGLE_SELECTOR: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
    const WORD_SIZE: usize = 32;

    fn router() -> Address {
        Address::new([0x11; 20])
    }

    fn sender() -> Address {
        Address::new([0xaa; 20])
    }

    fn token_a() -> Address {
        Address::new([0x22; 20])
    }

    fn token_b() -> Address {
        Address::new([0x33; 20])
    }

    fn encode_address_word(address: Address) -> [u8; WORD_SIZE] {
        let mut word = [0_u8; WORD_SIZE];
        word[12..32].copy_from_slice(&address.as_bytes());
        word
    }

    fn encode_u32_word(value: u32) -> [u8; WORD_SIZE] {
        let mut word = [0_u8; WORD_SIZE];
        word[28..32].copy_from_slice(&value.to_be_bytes());
        word
    }

    fn encode_u128_word(value: u128) -> [u8; WORD_SIZE] {
        let mut word = [0_u8; WORD_SIZE];
        word[16..32].copy_from_slice(&value.to_be_bytes());
        word
    }

    fn encode_exact_input_single(min_amount_out: u128) -> Vec<u8> {
        let mut input = Vec::with_capacity(4 + 8 * WORD_SIZE);
        input.extend_from_slice(&EXACT_INPUT_SINGLE_SELECTOR);
        input.extend_from_slice(&encode_address_word(token_b()));
        input.extend_from_slice(&encode_address_word(token_a()));
        input.extend_from_slice(&encode_u32_word(3_000));
        input.extend_from_slice(&encode_address_word(Address::new([0x44; 20])));
        input.extend_from_slice(&encode_u128_word(1));
        input.extend_from_slice(&encode_u128_word(1_000));
        input.extend_from_slice(&encode_u128_word(min_amount_out));
        input.extend_from_slice(&encode_u128_word(0));
        input
    }

    fn supported_tx(
        tx_hash: &str,
        nonce: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
        min_amount_out: u128,
    ) -> PendingTransaction {
        PendingTransaction {
            tx_hash: tx_hash.to_string(),
            from: sender(),
            nonce,
            to: Some(router()),
            max_fee_per_gas,
            max_priority_fee_per_gas,
            input: encode_exact_input_single(min_amount_out),
        }
    }

    fn unsupported_tx(tx_hash: &str, nonce: u64) -> PendingTransaction {
        PendingTransaction {
            tx_hash: tx_hash.to_string(),
            from: sender(),
            nonce,
            to: Some(Address::new([0x99; 20])),
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 2,
            input: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    fn malformed_supported_tx(tx_hash: &str, nonce: u64) -> PendingTransaction {
        let mut input = encode_exact_input_single(995);
        input.pop();

        PendingTransaction {
            tx_hash: tx_hash.to_string(),
            from: sender(),
            nonce,
            to: Some(router()),
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 2,
            input,
        }
    }

    fn build_pipeline() -> AnalysisPipeline<UniswapV3RouterDecoder, UniswapV3SinglePoolSimulator> {
        let q96 = U256::from(1u8) << 96;
        let pool = UniswapV3Pool {
            pool: crate::model::PoolKey::new(token_a(), token_b(), 3_000).unwrap(),
            sqrt_price_x96: q96,
            current_tick: 0,
            liquidity: 1_000_000,
            initialized_ticks: vec![
                InitializedTick {
                    index: -100,
                    sqrt_price_x96: q96 / U256::from(2u8),
                    liquidity_net: 1_000_000,
                },
                InitializedTick {
                    index: 100,
                    sqrt_price_x96: q96 * U256::from(2u8),
                    liquidity_net: -1_000_000,
                },
            ],
        };
        let simulator = UniswapV3SinglePoolSimulator::new([pool]).unwrap();
        let engine = BundleSearchEngine::new(
            simulator,
            SearchConfig {
                min_attacker_input: 1_000,
                max_attacker_input: 1_000,
                attacker_input_step: 1_000,
                min_net_profit: 1,
            },
        )
        .unwrap();

        AnalysisPipeline::new(UniswapV3RouterDecoder::new([router()]), engine)
    }

    #[test]
    fn supported_observation_tracks_and_analyzes() {
        let mut pipeline = build_pipeline();

        let effects =
            pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

        assert_eq!(effects.len(), 2);
        assert_eq!(
            effects[0],
            PipelineEffect::TrackingUpdated(ObserveOutcome::NewActive {
                tx_hash: "0x1".to_string(),
            })
        );
        match &effects[1] {
            PipelineEffect::Analyzed(report) => {
                assert_eq!(report.tx_hash, "0x1");
                assert_eq!(report.evaluated_candidates, 2);
            }
            other => panic!("expected analysis effect, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_observation_does_not_reanalyze() {
        let mut pipeline = build_pipeline();
        pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

        let effects =
            pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

        assert_eq!(
            effects,
            vec![PipelineEffect::TrackingUpdated(ObserveOutcome::Duplicate {
                tx_hash: "0x1".to_string(),
            })]
        );
    }

    #[test]
    fn replacement_observation_reanalyzes_new_active_hash() {
        let mut pipeline = build_pipeline();
        pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

        let effects =
            pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x2", 7, 120, 3, 994)));

        assert_eq!(effects.len(), 2);
        assert_eq!(
            effects[0],
            PipelineEffect::TrackingUpdated(ObserveOutcome::Replaced {
                old_hash: "0x1".to_string(),
                new_hash: "0x2".to_string(),
            })
        );
        match &effects[1] {
            PipelineEffect::Analyzed(report) => assert_eq!(report.tx_hash, "0x2"),
            other => panic!("expected analysis effect, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_router_observation_only_updates_tracking() {
        let mut pipeline = build_pipeline();

        let effects = pipeline.handle_event(PipelineEvent::Observed(unsupported_tx("0x1", 7)));

        assert_eq!(
            effects,
            vec![PipelineEffect::TrackingUpdated(ObserveOutcome::NewActive {
                tx_hash: "0x1".to_string(),
            })]
        );
    }

    #[test]
    fn malformed_supported_tx_emits_decode_failure() {
        let mut pipeline = build_pipeline();

        let effects =
            pipeline.handle_event(PipelineEvent::Observed(malformed_supported_tx("0x1", 7)));

        assert_eq!(effects.len(), 2);
        assert_eq!(
            effects[0],
            PipelineEffect::TrackingUpdated(ObserveOutcome::NewActive {
                tx_hash: "0x1".to_string(),
            })
        );
        match &effects[1] {
            PipelineEffect::DecodeFailed { tx_hash, error } => {
                assert_eq!(tx_hash, "0x1");
                assert!(matches!(
                    error,
                    DecodeError::UnexpectedCalldataLength { .. }
                ));
            }
            other => panic!("expected decode failure, got {other:?}"),
        }
    }

    #[test]
    fn included_and_head_events_update_pipeline_state() {
        let mut pipeline = build_pipeline();
        pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

        let head_effects = pipeline.handle_event(PipelineEvent::NewHead { block_number: 10 });
        assert_eq!(
            head_effects,
            vec![PipelineEffect::HeadAdvanced {
                block_number: 10,
                active_transactions: 1,
            }]
        );
        assert_eq!(pipeline.latest_block(), Some(10));

        let included_effects = pipeline.handle_event(PipelineEvent::Included {
            tx_hash: "0x1".to_string(),
            block_number: 11,
        });
        assert_eq!(
            included_effects,
            vec![PipelineEffect::Included {
                tx_hash: "0x1".to_string(),
                block_number: 11,
            }]
        );
        assert_eq!(pipeline.tracker().active_len(), 0);

        let stale_head_effects = pipeline.handle_event(PipelineEvent::NewHead { block_number: 9 });
        assert!(stale_head_effects.is_empty());
    }
}
