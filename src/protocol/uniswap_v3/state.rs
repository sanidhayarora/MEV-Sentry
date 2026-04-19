use std::io;
use std::net::TcpStream;

use ethnum::U256;
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::model::Address;
use crate::uniswap_v3::{UniswapV3Pool, UniswapV3SinglePoolSimulator};
use crate::SimulationError;

const SLOT0_SELECTOR: &str = "0x3850c7bd";
const LIQUIDITY_SELECTOR: &str = "0x1a686502";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredPoolSeed {
    pub address: Option<Address>,
    pub snapshot: UniswapV3Pool,
}

#[derive(Debug)]
pub enum StateError {
    Transport(tungstenite::Error),
    Json(serde_json::Error),
    Io(io::Error),
    Protocol(&'static str),
    InvalidHex(&'static str),
    QuantityOverflow(&'static str),
    Simulation(SimulationError),
}

impl From<tungstenite::Error> for StateError {
    fn from(value: tungstenite::Error) -> Self {
        Self::Transport(value)
    }
}

impl From<serde_json::Error> for StateError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<io::Error> for StateError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SimulationError> for StateError {
    fn from(value: SimulationError) -> Self {
        Self::Simulation(value)
    }
}

pub struct UniswapV3StateLoader {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_request_id: u64,
}

impl UniswapV3StateLoader {
    pub fn connect(endpoint: &str) -> Result<Self, StateError> {
        let (socket, _) = connect(endpoint)?;
        Ok(Self {
            socket,
            next_request_id: 1,
        })
    }

    pub fn refresh_simulator(
        &mut self,
        seeds: &[ConfiguredPoolSeed],
        simulator: &UniswapV3SinglePoolSimulator,
        block_number: Option<u64>,
    ) -> Result<(), StateError> {
        let pools = self.load_pools(seeds, block_number)?;
        simulator.replace_pools(pools)?;
        Ok(())
    }

    pub fn load_pools(
        &mut self,
        seeds: &[ConfiguredPoolSeed],
        block_number: Option<u64>,
    ) -> Result<Vec<UniswapV3Pool>, StateError> {
        let mut pools = Vec::with_capacity(seeds.len());

        for seed in seeds {
            match seed.address {
                Some(address) => {
                    let slot0 = self.eth_call(address, SLOT0_SELECTOR, block_number)?;
                    let liquidity = self.eth_call(address, LIQUIDITY_SELECTOR, block_number)?;
                    let (sqrt_price_x96, current_tick) = parse_slot0_result(&slot0)?;
                    let liquidity = parse_liquidity_result(&liquidity)?;
                    pools.push(overlay_live_state(
                        &seed.snapshot,
                        sqrt_price_x96,
                        current_tick,
                        liquidity,
                    ));
                }
                None => pools.push(seed.snapshot.clone()),
            }
        }

        Ok(pools)
    }

    fn eth_call(
        &mut self,
        to: Address,
        data: &str,
        block_number: Option<u64>,
    ) -> Result<String, StateError> {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.socket.send(Message::Text(
            build_eth_call_request(id, to, data, block_number).to_string(),
        ))?;

        loop {
            match self.socket.read()? {
                Message::Text(payload) => {
                    if let Some(result) = parse_eth_call_response(&payload, id)? {
                        return Ok(result);
                    }
                }
                Message::Binary(payload) => {
                    let text = String::from_utf8(payload).map_err(|_| {
                        StateError::Protocol("binary websocket frame must be utf-8")
                    })?;
                    if let Some(result) = parse_eth_call_response(&text, id)? {
                        return Ok(result);
                    }
                }
                Message::Ping(payload) => self.socket.send(Message::Pong(payload))?,
                Message::Pong(_) => {}
                Message::Frame(_) => {}
                Message::Close(_) => return Err(StateError::Protocol("websocket closed")),
            }
        }
    }
}

fn build_eth_call_request(id: u64, to: Address, data: &str, block_number: Option<u64>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "eth_call",
        "params": [
            {
                "to": to.to_hex(),
                "data": data,
            },
            block_tag(block_number),
        ],
    })
}

fn parse_eth_call_response(payload: &str, expected_id: u64) -> Result<Option<String>, StateError> {
    let value: Value = serde_json::from_str(payload)?;
    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return Ok(None);
    };
    if id != expected_id {
        return Ok(None);
    }

    let result = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or(StateError::Protocol("eth_call response missing result"))?;
    Ok(Some(result.to_string()))
}

fn block_tag(block_number: Option<u64>) -> String {
    block_number
        .map(|number| format!("0x{number:x}"))
        .unwrap_or_else(|| "latest".to_string())
}

fn overlay_live_state(
    snapshot: &UniswapV3Pool,
    sqrt_price_x96: U256,
    current_tick: i32,
    liquidity: u128,
) -> UniswapV3Pool {
    UniswapV3Pool {
        pool: snapshot.pool,
        sqrt_price_x96,
        current_tick,
        liquidity,
        initialized_ticks: snapshot.initialized_ticks.clone(),
    }
}

fn parse_slot0_result(result: &str) -> Result<(U256, i32), StateError> {
    let bytes = parse_hex_bytes(result, "slot0")?;
    if bytes.len() < 64 {
        return Err(StateError::Protocol("slot0 result too short"));
    }

    let sqrt_price_x96 = parse_u256_word(&bytes[..32]);
    let current_tick = parse_i24_word(&bytes[32..64]);
    Ok((sqrt_price_x96, current_tick))
}

fn parse_liquidity_result(result: &str) -> Result<u128, StateError> {
    let bytes = parse_hex_bytes(result, "liquidity")?;
    if bytes.len() != 32 {
        return Err(StateError::Protocol("liquidity result must be 32 bytes"));
    }
    parse_u128_word(&bytes)
}

fn parse_hex_bytes(value: &str, field: &'static str) -> Result<Vec<u8>, StateError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or(StateError::InvalidHex(field))?;
    if hex.len() % 2 != 0 {
        return Err(StateError::InvalidHex(field));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut index = 0;
    while index < hex.len() {
        let high = hex_value(hex.as_bytes()[index]).ok_or(StateError::InvalidHex(field))?;
        let low = hex_value(hex.as_bytes()[index + 1]).ok_or(StateError::InvalidHex(field))?;
        bytes.push((high << 4) | low);
        index += 2;
    }

    Ok(bytes)
}

fn parse_u256_word(word: &[u8]) -> U256 {
    word.iter().fold(U256::ZERO, |current, byte| {
        (current << 8) + U256::from(*byte)
    })
}

fn parse_u128_word(word: &[u8]) -> Result<u128, StateError> {
    if word[..16].iter().any(|byte| *byte != 0) {
        return Err(StateError::QuantityOverflow("liquidity"));
    }

    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&word[16..32]);
    Ok(u128::from_be_bytes(bytes))
}

fn parse_i24_word(word: &[u8]) -> i32 {
    let raw = ((word[29] as i32) << 16) | ((word[30] as i32) << 8) | word[31] as i32;
    if raw & 0x80_0000 != 0 {
        raw - 0x100_0000
    } else {
        raw
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitializedTick;

    fn sample_snapshot() -> UniswapV3Pool {
        let token0 = Address::new([0x22; 20]);
        let token1 = Address::new([0x33; 20]);
        UniswapV3Pool {
            pool: PoolKey::new(token0, token1, 3_000).unwrap(),
            sqrt_price_x96: U256::from(1u8) << 96,
            current_tick: 0,
            liquidity: 1_000_000,
            initialized_ticks: vec![
                InitializedTick {
                    index: -100,
                    sqrt_price_x96: (U256::from(1u8) << 96) / U256::from(2u8),
                    liquidity_net: 1_000_000,
                },
                InitializedTick {
                    index: 100,
                    sqrt_price_x96: (U256::from(1u8) << 96) * U256::from(2u8),
                    liquidity_net: -1_000_000,
                },
            ],
        }
    }

    use crate::PoolKey;

    #[test]
    fn builds_eth_call_request() {
        let request = build_eth_call_request(7, Address::new([0x11; 20]), SLOT0_SELECTOR, Some(10));

        assert_eq!(request["method"], "eth_call");
        assert_eq!(request["id"], 7);
        assert_eq!(request["params"][0]["data"], SLOT0_SELECTOR);
        assert_eq!(request["params"][1], "0xa");
    }

    #[test]
    fn parses_slot0_result_words() {
        let result = format!(
            "0x{sqrt}{tick}{rest}",
            sqrt = format!("{:064x}", U256::from(1u8) << 96),
            tick = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe7",
            rest = "00".repeat(32 * 5),
        );
        let (sqrt_price_x96, current_tick) = parse_slot0_result(&result).unwrap();

        assert_eq!(sqrt_price_x96, U256::from(1u8) << 96);
        assert_eq!(current_tick, -25);
    }

    #[test]
    fn parses_liquidity_result_word() {
        let result = format!("0x{:064x}", 1_500_000_u128);

        assert_eq!(parse_liquidity_result(&result).unwrap(), 1_500_000);
    }

    #[test]
    fn overlays_live_state_onto_seed_snapshot() {
        let seed = ConfiguredPoolSeed {
            address: Some(Address::new([0x44; 20])),
            snapshot: sample_snapshot(),
        };
        let refreshed = overlay_live_state(&seed.snapshot, U256::from(123u16), 7, 9);

        assert_eq!(refreshed.pool, seed.snapshot.pool);
        assert_eq!(refreshed.sqrt_price_x96, U256::from(123u16));
        assert_eq!(refreshed.current_tick, 7);
        assert_eq!(refreshed.liquidity, 9);
        assert_eq!(refreshed.initialized_ticks, seed.snapshot.initialized_ticks);
    }
}
