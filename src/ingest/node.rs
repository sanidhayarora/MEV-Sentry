use crate::model::{Address, PendingTransaction};
use crate::pipeline::PipelineEvent;

const ADDRESS_BYTES: usize = 20;
const TX_HASH_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodePendingTx {
    pub hash: String,
    pub from: String,
    pub nonce: String,
    pub to: Option<String>,
    pub input: String,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    pub gas_price: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeHead {
    pub number: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInclusion {
    pub tx_hash: String,
    pub block_number: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeDrop {
    pub tx_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeNotification {
    PendingTx(NodePendingTx),
    NewHead(NodeHead),
    Included(NodeInclusion),
    Dropped(NodeDrop),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeAdapterError {
    MissingField(&'static str),
    InvalidHex(&'static str),
    InvalidHexLength {
        field: &'static str,
        expected_bytes: usize,
        actual_bytes: usize,
    },
    QuantityOverflow(&'static str),
    InconsistentFeeFields,
}

#[derive(Default)]
pub struct NodeEventAdapter;

impl NodeEventAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn adapt(
        &self,
        notification: &NodeNotification,
    ) -> Result<PipelineEvent, NodeAdapterError> {
        match notification {
            NodeNotification::PendingTx(tx) => self.adapt_pending_tx(tx),
            NodeNotification::NewHead(head) => Ok(PipelineEvent::NewHead {
                block_number: parse_quantity_u64(&head.number, "number")?,
            }),
            NodeNotification::Included(inclusion) => Ok(PipelineEvent::Included {
                tx_hash: normalize_fixed_hex(&inclusion.tx_hash, "tx_hash", TX_HASH_BYTES)?,
                block_number: parse_quantity_u64(&inclusion.block_number, "block_number")?,
            }),
            NodeNotification::Dropped(drop) => Ok(PipelineEvent::Dropped {
                tx_hash: normalize_fixed_hex(&drop.tx_hash, "tx_hash", TX_HASH_BYTES)?,
            }),
        }
    }

    fn adapt_pending_tx(&self, tx: &NodePendingTx) -> Result<PipelineEvent, NodeAdapterError> {
        let tx_hash = normalize_fixed_hex(&tx.hash, "hash", TX_HASH_BYTES)?;
        let from = parse_address(&tx.from, "from")?;
        let nonce = parse_quantity_u64(&tx.nonce, "nonce")?;
        let to = tx
            .to
            .as_deref()
            .map(|value| parse_address(value, "to"))
            .transpose()?;
        let input = parse_bytes(&tx.input, "input")?;
        let max_fee_per_gas = parse_fee_field(
            tx.max_fee_per_gas.as_deref(),
            tx.gas_price.as_deref(),
            "max_fee_per_gas",
        )?;
        let max_priority_fee_per_gas = parse_priority_fee_field(
            tx.max_priority_fee_per_gas.as_deref(),
            tx.gas_price.as_deref(),
            max_fee_per_gas,
        )?;

        if max_priority_fee_per_gas > max_fee_per_gas {
            return Err(NodeAdapterError::InconsistentFeeFields);
        }

        Ok(PipelineEvent::Observed(PendingTransaction {
            tx_hash,
            from,
            nonce,
            to,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            input,
        }))
    }
}

fn parse_fee_field(
    preferred: Option<&str>,
    legacy_fallback: Option<&str>,
    field: &'static str,
) -> Result<u128, NodeAdapterError> {
    let value = preferred
        .or(legacy_fallback)
        .ok_or(NodeAdapterError::MissingField(field))?;
    parse_quantity_u128(value, field)
}

fn parse_priority_fee_field(
    preferred: Option<&str>,
    legacy_fallback: Option<&str>,
    max_fee_per_gas: u128,
) -> Result<u128, NodeAdapterError> {
    match preferred.or(legacy_fallback) {
        Some(value) => parse_quantity_u128(value, "max_priority_fee_per_gas"),
        None => Ok(max_fee_per_gas),
    }
}

fn parse_address(value: &str, field: &'static str) -> Result<Address, NodeAdapterError> {
    let bytes = parse_fixed_hex(value, field, ADDRESS_BYTES)?;
    let mut address = [0_u8; ADDRESS_BYTES];
    address.copy_from_slice(&bytes);
    Ok(Address::new(address))
}

fn normalize_fixed_hex(
    value: &str,
    field: &'static str,
    expected_bytes: usize,
) -> Result<String, NodeAdapterError> {
    let bytes = parse_fixed_hex(value, field, expected_bytes)?;
    let mut normalized = String::with_capacity(2 + bytes.len() * 2);
    normalized.push_str("0x");

    for byte in bytes {
        normalized.push(nibble_to_hex(byte >> 4));
        normalized.push(nibble_to_hex(byte & 0x0f));
    }

    Ok(normalized)
}

fn parse_fixed_hex(
    value: &str,
    field: &'static str,
    expected_bytes: usize,
) -> Result<Vec<u8>, NodeAdapterError> {
    let bytes = parse_bytes(value, field)?;
    if bytes.len() != expected_bytes {
        return Err(NodeAdapterError::InvalidHexLength {
            field,
            expected_bytes,
            actual_bytes: bytes.len(),
        });
    }

    Ok(bytes)
}

fn parse_bytes(value: &str, field: &'static str) -> Result<Vec<u8>, NodeAdapterError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or(NodeAdapterError::InvalidHex(field))?;

    if hex.is_empty() {
        return Ok(Vec::new());
    }
    if hex.len() % 2 != 0 {
        return Err(NodeAdapterError::InvalidHex(field));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut index = 0;
    while index < hex.len() {
        let high = hex_value(hex.as_bytes()[index]).ok_or(NodeAdapterError::InvalidHex(field))?;
        let low =
            hex_value(hex.as_bytes()[index + 1]).ok_or(NodeAdapterError::InvalidHex(field))?;
        bytes.push((high << 4) | low);
        index += 2;
    }

    Ok(bytes)
}

fn parse_quantity_u64(value: &str, field: &'static str) -> Result<u64, NodeAdapterError> {
    parse_quantity_u128(value, field)?
        .try_into()
        .map_err(|_| NodeAdapterError::QuantityOverflow(field))
}

fn parse_quantity_u128(value: &str, field: &'static str) -> Result<u128, NodeAdapterError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or(NodeAdapterError::InvalidHex(field))?;

    if hex.is_empty() {
        return Ok(0);
    }

    let mut parsed = 0_u128;
    for byte in hex.bytes() {
        let nibble = hex_value(byte).ok_or(NodeAdapterError::InvalidHex(field))? as u128;
        parsed = parsed
            .checked_mul(16)
            .and_then(|value| value.checked_add(nibble))
            .ok_or(NodeAdapterError::QuantityOverflow(field))?;
    }

    Ok(parsed)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be less than 16"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_tx() -> NodePendingTx {
        NodePendingTx {
            hash: format!("0x{}", "ab".repeat(32)),
            from: format!("0x{}", "11".repeat(20)),
            nonce: "0x7".to_string(),
            to: Some(format!("0x{}", "22".repeat(20))),
            input: "0xdeadbeef".to_string(),
            max_fee_per_gas: Some("0x64".to_string()),
            max_priority_fee_per_gas: Some("0x2".to_string()),
            gas_price: None,
        }
    }

    #[test]
    fn adapts_eip1559_pending_transaction() {
        let adapter = NodeEventAdapter::new();
        let event = adapter
            .adapt(&NodeNotification::PendingTx(pending_tx()))
            .unwrap();

        match event {
            PipelineEvent::Observed(tx) => {
                assert_eq!(tx.tx_hash, format!("0x{}", "ab".repeat(32)));
                assert_eq!(tx.from, Address::new([0x11; 20]));
                assert_eq!(tx.nonce, 7);
                assert_eq!(tx.to, Some(Address::new([0x22; 20])));
                assert_eq!(tx.max_fee_per_gas, 100);
                assert_eq!(tx.max_priority_fee_per_gas, 2);
                assert_eq!(tx.input, vec![0xde, 0xad, 0xbe, 0xef]);
            }
            other => panic!("expected observed event, got {other:?}"),
        }
    }

    #[test]
    fn adapts_legacy_gas_price_into_fee_fields() {
        let adapter = NodeEventAdapter::new();
        let mut tx = pending_tx();
        tx.max_fee_per_gas = None;
        tx.max_priority_fee_per_gas = None;
        tx.gas_price = Some("0x2a".to_string());

        let event = adapter.adapt(&NodeNotification::PendingTx(tx)).unwrap();

        match event {
            PipelineEvent::Observed(tx) => {
                assert_eq!(tx.max_fee_per_gas, 42);
                assert_eq!(tx.max_priority_fee_per_gas, 42);
            }
            other => panic!("expected observed event, got {other:?}"),
        }
    }

    #[test]
    fn adapts_head_and_terminal_notifications() {
        let adapter = NodeEventAdapter::new();

        let head = adapter
            .adapt(&NodeNotification::NewHead(NodeHead {
                number: "0xa".to_string(),
            }))
            .unwrap();
        let included = adapter
            .adapt(&NodeNotification::Included(NodeInclusion {
                tx_hash: format!("0x{}", "cd".repeat(32)),
                block_number: "0xb".to_string(),
            }))
            .unwrap();
        let dropped = adapter
            .adapt(&NodeNotification::Dropped(NodeDrop {
                tx_hash: format!("0x{}", "ef".repeat(32)),
            }))
            .unwrap();

        assert_eq!(head, PipelineEvent::NewHead { block_number: 10 });
        assert_eq!(
            included,
            PipelineEvent::Included {
                tx_hash: format!("0x{}", "cd".repeat(32)),
                block_number: 11,
            }
        );
        assert_eq!(
            dropped,
            PipelineEvent::Dropped {
                tx_hash: format!("0x{}", "ef".repeat(32)),
            }
        );
    }

    #[test]
    fn rejects_invalid_hex_payloads() {
        let adapter = NodeEventAdapter::new();
        let mut tx = pending_tx();
        tx.from = "0x1234".to_string();

        let error = adapter.adapt(&NodeNotification::PendingTx(tx)).unwrap_err();
        assert_eq!(
            error,
            NodeAdapterError::InvalidHexLength {
                field: "from",
                expected_bytes: 20,
                actual_bytes: 2,
            }
        );
    }

    #[test]
    fn rejects_priority_fee_above_max_fee() {
        let adapter = NodeEventAdapter::new();
        let mut tx = pending_tx();
        tx.max_fee_per_gas = Some("0x2".to_string());
        tx.max_priority_fee_per_gas = Some("0x3".to_string());

        let error = adapter.adapt(&NodeNotification::PendingTx(tx)).unwrap_err();
        assert_eq!(error, NodeAdapterError::InconsistentFeeFields);
    }

    #[test]
    fn rejects_missing_fee_fields_without_legacy_fallback() {
        let adapter = NodeEventAdapter::new();
        let mut tx = pending_tx();
        tx.max_fee_per_gas = None;
        tx.max_priority_fee_per_gas = None;
        tx.gas_price = None;

        let error = adapter.adapt(&NodeNotification::PendingTx(tx)).unwrap_err();
        assert_eq!(error, NodeAdapterError::MissingField("max_fee_per_gas"));
    }
}
