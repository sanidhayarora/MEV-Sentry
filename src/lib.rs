pub mod analysis;
pub mod app;
pub mod domain;
pub mod ingest;
pub mod protocol;

pub(crate) mod decoder {
    pub use crate::ingest::decoder::*;
}

pub(crate) mod engine {
    pub use crate::analysis::engine::*;
}

pub(crate) mod mempool {
    pub use crate::ingest::mempool::*;
}

pub(crate) mod model {
    pub use crate::domain::model::*;
}

pub(crate) mod node {
    pub use crate::ingest::node::*;
}

pub(crate) mod pipeline {
    pub use crate::ingest::pipeline::*;
}

pub(crate) mod simulator {
    pub use crate::analysis::simulator::*;
}

pub(crate) mod uniswap_v3 {
    pub use crate::protocol::uniswap_v3::*;
}

pub use analysis::{
    BaselineSimulation, BundleSearchEngine, BundleSimulation, BundleSimulator, CandidateStatus,
    EngineError, SimulationError,
};
pub use app::{format_effect, AppConfig, AppConfigError};
pub use domain::{
    Address, AnalysisReport, BundleCandidate, CandidateMetrics, PendingTransaction, PoolKey,
    PoolKeyError, RiskClassification, RouteHop, SearchConfig, SearchConfigError, StrategyKind,
    SwapDirection, TxIdentity, VictimTransaction, VictimTransactionError,
};
pub use ingest::{
    AnalysisPipeline, DecodeError, MempoolTracker, NodeAdapterError, NodeDrop, NodeEventAdapter,
    NodeHead, NodeInclusion, NodeNotification, NodePendingTx, NodeWsRuntime, ObserveOutcome,
    PendingRecord, PendingState, PendingTxDecoder, PipelineEffect, PipelineEvent, RuntimeError,
    UniswapV3RouterDecoder,
};
pub use protocol::{
    ConfiguredPoolSeed, InitializedTick, StateError, UniswapV3Pool, UniswapV3SinglePoolSimulator,
    UniswapV3StateLoader,
};
