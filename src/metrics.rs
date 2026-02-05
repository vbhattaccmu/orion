use prometheus::{Gauge, Histogram, HistogramOpts, HistogramVec, IntCounter, IntGauge, Registry};
use std::sync::Arc;

/// Metrics for the Orion-Reth integration.
/// Exposed via a Prometheus HTTP endpoint for Grafana dashboards.
pub struct OrionMetrics {
    pub registry: Registry,

    // ── Consensus metrics ──
    /// Total number of DAG blocks created by this node.
    pub dag_blocks_created: IntCounter,
    /// Total number of DAG blocks committed (sub-DAGs).
    pub dag_subdags_committed: IntCounter,
    /// Current DAG round number.
    pub dag_current_round: IntGauge,
    /// Current committed height.
    pub dag_committed_height: IntGauge,
    /// Number of transactions ordered through the DAG.
    pub dag_transactions_ordered: IntCounter,

    // ── BFT checkpoint metrics ──
    /// Total BFT checkpoints finalized.
    pub bft_checkpoints_finalized: IntCounter,
    /// Last finalized checkpoint height.
    pub bft_last_finalized_height: IntGauge,

    // ── Execution / Engine API metrics ──
    /// Total Reth blocks produced via Engine API.
    pub reth_blocks_produced: IntCounter,
    /// Total transactions submitted to Reth's pool.
    pub reth_txs_submitted: IntCounter,
    /// Latest Reth block number.
    pub reth_block_number: IntGauge,
    /// Histogram of Engine API call latencies (seconds), labeled by method.
    pub engine_api_latency: HistogramVec,
    /// Histogram of full block build cycle latency (seconds).
    pub block_build_latency: Histogram,

    // ── TPS metrics ──
    /// Current measured TPS (updated periodically).
    pub current_tps: Gauge,
    /// Peak TPS observed.
    pub peak_tps: Gauge,
    /// Total elapsed time in the benchmark (seconds).
    pub elapsed_seconds: Gauge,
}

impl OrionMetrics {
    pub fn new() -> Arc<Self> {
        let registry = Registry::new();

        let dag_blocks_created =
            IntCounter::new("orion_dag_blocks_created_total", "Total DAG blocks created").unwrap();
        let dag_subdags_committed = IntCounter::new(
            "orion_dag_subdags_committed_total",
            "Total sub-DAGs committed",
        )
        .unwrap();
        let dag_current_round =
            IntGauge::new("orion_dag_current_round", "Current DAG round").unwrap();
        let dag_committed_height =
            IntGauge::new("orion_dag_committed_height", "Current committed height").unwrap();
        let dag_transactions_ordered = IntCounter::new(
            "orion_dag_transactions_ordered_total",
            "Total transactions ordered",
        )
        .unwrap();

        let bft_checkpoints_finalized = IntCounter::new(
            "orion_bft_checkpoints_finalized_total",
            "Total BFT checkpoints finalized",
        )
        .unwrap();
        let bft_last_finalized_height = IntGauge::new(
            "orion_bft_last_finalized_height",
            "Last finalized checkpoint height",
        )
        .unwrap();

        let reth_blocks_produced = IntCounter::new(
            "orion_reth_blocks_produced_total",
            "Total Reth blocks produced",
        )
        .unwrap();
        let reth_txs_submitted = IntCounter::new(
            "orion_reth_txs_submitted_total",
            "Total transactions submitted to Reth",
        )
        .unwrap();
        let reth_block_number =
            IntGauge::new("orion_reth_block_number", "Latest Reth block number").unwrap();

        let engine_api_latency = HistogramVec::new(
            HistogramOpts::new(
                "orion_engine_api_latency_seconds",
                "Engine API call latency",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
            ]),
            &["method"],
        )
        .unwrap();

        let block_build_latency = Histogram::with_opts(
            HistogramOpts::new(
                "orion_block_build_latency_seconds",
                "Full block build cycle latency",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        )
        .unwrap();

        let current_tps =
            Gauge::new("orion_current_tps", "Current transactions per second").unwrap();
        let peak_tps =
            Gauge::new("orion_peak_tps", "Peak transactions per second observed").unwrap();
        let elapsed_seconds =
            Gauge::new("orion_elapsed_seconds", "Elapsed benchmark time").unwrap();

        // Register all metrics
        registry
            .register(Box::new(dag_blocks_created.clone()))
            .unwrap();
        registry
            .register(Box::new(dag_subdags_committed.clone()))
            .unwrap();
        registry
            .register(Box::new(dag_current_round.clone()))
            .unwrap();
        registry
            .register(Box::new(dag_committed_height.clone()))
            .unwrap();
        registry
            .register(Box::new(dag_transactions_ordered.clone()))
            .unwrap();
        registry
            .register(Box::new(bft_checkpoints_finalized.clone()))
            .unwrap();
        registry
            .register(Box::new(bft_last_finalized_height.clone()))
            .unwrap();
        registry
            .register(Box::new(reth_blocks_produced.clone()))
            .unwrap();
        registry
            .register(Box::new(reth_txs_submitted.clone()))
            .unwrap();
        registry
            .register(Box::new(reth_block_number.clone()))
            .unwrap();
        registry
            .register(Box::new(engine_api_latency.clone()))
            .unwrap();
        registry
            .register(Box::new(block_build_latency.clone()))
            .unwrap();
        registry.register(Box::new(current_tps.clone())).unwrap();
        registry.register(Box::new(peak_tps.clone())).unwrap();
        registry
            .register(Box::new(elapsed_seconds.clone()))
            .unwrap();

        Arc::new(Self {
            registry,
            dag_blocks_created,
            dag_subdags_committed,
            dag_current_round,
            dag_committed_height,
            dag_transactions_ordered,
            bft_checkpoints_finalized,
            bft_last_finalized_height,
            reth_blocks_produced,
            reth_txs_submitted,
            reth_block_number,
            engine_api_latency,
            block_build_latency,
            current_tps,
            peak_tps,
            elapsed_seconds,
        })
    }

    /// Encode all metrics into Prometheus text format.
    pub fn gather(&self) -> String {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }
}
