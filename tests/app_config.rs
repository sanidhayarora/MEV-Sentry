use std::path::PathBuf;

use mev_sentry::{format_effect, AppConfig, PipelineEffect};

fn example_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("configs")
        .join("example.json")
}

#[test]
fn parses_tracked_example_config() {
    let config = AppConfig::from_path(example_config_path()).expect("parse example config");

    assert_eq!(config.ws_endpoint, "ws://127.0.0.1:8546");
    assert_eq!(config.routers.len(), 1);
    assert_eq!(config.pool_count(), 1);
    assert_eq!(config.live_pool_count(), 1);
    assert_eq!(config.search.min_attacker_input, 1_000);
    assert_eq!(config.pool_seeds[0].snapshot.current_tick, 0);
    assert_eq!(config.pool_seeds[0].snapshot.initialized_ticks.len(), 2);
}

#[test]
fn rejects_empty_router_list() {
    let mut config = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(example_config_path()).expect("read config"),
    )
    .expect("valid json");
    config["routers"] = serde_json::json!([]);

    let error = AppConfig::parse(&config).expect_err("empty routers should fail");
    assert!(error.to_string().contains("routers must not be empty"));
}

#[test]
fn formats_head_effect_compactly() {
    let effect = PipelineEffect::HeadAdvanced {
        block_number: 10,
        active_transactions: 3,
    };

    assert_eq!(format_effect(&effect), "head block=10 active_txs=3");
}
