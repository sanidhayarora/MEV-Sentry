use std::env;
use std::process::ExitCode;

use mev_sentry::{
    format_effect, AnalysisPipeline, AppConfig, BundleSearchEngine, NodeWsRuntime,
    UniswapV3RouterDecoder, UniswapV3SinglePoolSimulator, UniswapV3StateLoader,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let config_path = env::args()
        .nth(1)
        .ok_or_else(|| "usage: cargo run -- <config.json>".to_string())?;
    let config = AppConfig::from_path(&config_path).map_err(|error| error.to_string())?;

    let mut state_loader = if config.has_live_pool_seeds() {
        Some(
            UniswapV3StateLoader::connect(&config.ws_endpoint)
                .map_err(|error| format!("state loader connect failed: {error:?}"))?,
        )
    } else {
        None
    };

    let initial_pools = if let Some(loader) = state_loader.as_mut() {
        loader
            .load_pools(&config.pool_seeds, None)
            .map_err(|error| format!("initial live pool load failed: {error:?}"))?
    } else {
        config
            .pool_seeds
            .iter()
            .map(|seed| seed.snapshot.clone())
            .collect()
    };

    let simulator = UniswapV3SinglePoolSimulator::new(initial_pools)
        .map_err(|error| format!("invalid pool configuration: {error:?}"))?;
    let simulator_handle = simulator.clone();
    let engine = BundleSearchEngine::new(simulator, config.search)
        .map_err(|error| format!("invalid search configuration: {error:?}"))?;
    let pipeline =
        AnalysisPipeline::new(UniswapV3RouterDecoder::new(config.routers.clone()), engine);
    let mut runtime = NodeWsRuntime::connect(&config.ws_endpoint, pipeline)
        .map_err(|error| format!("runtime connect failed: {error:?}"))?;

    eprintln!(
        "mev-sentry connected to {} with {} configured pools ({} live)",
        config.ws_endpoint,
        config.pool_count(),
        config.live_pool_count()
    );

    loop {
        let effects = runtime
            .process_next_message()
            .map_err(|error| format!("runtime failed: {error:?}"))?;
        for effect in effects {
            println!("{}", format_effect(&effect));
            if let (Some(loader), mev_sentry::PipelineEffect::HeadAdvanced { block_number, .. }) =
                (state_loader.as_mut(), &effect)
            {
                if let Err(error) = loader.refresh_simulator(
                    &config.pool_seeds,
                    &simulator_handle,
                    Some(*block_number),
                ) {
                    eprintln!("warning: pool refresh failed at block {block_number}: {error:?}");
                }
            }
        }
    }
}
