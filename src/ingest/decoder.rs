use std::collections::BTreeSet;

use crate::model::{
    Address, PendingTransaction, PoolKey, PoolKeyError, RouteHop, VictimTransaction,
};

const EXACT_INPUT_SELECTOR: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
const EXACT_INPUT_SINGLE_SELECTOR: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
const EXACT_INPUT_WORDS: usize = 5;
const EXACT_INPUT_SINGLE_WORDS: usize = 8;
const WORD_SIZE: usize = 32;
const PATH_SEGMENT_SIZE: usize = 23;
const PATH_MIN_LENGTH: usize = 20 + PATH_SEGMENT_SIZE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    CalldataTooShort,
    UnexpectedCalldataLength { expected: usize, actual: usize },
    InvalidAddress,
    ValueOverflow,
    NonZeroSqrtPriceLimit,
    IdenticalTokens,
    InvalidPathLength,
}

impl From<PoolKeyError> for DecodeError {
    fn from(value: PoolKeyError) -> Self {
        match value {
            PoolKeyError::IdenticalTokens => Self::IdenticalTokens,
        }
    }
}

#[derive(Clone, Debug)]
pub struct UniswapV3RouterDecoder {
    routers: BTreeSet<Address>,
}

pub trait PendingTxDecoder {
    fn decode(&self, tx: &PendingTransaction) -> Result<Option<VictimTransaction>, DecodeError>;
}

impl UniswapV3RouterDecoder {
    pub fn new<I>(routers: I) -> Self
    where
        I: IntoIterator<Item = Address>,
    {
        Self {
            routers: routers.into_iter().collect(),
        }
    }

    pub fn decode(
        &self,
        tx: &PendingTransaction,
    ) -> Result<Option<VictimTransaction>, DecodeError> {
        let Some(to) = tx.to else {
            return Ok(None);
        };

        if !self.routers.contains(&to) {
            return Ok(None);
        }

        if tx.input.len() < 4 {
            return Err(DecodeError::CalldataTooShort);
        }

        let selector = &tx.input[..4];
        if selector == EXACT_INPUT_SELECTOR {
            self.decode_exact_input(tx)
        } else if selector == EXACT_INPUT_SINGLE_SELECTOR {
            self.decode_exact_input_single(tx)
        } else {
            Ok(None)
        }
    }

    fn decode_exact_input_single(
        &self,
        tx: &PendingTransaction,
    ) -> Result<Option<VictimTransaction>, DecodeError> {
        let expected_len = 4 + EXACT_INPUT_SINGLE_WORDS * WORD_SIZE;
        if tx.input.len() != expected_len {
            return Err(DecodeError::UnexpectedCalldataLength {
                expected: expected_len,
                actual: tx.input.len(),
            });
        }

        let payload = &tx.input[4..];
        let token_in = parse_address_word(word(payload, 0)?)?;
        let token_out = parse_address_word(word(payload, 1)?)?;
        let fee_pips = parse_u24_word(word(payload, 2)?)?;
        let amount_in = parse_u128_word(word(payload, 5)?)?;
        let min_amount_out = parse_u128_word(word(payload, 6)?)?;

        if !is_zero_word(word(payload, 7)?) {
            return Err(DecodeError::NonZeroSqrtPriceLimit);
        }

        let (pool, direction) = PoolKey::from_swap(token_in, token_out, fee_pips)?;

        Ok(Some(VictimTransaction {
            tx_hash: tx.tx_hash.clone(),
            route: vec![RouteHop { pool, direction }],
            amount_in,
            min_amount_out,
        }))
    }

    fn decode_exact_input(
        &self,
        tx: &PendingTransaction,
    ) -> Result<Option<VictimTransaction>, DecodeError> {
        let payload = &tx.input[4..];
        if payload.len() < EXACT_INPUT_WORDS * WORD_SIZE {
            return Err(DecodeError::CalldataTooShort);
        }

        let path_offset = parse_usize_word(word(payload, 0)?)?;
        let amount_in = parse_u128_word(word(payload, 3)?)?;
        let min_amount_out = parse_u128_word(word(payload, 4)?)?;
        let path = parse_dynamic_bytes(payload, path_offset)?;
        let route = decode_path(path)?;

        Ok(Some(VictimTransaction {
            tx_hash: tx.tx_hash.clone(),
            route,
            amount_in,
            min_amount_out,
        }))
    }
}

impl PendingTxDecoder for UniswapV3RouterDecoder {
    fn decode(&self, tx: &PendingTransaction) -> Result<Option<VictimTransaction>, DecodeError> {
        UniswapV3RouterDecoder::decode(self, tx)
    }
}

fn word(payload: &[u8], index: usize) -> Result<&[u8], DecodeError> {
    let start = index * WORD_SIZE;
    let end = start + WORD_SIZE;
    payload.get(start..end).ok_or(DecodeError::CalldataTooShort)
}

fn parse_address_word(word: &[u8]) -> Result<Address, DecodeError> {
    if word[..12].iter().any(|byte| *byte != 0) {
        return Err(DecodeError::InvalidAddress);
    }

    Ok(parse_address_bytes(&word[12..32]))
}

fn parse_u24_word(word: &[u8]) -> Result<u32, DecodeError> {
    if word[..29].iter().any(|byte| *byte != 0) {
        return Err(DecodeError::ValueOverflow);
    }

    Ok(parse_u24_bytes(&word[29..32]))
}

fn parse_u128_word(word: &[u8]) -> Result<u128, DecodeError> {
    if word[..16].iter().any(|byte| *byte != 0) {
        return Err(DecodeError::ValueOverflow);
    }

    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&word[16..32]);
    Ok(u128::from_be_bytes(bytes))
}

fn parse_usize_word(word: &[u8]) -> Result<usize, DecodeError> {
    let value = parse_u128_word(word)?;
    usize::try_from(value).map_err(|_| DecodeError::ValueOverflow)
}

fn parse_dynamic_bytes<'a>(payload: &'a [u8], offset: usize) -> Result<&'a [u8], DecodeError> {
    let length_word = payload
        .get(offset..offset + WORD_SIZE)
        .ok_or(DecodeError::CalldataTooShort)?;
    let length = parse_usize_word(length_word)?;
    let data_start = offset + WORD_SIZE;
    let padded_length = round_up_to_word(length);
    let expected_payload_len = data_start
        .checked_add(padded_length)
        .ok_or(DecodeError::ValueOverflow)?;

    if payload.len() != expected_payload_len {
        return Err(DecodeError::UnexpectedCalldataLength {
            expected: 4 + expected_payload_len,
            actual: 4 + payload.len(),
        });
    }

    payload
        .get(data_start..data_start + length)
        .ok_or(DecodeError::CalldataTooShort)
}

fn decode_path(path: &[u8]) -> Result<Vec<RouteHop>, DecodeError> {
    if path.len() < PATH_MIN_LENGTH || (path.len() - 20) % PATH_SEGMENT_SIZE != 0 {
        return Err(DecodeError::InvalidPathLength);
    }

    let hop_count = (path.len() - 20) / PATH_SEGMENT_SIZE;
    let mut route = Vec::with_capacity(hop_count);
    let mut token_in = parse_address_bytes(&path[..20]);
    let mut cursor = 20;

    while cursor < path.len() {
        let fee_pips = parse_u24_bytes(&path[cursor..cursor + 3]);
        let token_out = parse_address_bytes(&path[cursor + 3..cursor + PATH_SEGMENT_SIZE]);
        let (pool, direction) = PoolKey::from_swap(token_in, token_out, fee_pips)?;
        route.push(RouteHop { pool, direction });
        token_in = token_out;
        cursor += PATH_SEGMENT_SIZE;
    }

    Ok(route)
}

fn parse_address_bytes(bytes: &[u8]) -> Address {
    let mut address = [0_u8; 20];
    address.copy_from_slice(bytes);
    Address::new(address)
}

fn parse_u24_bytes(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32
}

fn round_up_to_word(value: usize) -> usize {
    let remainder = value % WORD_SIZE;
    if remainder == 0 {
        value
    } else {
        value + (WORD_SIZE - remainder)
    }
}

fn is_zero_word(word: &[u8]) -> bool {
    word.iter().all(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BundleSearchEngine;
    use crate::model::{RiskClassification, SearchConfig, SwapDirection};
    use crate::uniswap_v3::{InitializedTick, UniswapV3Pool, UniswapV3SinglePoolSimulator};
    use ethnum::U256;

    fn router() -> Address {
        Address::new([0x11; 20])
    }

    fn token_a() -> Address {
        Address::new([0x22; 20])
    }

    fn token_b() -> Address {
        Address::new([0x33; 20])
    }

    fn token_c() -> Address {
        Address::new([0x44; 20])
    }

    fn encode_exact_input_single(
        token_in: Address,
        token_out: Address,
        fee_pips: u32,
        recipient: Address,
        deadline: u128,
        amount_in: u128,
        amount_out_minimum: u128,
        sqrt_price_limit_x96: u128,
    ) -> Vec<u8> {
        let mut input = Vec::with_capacity(4 + EXACT_INPUT_SINGLE_WORDS * WORD_SIZE);
        input.extend_from_slice(&EXACT_INPUT_SINGLE_SELECTOR);
        input.extend_from_slice(&encode_address_word(token_in));
        input.extend_from_slice(&encode_address_word(token_out));
        input.extend_from_slice(&encode_u32_word(fee_pips));
        input.extend_from_slice(&encode_address_word(recipient));
        input.extend_from_slice(&encode_u128_word(deadline));
        input.extend_from_slice(&encode_u128_word(amount_in));
        input.extend_from_slice(&encode_u128_word(amount_out_minimum));
        input.extend_from_slice(&encode_u128_word(sqrt_price_limit_x96));
        input
    }

    fn encode_exact_input(
        path: &[u8],
        recipient: Address,
        deadline: u128,
        amount_in: u128,
        amount_out_minimum: u128,
    ) -> Vec<u8> {
        let mut input = Vec::with_capacity(
            4 + EXACT_INPUT_WORDS * WORD_SIZE + WORD_SIZE + round_up_to_word(path.len()),
        );
        input.extend_from_slice(&EXACT_INPUT_SELECTOR);
        input.extend_from_slice(&encode_u128_word((EXACT_INPUT_WORDS * WORD_SIZE) as u128));
        input.extend_from_slice(&encode_address_word(recipient));
        input.extend_from_slice(&encode_u128_word(deadline));
        input.extend_from_slice(&encode_u128_word(amount_in));
        input.extend_from_slice(&encode_u128_word(amount_out_minimum));
        input.extend_from_slice(&encode_u128_word(path.len() as u128));
        input.extend_from_slice(path);
        input.resize(
            4 + EXACT_INPUT_WORDS * WORD_SIZE + WORD_SIZE + round_up_to_word(path.len()),
            0,
        );
        input
    }

    fn encode_path(tokens: &[Address], fees: &[u32]) -> Vec<u8> {
        assert_eq!(tokens.len(), fees.len() + 1);
        let mut path = Vec::with_capacity(20 + fees.len() * PATH_SEGMENT_SIZE);
        path.extend_from_slice(&tokens[0].as_bytes());

        for (fee_pips, token) in fees.iter().zip(tokens.iter().skip(1)) {
            path.push((fee_pips >> 16) as u8);
            path.push((fee_pips >> 8) as u8);
            path.push(*fee_pips as u8);
            path.extend_from_slice(&token.as_bytes());
        }

        path
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

    fn sample_tx(input: Vec<u8>) -> PendingTransaction {
        PendingTransaction {
            tx_hash: "0xtx".to_string(),
            from: Address::new([0xaa; 20]),
            nonce: 7,
            to: Some(router()),
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 2,
            input,
        }
    }

    #[test]
    fn ignores_transactions_to_other_routers() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = PendingTransaction {
            tx_hash: "0xother".to_string(),
            from: Address::new([0xaa; 20]),
            nonce: 7,
            to: Some(Address::new([0x99; 20])),
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 2,
            input: vec![0u8; 4],
        };

        assert_eq!(decoder.decode(&tx), Ok(None));
    }

    #[test]
    fn decodes_exact_input_single_into_canonical_route() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = sample_tx(encode_exact_input_single(
            token_b(),
            token_a(),
            3_000,
            Address::new([0x44; 20]),
            1,
            1_000,
            900,
            0,
        ));

        let victim = decoder.decode(&tx).unwrap().expect("decoded victim");

        assert_eq!(victim.tx_hash, "0xtx");
        assert_eq!(victim.route.len(), 1);
        assert_eq!(victim.route[0].pool.token0, token_a());
        assert_eq!(victim.route[0].pool.token1, token_b());
        assert_eq!(victim.route[0].pool.fee_pips, 3_000);
        assert_eq!(victim.route[0].direction, SwapDirection::OneForZero);
        assert_eq!(victim.amount_in, 1_000);
        assert_eq!(victim.min_amount_out, 900);
    }

    #[test]
    fn decodes_exact_input_path_into_ordered_route() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = sample_tx(encode_exact_input(
            &encode_path(&[token_a(), token_b(), token_c()], &[500, 3_000]),
            Address::new([0x55; 20]),
            1,
            2_000,
            1_500,
        ));

        let victim = decoder.decode(&tx).unwrap().expect("decoded victim");

        assert_eq!(victim.route.len(), 2);
        assert_eq!(victim.route[0].pool.fee_pips, 500);
        assert_eq!(victim.route[0].direction, SwapDirection::ZeroForOne);
        assert_eq!(victim.route[1].pool.fee_pips, 3_000);
        assert_eq!(victim.route[1].direction, SwapDirection::ZeroForOne);
        assert_eq!(victim.amount_in, 2_000);
        assert_eq!(victim.min_amount_out, 1_500);
    }

    #[test]
    fn rejects_non_zero_sqrt_price_limit() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = sample_tx(encode_exact_input_single(
            token_a(),
            token_b(),
            3_000,
            Address::new([0x44; 20]),
            1,
            1_000,
            900,
            1,
        ));

        let error = decoder.decode(&tx).unwrap_err();
        assert_eq!(error, DecodeError::NonZeroSqrtPriceLimit);
    }

    #[test]
    fn decodes_pending_tx_and_produces_analysis_report() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let pending = sample_tx(encode_exact_input_single(
            token_b(),
            token_a(),
            3_000,
            Address::new([0x44; 20]),
            1,
            1_000,
            995,
            0,
        ));
        let victim = decoder.decode(&pending).unwrap().expect("decoded victim");

        let q96 = U256::from(1u8) << 96;
        let pool = UniswapV3Pool {
            pool: victim.route[0].pool,
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

        let report = engine.analyze(&victim);

        assert_eq!(report.classification, RiskClassification::Safe);
        assert_eq!(report.revert_threshold_input, Some(1_000));
        assert_eq!(report.best_candidate, None);
        assert_eq!(report.evaluated_candidates, 2);
    }

    #[test]
    fn ignores_other_selectors_even_for_supported_router() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = sample_tx(vec![0xde, 0xad, 0xbe, 0xef]);

        assert_eq!(decoder.decode(&tx), Ok(None));
    }

    #[test]
    fn rejects_unexpected_calldata_length_for_targeted_selector() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let mut input = encode_exact_input_single(
            token_a(),
            token_b(),
            3_000,
            Address::new([0x44; 20]),
            1,
            1_000,
            900,
            0,
        );
        input.pop();

        let error = decoder.decode(&sample_tx(input)).unwrap_err();
        assert_eq!(
            error,
            DecodeError::UnexpectedCalldataLength {
                expected: 260,
                actual: 259,
            }
        );
    }

    #[test]
    fn rejects_invalid_exact_input_path_length() {
        let decoder = UniswapV3RouterDecoder::new([router()]);
        let tx = sample_tx(encode_exact_input(
            &[0_u8; 20],
            Address::new([0x55; 20]),
            1,
            2_000,
            1_500,
        ));

        let error = decoder.decode(&tx).unwrap_err();
        assert_eq!(error, DecodeError::InvalidPathLength);
    }
}
