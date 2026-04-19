# MEV-Sentry Architecture and Implementation Plan

## Overview

MEV-Sentry is a real-time bundle feasibility analyzer. For each relevant pending transaction, the system synthesizes a bounded family of attacker bundles, simulates them against a deterministic market-state interface, and emits a compact report:

- can the transaction be exploited
- how much capital is required
- how profitable is the best bundle
- how close is the victim to reverting
- how confident is the result

The product is analysis-only. It does not send bundles, manage keys, or participate in searcher infrastructure.

## Phase 1 Goal

Land a compact, production-grade Rust core that is small enough to reason about but strong enough to support the real system later.

Phase 1 intentionally excludes:

- persistence and dashboards
- private orderflow modeling
- exact-output swaps and broader router coverage
- automatic pool topology discovery beyond configured seeds

Those layers come after the core search/reporting interfaces are stable.

## Functional Requirements

- represent a pending victim transaction in a strict internal model
- represent bounded attacker bundle templates
- run baseline and candidate simulations through a deterministic trait boundary
- search candidate attacker sizes across a configured range
- compute attacker capital, net profit, victim loss, and revert threshold
- classify results as `Safe`, `Vulnerable`, or `Inconclusive`
- surface unsupported or stale-state outcomes explicitly
- support exact-input Uniswap v3 swaps across initialized ticks and ordered routes
- track pending transaction lifecycle by `(sender, nonce)` with deterministic replacement handling
- expose an event-driven pipeline that binds observations to decode and analysis
- adapt Reth-style node payloads into typed pipeline events without transport coupling
- sustain a JSON-RPC WebSocket runtime for pending-tx and head subscriptions
- expose a runnable executable that assembles the stack from explicit config
- live-load and refresh mutable pool state for configured Uniswap v3 pools

## Non-Functional Requirements

- memory-safe implementation
- deterministic outputs for a fixed simulator
- small, inspectable file structure
- explicit configuration validation
- edge-case unit tests before runtime integration

## Compact Runtime Modules

Phase 1 uses a single Rust crate with ten modules:

1. `model`
   Domain types for pending transactions, victim transactions, canonical pool keys, reports, and configuration.

2. `simulator`
   Trait boundary for deterministic market simulation, plus simulation result types.

3. `decoder`
   Narrow calldata decoding for real pending transactions. Phase 1 supports Uniswap v3 `exactInputSingle` and `exactInput`.

4. `mempool`
   Compact in-memory normalization for duplicate observations, sender/nonce replacement, inclusion, and drops.

5. `engine`
   Candidate generation, bounded search, scoring, classification, and report assembly.

6. `pipeline`
   Thin runtime orchestration layer for observation, inclusion, drop, and head events.

7. `node`
   Dependency-light adapter that parses node-originated hex payloads and emits `PipelineEvent`.

8. `runtime`
   Blocking JSON-RPC WebSocket runtime that subscribes to local node streams and feeds the pipeline.

9. `state`
   JSON-RPC pool-state loader that hydrates and refreshes `slot0` and `liquidity` for configured pools.

10. `uniswap_v3`
   Exact-input Uniswap v3 simulation over initialized ticks, liquidity-net updates, and ordered routes.

This keeps the project manageable while preserving future extraction points for a larger workspace.

## Core Data Model

### Victim transaction

```text
VictimTransaction {
  tx_hash,
  route,
  amount_in,
  min_amount_out
}
```

Where `route` is an ordered list of `(pool, direction)` hops validated for token continuity.

### Pending transaction

```text
PendingTransaction {
  tx_hash,
  from,
  nonce,
  to,
  max_fee_per_gas,
  max_priority_fee_per_gas,
  input
}
```

### Candidate bundle

```text
CandidateBundle {
  strategy,
  attacker_input
}
```

Supported strategies in Phase 1:

- `Sandwich`
- `PressureToRevert`

### Simulation boundary

The engine does not know Uniswap internals directly. Real pending transactions first pass through the decoder:

```text
PendingTransaction -> VictimTransaction
```

Then analysis depends on:

```text
simulate_baseline(victim) -> BaselineSimulation
simulate_candidate(victim, candidate) -> BundleSimulation
```

That allows us to keep the engine generic while plugging in exact Uniswap v3 state transitions immediately for the narrowest rigorous slice.

The event pipeline now composes the runtime path as:

```text
PipelineEvent::Observed(tx)
  -> MempoolTracker::observe(tx)
  -> decoder.decode(tx)
  -> engine.analyze(victim)
  -> PipelineEffect::Analyzed(report)
```

Node-originated messages now enter through a narrow adapter layer:

```text
NodeNotification
  -> NodeEventAdapter::adapt(...)
  -> PipelineEvent
  -> AnalysisPipeline::handle_event(...)
```

The runtime layer now owns the outer subscription loop:

```text
WebSocket(JSON-RPC)
  -> subscription ack parsing
  -> notification parsing
  -> NodeNotification
  -> NodeEventAdapter
  -> AnalysisPipeline
```

The binary entrypoint now owns stack construction:

```text
config file
  -> routers + search + pool seeds
  -> state hydration
  -> decoder + simulator + engine + pipeline + runtime
  -> stdout effects
```

Mutable pool state now refreshes on head advance:

```text
newHeads
  -> AnalysisPipeline::handle_event(NewHead)
  -> UniswapV3StateLoader::refresh_simulator(...)
  -> subsequent analyses use the refreshed simulator snapshot
```

## Implemented Protocol Slice

The first concrete protocol implementation is:

- two decoded router call shapes: Uniswap v3 `exactInputSingle` and `exactInput`
- ordered exact-input routes over one or more pools
- multiple initialized liquidity ranges per pool
- exact-input swaps only
- baseline execution, sandwich bundles, and revert-pressure probes

This slice is exact for the supported state space and returns `Unsupported` if a swap exhausts the initialized liquidity boundaries we have mirrored.

## Bundle Search Algorithm

Given a victim transaction `V`:

1. Run baseline simulation.
2. Generate candidate bundles for each strategy and attacker input in the configured range.
3. Simulate each candidate.
4. Track:
   - best profitable candidate
   - minimum capital among profitable candidates
   - earliest revert threshold
   - simulation coverage and errors
5. Emit a final report.

### Search Space

For attacker input `a` and strategy `s`:

```text
CandidateBundle(s, a)
```

Phase 1 uses bounded linear exploration:

```text
a in [min_attacker_input, max_attacker_input] step attacker_input_step
```

The search engine is deterministic and easy to verify. Phase 2 can replace the search policy with breakpoint-aware refinement for exact v3 math.

## Report Semantics

### `Vulnerable`

- at least one candidate bundle was evaluated successfully
- at least one candidate has positive net profit after gas

### `Safe`

- at least one candidate bundle was evaluated successfully
- none are profitable

### `Inconclusive`

- baseline simulation fails
- or no candidate bundle can be evaluated successfully

## State Transitions

### Analysis lifecycle

| From | To | Trigger |
|------|----|---------|
| `Queued` | `RunningBaseline` | analysis starts |
| `RunningBaseline` | `RunningCandidates` | baseline simulation succeeds |
| `RunningBaseline` | `Inconclusive` | baseline simulation fails |
| `RunningCandidates` | `Completed` | at least one candidate evaluated |
| `RunningCandidates` | `Inconclusive` | all candidate simulations fail |

### Candidate lifecycle

| From | To | Trigger |
|------|----|---------|
| `Generated` | `Simulated` | candidate simulation succeeds |
| `Generated` | `Rejected` | simulator returns unsupported/stale/invalid |
| `Simulated` | `Feasible` | net profit is positive |
| `Simulated` | `Unprofitable` | net profit is zero or negative |
| `Simulated` | `VictimReverts` | victim no longer clears constraints |

## Implementation Order

1. `model`
   Freeze the report and configuration types.

2. `simulator`
   Define the deterministic interface that the engine consumes.

3. `engine`
   Implement bounded search, scoring, and classification.

4. tests
   Verify:
   - invalid configuration
   - best-candidate selection
   - minimum capital calculation
   - revert-threshold tracking
   - mempool replacement and terminal lifecycle transitions
   - observation pipeline behavior for duplicate, replacement, inclusion, drop, and head events
   - node adapter behavior for fee parsing, hex validation, and terminal event conversion
   - runtime behavior for subscription request building, ack parsing, and notification decoding
   - state loader behavior for request building, response parsing, and snapshot overlay
   - binary config parsing and effect formatting
   - safe vs vulnerable vs inconclusive outcomes

5. next phase
   Expand live pool-state mirroring beyond `slot0` and `liquidity`.

## Repository Layout

```text
.
|-- Cargo.toml
|-- docs/
|   `-- mev_sentry_architecture.md
|-- src/
|   |-- decoder.rs
|   |-- engine.rs
|   |-- lib.rs
|   |-- main.rs
|   |-- mempool.rs
|   |-- model.rs
|   |-- node.rs
|   |-- pipeline.rs
|   |-- runtime.rs
|   |-- state.rs
|   |-- simulator.rs
|   `-- uniswap_v3.rs
`-- tasks/
    `-- lessons.md
```

## Near-Term Follow-Up

After this increment, the next design and implementation step is:

- widen live state mirroring to support less manually seeded pool topology
- extend router support carefully beyond exact-input Uniswap v3 paths

That sequencing keeps the project rigorous without overbuilding too early.
