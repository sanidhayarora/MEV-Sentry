use crate::PipelineEffect;

pub fn format_effect(effect: &PipelineEffect) -> String {
    match effect {
        PipelineEffect::TrackingUpdated(outcome) => format!("tracking {outcome:?}"),
        PipelineEffect::DecodeFailed { tx_hash, error } => {
            format!("decode_failed tx={tx_hash} error={error:?}")
        }
        PipelineEffect::Analyzed(report) => format!(
            "analysis tx={} class={:?} baseline_out={} max_profit={} max_loss={}",
            report.tx_hash,
            report.classification,
            report.baseline_output,
            report.max_feasible_attacker_profit,
            report.max_victim_loss
        ),
        PipelineEffect::Included {
            tx_hash,
            block_number,
        } => format!("included tx={tx_hash} block={block_number}"),
        PipelineEffect::Dropped { tx_hash } => format!("dropped tx={tx_hash}"),
        PipelineEffect::HeadAdvanced {
            block_number,
            active_transactions,
        } => format!("head block={block_number} active_txs={active_transactions}"),
    }
}
