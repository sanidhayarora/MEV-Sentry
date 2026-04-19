use std::fmt;
use std::fs;
use std::path::Path;

use ethnum::U256;
use serde_json::Value;

use crate::{Address, ConfiguredPoolSeed, InitializedTick, PoolKey, SearchConfig, UniswapV3Pool};

#[derive(Debug)]
pub enum AppConfigError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Validation(String),
}

impl fmt::Display for AppConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to read config: {error}"),
            Self::Json(error) => write!(f, "invalid config json: {error}"),
            Self::Validation(message) => f.write_str(message),
        }
    }
}

impl From<std::io::Error> for AppConfigError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AppConfigError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub ws_endpoint: String,
    pub routers: Vec<Address>,
    pub search: SearchConfig,
    pub pool_seeds: Vec<ConfiguredPoolSeed>,
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, AppConfigError> {
        let path = path.as_ref();
        let config_text = fs::read_to_string(path)?;
        let config_json: Value = serde_json::from_str(&config_text)?;
        Self::parse(&config_json)
    }

    pub fn parse(value: &Value) -> Result<Self, AppConfigError> {
        let object = value
            .as_object()
            .ok_or_else(|| Self::validation("config root must be a JSON object"))?;

        let ws_endpoint = required_string(object, "ws_endpoint")?;
        let routers = required_array(object, "routers")?
            .iter()
            .map(parse_address_value)
            .collect::<Result<Vec<_>, _>>()?;
        if routers.is_empty() {
            return Err(Self::validation("routers must not be empty"));
        }

        let search = parse_search_config(
            object
                .get("search")
                .ok_or_else(|| Self::validation("missing search configuration"))?,
        )?;

        let pool_seeds = required_array(object, "pools")?
            .iter()
            .map(parse_pool_seed)
            .collect::<Result<Vec<_>, _>>()?;
        if pool_seeds.is_empty() {
            return Err(Self::validation("pools must not be empty"));
        }

        Ok(Self {
            ws_endpoint,
            routers,
            search,
            pool_seeds,
        })
    }

    pub fn pool_count(&self) -> usize {
        self.pool_seeds.len()
    }

    pub fn has_live_pool_seeds(&self) -> bool {
        self.pool_seeds.iter().any(|seed| seed.address.is_some())
    }

    pub fn live_pool_count(&self) -> usize {
        self.pool_seeds
            .iter()
            .filter(|seed| seed.address.is_some())
            .count()
    }

    fn validation(message: impl Into<String>) -> AppConfigError {
        AppConfigError::Validation(message.into())
    }
}

fn parse_search_config(value: &Value) -> Result<SearchConfig, AppConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| AppConfig::validation("search must be a JSON object"))?;

    Ok(SearchConfig {
        min_attacker_input: parse_u128_value(
            object
                .get("min_attacker_input")
                .ok_or_else(|| AppConfig::validation("missing search.min_attacker_input"))?,
        )?,
        max_attacker_input: parse_u128_value(
            object
                .get("max_attacker_input")
                .ok_or_else(|| AppConfig::validation("missing search.max_attacker_input"))?,
        )?,
        attacker_input_step: parse_u128_value(
            object
                .get("attacker_input_step")
                .ok_or_else(|| AppConfig::validation("missing search.attacker_input_step"))?,
        )?,
        min_net_profit: parse_i128_value(
            object
                .get("min_net_profit")
                .ok_or_else(|| AppConfig::validation("missing search.min_net_profit"))?,
        )?,
    })
}

fn parse_pool_seed(value: &Value) -> Result<ConfiguredPoolSeed, AppConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| AppConfig::validation("pool must be a JSON object"))?;
    let address = object.get("address").map(parse_address_value).transpose()?;
    let token0 = parse_address_value(
        object
            .get("token0")
            .ok_or_else(|| AppConfig::validation("missing pool.token0"))?,
    )?;
    let token1 = parse_address_value(
        object
            .get("token1")
            .ok_or_else(|| AppConfig::validation("missing pool.token1"))?,
    )?;
    let fee_pips = parse_u32_value(
        object
            .get("fee_pips")
            .ok_or_else(|| AppConfig::validation("missing pool.fee_pips"))?,
    )?;
    let initialized_ticks = required_array(object, "initialized_ticks")?
        .iter()
        .map(parse_tick)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ConfiguredPoolSeed {
        address,
        snapshot: UniswapV3Pool {
            pool: PoolKey::new(token0, token1, fee_pips)
                .map_err(|error| AppConfig::validation(format!("invalid pool key: {error:?}")))?,
            sqrt_price_x96: parse_u256_value(
                object
                    .get("sqrt_price_x96")
                    .ok_or_else(|| AppConfig::validation("missing pool.sqrt_price_x96"))?,
            )?,
            current_tick: parse_i32_value(
                object
                    .get("current_tick")
                    .ok_or_else(|| AppConfig::validation("missing pool.current_tick"))?,
            )?,
            liquidity: parse_u128_value(
                object
                    .get("liquidity")
                    .ok_or_else(|| AppConfig::validation("missing pool.liquidity"))?,
            )?,
            initialized_ticks,
        },
    })
}

fn parse_tick(value: &Value) -> Result<InitializedTick, AppConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| AppConfig::validation("initialized tick must be a JSON object"))?;

    Ok(InitializedTick {
        index: parse_i32_value(
            object
                .get("index")
                .ok_or_else(|| AppConfig::validation("missing initialized_tick.index"))?,
        )?,
        sqrt_price_x96: parse_u256_value(
            object
                .get("sqrt_price_x96")
                .ok_or_else(|| AppConfig::validation("missing initialized_tick.sqrt_price_x96"))?,
        )?,
        liquidity_net: parse_i128_value(
            object
                .get("liquidity_net")
                .ok_or_else(|| AppConfig::validation("missing initialized_tick.liquidity_net"))?,
        )?,
    })
}

fn parse_address_value(value: &Value) -> Result<Address, AppConfigError> {
    let string = value
        .as_str()
        .ok_or_else(|| AppConfig::validation("address must be a hex string"))?;
    let bytes = parse_hex_bytes(string)?;
    if bytes.len() != 20 {
        return Err(AppConfig::validation(format!(
            "address must be 20 bytes, got {} bytes",
            bytes.len()
        )));
    }

    let mut address = [0_u8; 20];
    address.copy_from_slice(&bytes);
    Ok(Address::new(address))
}

fn parse_u256_value(value: &Value) -> Result<U256, AppConfigError> {
    let string = parse_number_string(value)?;
    parse_u256_string(&string)
}

fn parse_u128_value(value: &Value) -> Result<u128, AppConfigError> {
    let string = parse_number_string(value)?;
    string
        .parse::<u128>()
        .map_err(|error| AppConfig::validation(format!("invalid u128 value {string}: {error}")))
}

fn parse_u32_value(value: &Value) -> Result<u32, AppConfigError> {
    let string = parse_number_string(value)?;
    string
        .parse::<u32>()
        .map_err(|error| AppConfig::validation(format!("invalid u32 value {string}: {error}")))
}

fn parse_i128_value(value: &Value) -> Result<i128, AppConfigError> {
    let string = parse_number_string(value)?;
    string
        .parse::<i128>()
        .map_err(|error| AppConfig::validation(format!("invalid i128 value {string}: {error}")))
}

fn parse_i32_value(value: &Value) -> Result<i32, AppConfigError> {
    let string = parse_number_string(value)?;
    string
        .parse::<i32>()
        .map_err(|error| AppConfig::validation(format!("invalid i32 value {string}: {error}")))
}

fn parse_number_string(value: &Value) -> Result<String, AppConfigError> {
    match value {
        Value::String(string) => Ok(string.clone()),
        Value::Number(number) => Ok(number.to_string()),
        _ => Err(AppConfig::validation(
            "numeric field must be a number or string",
        )),
    }
}

fn parse_u256_string(value: &str) -> Result<U256, AppConfigError> {
    if value.starts_with("0x") || value.starts_with("0X") {
        let hex = value[2..].as_bytes();
        let mut parsed = U256::ZERO;
        for byte in hex {
            let nibble = hex_value(*byte)
                .ok_or_else(|| AppConfig::validation(format!("invalid hex U256 value {value}")))?;
            parsed = parsed
                .checked_mul(U256::from(16u8))
                .and_then(|current| current.checked_add(U256::from(nibble)))
                .ok_or_else(|| {
                    AppConfig::validation(format!("U256 overflow while parsing {value}"))
                })?;
        }
        Ok(parsed)
    } else {
        let mut parsed = U256::ZERO;
        for byte in value.bytes() {
            if !byte.is_ascii_digit() {
                return Err(AppConfig::validation(format!(
                    "invalid decimal U256 value {value}"
                )));
            }
            parsed = parsed
                .checked_mul(U256::from(10u8))
                .and_then(|current| current.checked_add(U256::from(byte - b'0')))
                .ok_or_else(|| {
                    AppConfig::validation(format!("U256 overflow while parsing {value}"))
                })?;
        }
        Ok(parsed)
    }
}

fn parse_hex_bytes(value: &str) -> Result<Vec<u8>, AppConfigError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| AppConfig::validation(format!("hex value must start with 0x: {value}")))?;
    if hex.len() % 2 != 0 {
        return Err(AppConfig::validation(format!(
            "hex value must have even length: {value}"
        )));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut index = 0;
    while index < hex.len() {
        let high = hex_value(hex.as_bytes()[index])
            .ok_or_else(|| AppConfig::validation(format!("invalid hex value: {value}")))?;
        let low = hex_value(hex.as_bytes()[index + 1])
            .ok_or_else(|| AppConfig::validation(format!("invalid hex value: {value}")))?;
        bytes.push((high << 4) | low);
        index += 2;
    }

    Ok(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<String, AppConfigError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppConfig::validation(format!("missing {key}")))
}

fn required_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<&'a Vec<Value>, AppConfigError> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| AppConfig::validation(format!("missing {key}")))
}
