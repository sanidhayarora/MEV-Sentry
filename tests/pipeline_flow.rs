use ethnum::U256;
use mev_sentry::protocol::uniswap_v3::InitializedTick;
use mev_sentry::{
    Address, AnalysisPipeline, BundleSearchEngine, ObserveOutcome, PendingTransaction,
    PipelineEffect, PipelineEvent, SearchConfig, UniswapV3Pool, UniswapV3RouterDecoder,
    UniswapV3SinglePoolSimulator,
};

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

fn build_pipeline() -> AnalysisPipeline<UniswapV3RouterDecoder, UniswapV3SinglePoolSimulator> {
    let q96 = U256::from(1u8) << 96;
    let pool = UniswapV3Pool {
        pool: mev_sentry::PoolKey::new(token_a(), token_b(), 3_000).unwrap(),
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
fn replacement_observation_reanalyzes_new_active_hash() {
    let mut pipeline = build_pipeline();
    pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x1", 7, 100, 2, 995)));

    let effects =
        pipeline.handle_event(PipelineEvent::Observed(supported_tx("0x2", 7, 200, 3, 990)));

    assert_eq!(
        effects[0],
        PipelineEffect::TrackingUpdated(ObserveOutcome::Replaced {
            old_hash: "0x1".to_string(),
            new_hash: "0x2".to_string(),
        })
    );
    assert!(matches!(effects.get(1), Some(PipelineEffect::Analyzed(_))));
}
