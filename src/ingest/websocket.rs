use std::io;
use std::net::TcpStream;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::decoder::PendingTxDecoder;
use crate::node::{NodeAdapterError, NodeEventAdapter, NodeHead, NodeNotification, NodePendingTx};
use crate::pipeline::{AnalysisPipeline, PipelineEffect, PipelineEvent};
use crate::simulator::BundleSimulator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubscriptionKind {
    PendingTransactions,
    NewHeads,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SubscriptionRegistry {
    pending_transactions: Option<String>,
    new_heads: Option<String>,
}

impl SubscriptionRegistry {
    fn register(&mut self, kind: SubscriptionKind, id: String) {
        match kind {
            SubscriptionKind::PendingTransactions => self.pending_transactions = Some(id),
            SubscriptionKind::NewHeads => self.new_heads = Some(id),
        }
    }

    fn resolve(&self, id: &str) -> Option<SubscriptionKind> {
        if self.pending_transactions.as_deref() == Some(id) {
            Some(SubscriptionKind::PendingTransactions)
        } else if self.new_heads.as_deref() == Some(id) {
            Some(SubscriptionKind::NewHeads)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    Transport(tungstenite::Error),
    Json(serde_json::Error),
    Io(io::Error),
    Protocol(&'static str),
    NodeAdapter(NodeAdapterError),
}

impl From<tungstenite::Error> for RuntimeError {
    fn from(value: tungstenite::Error) -> Self {
        Self::Transport(value)
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<io::Error> for RuntimeError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<NodeAdapterError> for RuntimeError {
    fn from(value: NodeAdapterError) -> Self {
        Self::NodeAdapter(value)
    }
}

pub struct NodeWsRuntime<D, S> {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    adapter: NodeEventAdapter,
    pipeline: AnalysisPipeline<D, S>,
    subscriptions: SubscriptionRegistry,
    next_request_id: u64,
}

impl<D, S> NodeWsRuntime<D, S>
where
    D: PendingTxDecoder,
    S: BundleSimulator,
{
    pub fn connect(endpoint: &str, pipeline: AnalysisPipeline<D, S>) -> Result<Self, RuntimeError> {
        let (socket, _) = connect(endpoint)?;
        let mut runtime = Self {
            socket,
            adapter: NodeEventAdapter::new(),
            pipeline,
            subscriptions: SubscriptionRegistry::default(),
            next_request_id: 1,
        };
        runtime.subscribe_default_streams()?;
        Ok(runtime)
    }

    pub fn pipeline(&self) -> &AnalysisPipeline<D, S> {
        &self.pipeline
    }

    pub fn pipeline_mut(&mut self) -> &mut AnalysisPipeline<D, S> {
        &mut self.pipeline
    }

    pub fn process_next_message(&mut self) -> Result<Vec<PipelineEffect>, RuntimeError> {
        loop {
            match self.socket.read()? {
                Message::Text(payload) => {
                    if let Some(event) =
                        parse_incoming_message(&payload, &self.subscriptions, &self.adapter)?
                    {
                        return Ok(self.pipeline.handle_event(event));
                    }
                }
                Message::Binary(payload) => {
                    let text = String::from_utf8(payload).map_err(|_| {
                        RuntimeError::Protocol("binary websocket frame must be utf-8")
                    })?;
                    if let Some(event) =
                        parse_incoming_message(&text, &self.subscriptions, &self.adapter)?
                    {
                        return Ok(self.pipeline.handle_event(event));
                    }
                }
                Message::Ping(payload) => self.socket.send(Message::Pong(payload))?,
                Message::Pong(_) => {}
                Message::Frame(_) => {}
                Message::Close(_) => return Err(RuntimeError::Protocol("websocket closed")),
            }
        }
    }

    fn subscribe_default_streams(&mut self) -> Result<(), RuntimeError> {
        let pending_id = self.next_request_id();
        self.socket.send(Message::Text(
            build_subscribe_request(pending_id, SubscriptionKind::PendingTransactions).to_string(),
        ))?;
        let pending_ack = read_subscription_ack(&mut self.socket, pending_id)?;
        self.subscriptions
            .register(SubscriptionKind::PendingTransactions, pending_ack);

        let head_id = self.next_request_id();
        self.socket.send(Message::Text(
            build_subscribe_request(head_id, SubscriptionKind::NewHeads).to_string(),
        ))?;
        let head_ack = read_subscription_ack(&mut self.socket, head_id)?;
        self.subscriptions
            .register(SubscriptionKind::NewHeads, head_ack);

        Ok(())
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        id
    }
}

fn build_subscribe_request(id: u64, kind: SubscriptionKind) -> Value {
    match kind {
        SubscriptionKind::PendingTransactions => json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "eth_subscribe",
            "params": ["newPendingTransactions", true],
        }),
        SubscriptionKind::NewHeads => json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "eth_subscribe",
            "params": ["newHeads"],
        }),
    }
}

fn read_subscription_ack(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    expected_id: u64,
) -> Result<String, RuntimeError> {
    loop {
        match socket.read()? {
            Message::Text(payload) => {
                if let Some(id) = parse_subscription_ack(&payload, expected_id)? {
                    return Ok(id);
                }
            }
            Message::Binary(payload) => {
                let text = String::from_utf8(payload)
                    .map_err(|_| RuntimeError::Protocol("binary websocket frame must be utf-8"))?;
                if let Some(id) = parse_subscription_ack(&text, expected_id)? {
                    return Ok(id);
                }
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload))?,
            Message::Pong(_) => {}
            Message::Frame(_) => {}
            Message::Close(_) => return Err(RuntimeError::Protocol("websocket closed")),
        }
    }
}

fn parse_subscription_ack(payload: &str, expected_id: u64) -> Result<Option<String>, RuntimeError> {
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
        .ok_or(RuntimeError::Protocol("subscription ack missing result"))?;
    Ok(Some(result.to_string()))
}

fn parse_incoming_message(
    payload: &str,
    subscriptions: &SubscriptionRegistry,
    adapter: &NodeEventAdapter,
) -> Result<Option<PipelineEvent>, RuntimeError> {
    let value: Value = serde_json::from_str(payload)?;
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    if method != "eth_subscription" {
        return Ok(None);
    }

    let params = value
        .get("params")
        .and_then(Value::as_object)
        .ok_or(RuntimeError::Protocol(
            "subscription notification missing params",
        ))?;
    let subscription_id =
        params
            .get("subscription")
            .and_then(Value::as_str)
            .ok_or(RuntimeError::Protocol(
                "subscription notification missing id",
            ))?;
    let result = params.get("result").ok_or(RuntimeError::Protocol(
        "subscription notification missing result",
    ))?;

    let Some(kind) = subscriptions.resolve(subscription_id) else {
        return Ok(None);
    };

    let notification = match kind {
        SubscriptionKind::PendingTransactions => {
            NodeNotification::PendingTx(parse_pending_tx_notification(result)?)
        }
        SubscriptionKind::NewHeads => {
            NodeNotification::NewHead(parse_new_head_notification(result)?)
        }
    };

    Ok(Some(adapter.adapt(&notification)?))
}

fn parse_pending_tx_notification(value: &Value) -> Result<NodePendingTx, RuntimeError> {
    let object = value.as_object().ok_or(RuntimeError::Protocol(
        "pending tx notification must be an object",
    ))?;

    Ok(NodePendingTx {
        hash: required_string_field(object, "hash")?,
        from: required_string_field(object, "from")?,
        nonce: required_string_field(object, "nonce")?,
        to: optional_string_field(object, "to")?,
        input: required_string_field(object, "input")?,
        max_fee_per_gas: optional_string_field(object, "maxFeePerGas")?,
        max_priority_fee_per_gas: optional_string_field(object, "maxPriorityFeePerGas")?,
        gas_price: optional_string_field(object, "gasPrice")?,
    })
}

fn parse_new_head_notification(value: &Value) -> Result<NodeHead, RuntimeError> {
    let object = value.as_object().ok_or(RuntimeError::Protocol(
        "newHeads notification must be an object",
    ))?;

    Ok(NodeHead {
        number: required_string_field(object, "number")?,
    })
}

fn required_string_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<String, RuntimeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(RuntimeError::Protocol("missing required string field"))
}

fn optional_string_field(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, RuntimeError> {
    match object.get(field) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(RuntimeError::Protocol("expected string field")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::UniswapV3RouterDecoder;
    use crate::engine::BundleSearchEngine;
    use crate::model::{Address, SearchConfig};
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

    fn registry() -> SubscriptionRegistry {
        SubscriptionRegistry {
            pending_transactions: Some("0xsub-pending".to_string()),
            new_heads: Some("0xsub-head".to_string()),
        }
    }

    #[test]
    fn builds_expected_subscription_requests() {
        let pending = build_subscribe_request(1, SubscriptionKind::PendingTransactions);
        let heads = build_subscribe_request(2, SubscriptionKind::NewHeads);

        assert_eq!(pending["method"], "eth_subscribe");
        assert_eq!(pending["params"][0], "newPendingTransactions");
        assert_eq!(pending["params"][1], true);
        assert_eq!(heads["params"][0], "newHeads");
    }

    #[test]
    fn parses_subscription_ack() {
        let payload = r#"{"jsonrpc":"2.0","id":1,"result":"0xsub-pending"}"#;
        let ack = parse_subscription_ack(payload, 1).unwrap();

        assert_eq!(ack, Some("0xsub-pending".to_string()));
    }

    #[test]
    fn parses_pending_tx_notification_into_pipeline_event() {
        let payload = format!(
            r#"{{
                "jsonrpc":"2.0",
                "method":"eth_subscription",
                "params":{{
                    "subscription":"0xsub-pending",
                    "result":{{
                        "hash":"0x{hash}",
                        "from":"0x{from}",
                        "nonce":"0x7",
                        "to":"0x{to}",
                        "input":"0xdeadbeef",
                        "maxFeePerGas":"0x64",
                        "maxPriorityFeePerGas":"0x2"
                    }}
                }}
            }}"#,
            hash = "ab".repeat(32),
            from = "11".repeat(20),
            to = "22".repeat(20),
        );

        let event = parse_incoming_message(&payload, &registry(), &NodeEventAdapter::new())
            .unwrap()
            .expect("pipeline event");

        match event {
            PipelineEvent::Observed(tx) => {
                assert_eq!(tx.tx_hash, format!("0x{}", "ab".repeat(32)));
                assert_eq!(tx.from, Address::new([0x11; 20]));
                assert_eq!(tx.nonce, 7);
                assert_eq!(tx.max_fee_per_gas, 100);
            }
            other => panic!("expected observed event, got {other:?}"),
        }
    }

    #[test]
    fn parses_new_head_notification_into_pipeline_event() {
        let payload = r#"{
            "jsonrpc":"2.0",
            "method":"eth_subscription",
            "params":{
                "subscription":"0xsub-head",
                "result":{"number":"0xa"}
            }
        }"#;

        let event = parse_incoming_message(payload, &registry(), &NodeEventAdapter::new())
            .unwrap()
            .expect("pipeline event");

        assert_eq!(event, PipelineEvent::NewHead { block_number: 10 });
    }

    #[test]
    fn ignores_unknown_subscription_ids() {
        let payload = r#"{
            "jsonrpc":"2.0",
            "method":"eth_subscription",
            "params":{
                "subscription":"0xother",
                "result":{"number":"0xa"}
            }
        }"#;

        assert_eq!(
            parse_incoming_message(payload, &registry(), &NodeEventAdapter::new()).unwrap(),
            None
        );
    }

    #[test]
    fn parse_errors_on_invalid_pending_payload_shape() {
        let payload = r#"{
            "jsonrpc":"2.0",
            "method":"eth_subscription",
            "params":{
                "subscription":"0xsub-pending",
                "result":"0xdeadbeef"
            }
        }"#;

        let error =
            parse_incoming_message(payload, &registry(), &NodeEventAdapter::new()).unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Protocol("pending tx notification must be an object")
        ));
    }

    #[test]
    fn runtime_can_be_built_around_pipeline() {
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
        let pipeline = AnalysisPipeline::new(UniswapV3RouterDecoder::new([router()]), engine);

        let latest = pipeline.latest_block();

        assert_eq!(latest, None);
    }
}
