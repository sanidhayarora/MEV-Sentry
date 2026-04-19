# MEV-Sentry

MEV-Sentry is a real-time bundle feasibility analyzer for Ethereum orderflow.

It connects to a WebSocket-enabled Ethereum node, watches pending transactions, decodes supported Uniswap v3 router calls, simulates bounded adversarial bundles in memory, and emits risk reports for each supported victim transaction. The current implementation is optimized for deterministic execution analysis on a tight surface area rather than broad but approximate coverage.

## Core Capabilities

- real-time ingestion of `newPendingTransactions` and `newHeads`
- mempool normalization by `(sender, nonce)` with duplicate and replacement handling
- Uniswap v3 exact-input router decoding for:
  `exactInputSingle`
  `exactInput`
- deterministic Uniswap v3 route simulation across configured pools
- bounded search over:
  `Sandwich`
  `PressureToRevert`
- per-transaction metrics including:
  classification
  baseline output
  max feasible attacker profit
  max victim loss
  revert threshold
- live refresh of mutable pool state for configured pool addresses via `eth_call`

## Project Scope

MEV-Sentry is intentionally focused on a narrow, production-relevant slice:

- Ethereum JSON-RPC over WebSocket
- Uniswap v3 exact-input flows
- single-hop and multi-hop route analysis
- local-node operation with configured pool seeds

That focus keeps the analysis deterministic, testable, and easy to extend.

## Repository Layout

```text
configs/
  example.json          tracked local-node configuration example
docs/
  mev_sentry_architecture.md
  roadmap.md
scripts/
  run-local.ps1         convenience wrapper for local execution
  test.ps1              convenience wrapper for cargo test
src/
  analysis/             bounded search engine and simulator trait
  app/                  config loading and output formatting
  domain/               shared typed domain model
  ingest/               decoder, mempool, node adapter, pipeline, websocket runtime
  protocol/             Uniswap v3 simulation and live state loader
tests/
  app_config.rs
  pipeline_flow.rs
```

## Requirements

- Rust stable toolchain with `cargo`
- a WebSocket-enabled Ethereum node
- node support for:
  `eth_subscribe`
  `eth_call`
- support for `newPendingTransactions` with full transaction objects

Reth is the intended target, but any sufficiently compatible local node may work.

## Build

Debug build:

```text
cargo build
```

Release build:

```text
cargo build --release
```

## Test

Run the full suite:

```text
cargo test
```

Windows helper:

```text
.\scripts\test.ps1
```

The repo currently includes both module-level unit tests and integration tests under `tests/`.

## Run

Use the tracked example config as a starting point:

```text
cargo run -- configs/example.json
```

Release mode:

```text
cargo run --release -- configs/example.json
```

Windows helper:

```text
.\scripts\run-local.ps1
.\scripts\run-local.ps1 -Release
```

At startup the binary will:

1. load the JSON config
2. hydrate configured pools
3. connect to the node WebSocket endpoint
4. subscribe to pending transactions and heads
5. emit line-oriented runtime effects to stdout

Example output:

```text
tracking NewActive { tx_hash: "0x..." }
analysis tx=0x... class=Safe baseline_out=996 max_profit=0 max_loss=0
head block=12345678 active_txs=42
```

## Configuration

Configuration lives in JSON. A tracked example is provided at [configs/example.json](configs/example.json).

Top-level fields:

- `ws_endpoint`
- `routers`
- `search`
- `pools`

### `routers`

List of router addresses that should be treated as supported decode targets.

### `search`

- `min_attacker_input`
- `max_attacker_input`
- `attacker_input_step`
- `min_net_profit`

### `pools`

Each pool entry seeds the deterministic simulator.

- `address` is optional
- when `address` is present, the runtime will refresh `slot0` and `liquidity` from chain
- `initialized_ticks` are loaded from config

Large numeric fields such as `sqrt_price_x96` are best encoded as decimal strings.

## Architecture

Runtime path:

```text
WebSocket(JSON-RPC)
  -> node adapter
  -> pipeline event
  -> mempool tracker
  -> router decoder
  -> bundle search engine
  -> stdout effect
```

Analysis path:

```text
pending tx
  -> normalized victim route
  -> baseline simulation
  -> bounded adversarial search
  -> risk report
```

The full architecture write-up is in [docs/mev_sentry_architecture.md](docs/mev_sentry_architecture.md).

## Roadmap

Forward-looking work is tracked in [docs/roadmap.md](docs/roadmap.md). The README stays focused on the project as it exists today.
