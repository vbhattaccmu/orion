use blake2::Digest;
use clap::Parser;
use color_eyre::Result;
use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::dag::DagOrdering;
use orion::engine_api::{EngineApiClient, EngineApiConfig, EngineApiError};
use orion::metrics::OrionMetrics;
use orion::network::{DataPlane, TcpDataPlane};
use orion::reth_execution::RethExecutionEngine;
use orion::types::{CommittedSubDag, DagBlock, Transaction};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Orion-Reth TPS benchmark: measures block production throughput"
)]
struct Args {
    /// Reth Engine API (authenticated) endpoint
    #[clap(long, default_value = "http://reth:8551")]
    reth_auth_endpoint: String,

    /// Reth JSON-RPC endpoint
    #[clap(long, default_value = "http://reth:8545")]
    reth_rpc_endpoint: String,

    /// Path to hex-encoded JWT secret file
    #[clap(long, default_value = "/secrets/jwt.hex")]
    jwt_secret_path: String,

    /// Fee recipient address
    #[clap(long, default_value = "0x0000000000000000000000000000000000000000")]
    fee_recipient: String,

    /// Total number of blocks to produce
    #[clap(long, default_value = "50")]
    num_blocks: usize,

    /// Number of transactions per block
    #[clap(long, default_value = "10")]
    txs_per_block: usize,

    /// Whether to submit EVM transactions to Reth's pool before each block
    #[clap(long, default_value = "true")]
    submit_evm_txs: bool,

    /// Delay between benchmark block submissions (ms).
    /// Helps avoid overdriving Engine API and txpool under heavy load.
    #[clap(long, default_value = "25")]
    block_delay_ms: u64,

    /// Number of retries for block build on retryable Engine API errors.
    #[clap(long, default_value = "1")]
    max_block_build_retries: usize,

    /// Delay between block build retries (ms).
    #[clap(long, default_value = "25")]
    block_build_retry_delay_ms: u64,

    /// Extra delay added after an execution/build failure (ms).
    #[clap(long, default_value = "50")]
    error_backoff_ms: u64,

    /// Max pacing delay (ms) when backing off on repeated failures.
    #[clap(long, default_value = "1000")]
    max_block_delay_ms: u64,

    /// Delay reduction after successful blocks (ms).
    #[clap(long, default_value = "25")]
    recovery_step_ms: u64,

    /// Prometheus metrics port
    #[clap(long, default_value = "9090")]
    metrics_port: u16,

    /// Retry delay for Reth connection in seconds
    #[clap(long, default_value = "5")]
    retry_delay_secs: u64,

    /// Maximum retries for Reth connection
    #[clap(long, default_value = "60")]
    max_retries: usize,

    /// Run indefinitely instead of stopping after `num_blocks`.
    #[clap(long, default_value = "false")]
    run_forever: bool,

    /// Validator index within the Orion committee (0-based).
    #[clap(long, default_value = "0")]
    validator_index: u32,

    /// Total number of validators in the Orion committee.
    #[clap(long, default_value = "1")]
    committee_size: u32,

    /// TCP listen address for the Orion data plane (for DAG blocks).
    #[clap(long, default_value = "0.0.0.0:9000")]
    listen_addr: String,

    /// Comma-separated list of peer TCP addresses for other Orion validators.
    /// Example: "orion-bench2:9000,orion-bench3:9000"
    #[clap(long, default_value = "")]
    peer_addrs: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    fmt().with_env_filter(filter).init();

    let args = Args::parse();

    info!("╔══════════════════════════════════════════════════╗");
    info!("║          Orion-Reth TPS Benchmark                ║");
    info!("╚══════════════════════════════════════════════════╝");
    info!(
        "Blocks: {}, TXs/block: {}",
        args.num_blocks, args.txs_per_block
    );
    info!(
        "Total transactions: {}",
        args.num_blocks * args.txs_per_block
    );

    // Initialize metrics
    let metrics = OrionMetrics::new();

    // Start Prometheus metrics server
    let metrics_clone = metrics.clone();
    let metrics_port = args.metrics_port;
    tokio::spawn(async move {
        serve_metrics(metrics_clone, metrics_port).await;
    });
    info!(
        "Prometheus metrics at http://0.0.0.0:{}/metrics",
        args.metrics_port
    );

    // Load JWT secret
    let jwt_secret = load_jwt_secret(&args.jwt_secret_path)?;

    // Create Engine API client
    let engine_api = Arc::new(EngineApiClient::new(EngineApiConfig {
        auth_endpoint: args.reth_auth_endpoint.clone(),
        rpc_endpoint: args.reth_rpc_endpoint.clone(),
        jwt_secret_hex: jwt_secret,
    }));

    // Wait for Reth
    info!("Waiting for Reth...");
    wait_for_reth(&engine_api, args.max_retries, args.retry_delay_secs).await?;
    info!("Reth is ready.");

    // Create Reth execution engine
    let reth_engine = Arc::new(RethExecutionEngine::new(
        engine_api.clone(),
        args.fee_recipient.clone(),
    ));
    reth_engine.init().await?;
    info!(genesis = %reth_engine.latest_block_hash(), "Genesis loaded");

    // Setup multi-node consensus via a shared committee derived deterministically.
    // All Orion benchmarks derive the same committee from seeds; each uses its own validator_index.
    let committee_size = args.committee_size.max(1);
    let my_index = args.validator_index.min(committee_size - 1);

    let mut validators = Vec::new();
    let mut keypairs = Vec::new();
    for i in 0..committee_size {
        use blake2::Blake2s256;

        let mut hasher = Blake2s256::new();
        Digest::update(&mut hasher, format!("orion-dev-committee-{}", i).as_bytes());
        let hash = hasher.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&hash[..32]);

        let kp = KeyPair::from_seed(&seed);
        validators.push(ValidatorInfo {
            index: i,
            public_key: kp.verifying_key.clone(),
            stake: 1,
        });
        keypairs.push(kp);
    }

    let committee = Arc::new(Committee::new(validators));
    let my_kp = keypairs[my_index as usize].clone();
    let dag = Arc::new(DagOrdering::new(committee.clone(), my_index, my_kp));

    // Attach TCP-based data plane so all Orion validators gossip over the Docker network.
    let peer_addrs: Vec<String> = args
        .peer_addrs
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();
    let tcp_plane = TcpDataPlane::new(args.listen_addr.clone(), peer_addrs).await;
    dag.set_data_plane(tcp_plane.clone());

    // Spawn network receiver: listen on the DataPlane and feed incoming DAG blocks
    // into this node's DAG. This is what allows all validators to see each other's
    // blocks and reach quorum.
    {
        let dag_clone = dag.clone();
        tokio::spawn(async move {
            let mut rx = tcp_plane.subscribe();
            while let Some(data) = rx.recv().await {
                // DataPlane sends Vec<u8>
                let data: Vec<u8> = data;
                if let Ok(block) = bincode::deserialize::<DagBlock>(&data) {
                    let _ = dag_clone.add_block(block);
                }
            }
        });
    }

    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel::<CommittedSubDag>();
    dag.set_commit_callback(commit_tx);

    // Completion signal
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(u64, f64)>(); // (height, latency_secs)

    let total_txs_executed = Arc::new(AtomicU64::new(0));
    let total_txs_clone = total_txs_executed.clone();

    // Execution task
    let reth_engine_clone = reth_engine.clone();
    let engine_api_clone = engine_api.clone();
    let metrics_clone = metrics.clone();
    let submit_evm = args.submit_evm_txs;
    let max_build_retries = args.max_block_build_retries;
    let retry_delay_ms = args.block_build_retry_delay_ms;

    let exec_handle = tokio::spawn(async move {
        let mut last_height = 0u64;
        while let Some(subdag) = commit_rx.recv().await {
            if subdag.height <= last_height {
                continue;
            }

            let block_start = Instant::now();

            // Submit EVM transactions if enabled
            if submit_evm {
                for block in subdag.blocks.iter() {
                    for _tx in block.transactions.iter() {
                        let recipient_bytes: [u8; 20] = rand::random();
                        let recipient = format!("0x{}", hex::encode(recipient_bytes));

                        let tx_start = Instant::now();
                        match engine_api_clone
                            .send_transaction(
                                "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
                                &recipient,
                                "0x1",
                            )
                            .await
                        {
                            Ok(_) => {
                                metrics_clone.reth_txs_submitted.inc();
                                metrics_clone
                                    .engine_api_latency
                                    .with_label_values(&["eth_sendTransaction"])
                                    .observe(tx_start.elapsed().as_secs_f64());
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to submit EVM tx");
                            }
                        }
                    }
                }
            }

            // Build Reth block with retry on known transient Engine API errors.
            let mut attempt = 0usize;
            let build_result = loop {
                let result = reth_engine_clone.execute_subdag(&subdag).await;
                match result {
                    Ok(executed) => break Ok(executed),
                    Err(e) if is_retryable_build_error(&e) && attempt < max_build_retries => {
                        attempt += 1;
                        warn!(
                            height = subdag.height,
                            attempt,
                            max_build_retries,
                            error = %e,
                            "Retrying block build after retryable error"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms))
                            .await;
                    }
                    Err(e) => break Err(e),
                }
            };

            match build_result {
                Ok(executed) => {
                    let latency = block_start.elapsed().as_secs_f64();
                    let tx_count = executed.transactions.len() as u64;

                    metrics_clone.reth_blocks_produced.inc();
                    metrics_clone.dag_subdags_committed.inc();
                    metrics_clone
                        .dag_committed_height
                        .set(executed.height as i64);
                    metrics_clone.dag_transactions_ordered.inc_by(tx_count);
                    metrics_clone.block_build_latency.observe(latency);

                    total_txs_clone.fetch_add(tx_count, AtomicOrdering::Relaxed);
                    last_height = executed.height;

                    let _ = done_tx.send((executed.height, latency));
                }
                Err(e) => {
                    error!(error = %e, height = subdag.height, "Block build failed");
                    let _ = done_tx.send((subdag.height, -1.0));
                }
            }
        }
    });

    // ── Run the benchmark ──
    info!("Starting benchmark...");
    info!("─────────────────────────────────────────────────");

    if args.run_forever {
        info!("Running in run-forever mode (ignoring num_blocks)");
        let mut i: usize = 0;
        let mut current_delay_ms = args.block_delay_ms;
        let mut successful_blocks = 0u64;
        let mut failed_blocks = 0u64;
        let run_start = Instant::now();
        loop {
            let round = i as u64;

            let txs: Vec<Transaction> = (0..args.txs_per_block)
                .map(|j| format!("bench_block_{}_tx_{}", i, j).into_bytes())
                .collect();

            metrics.dag_blocks_created.inc();
            metrics.dag_current_round.set(round as i64);

            let block = dag.create_block(round, vec![], txs);
            dag.add_block(block);

            i += 1;

            // Drain completed execution results and adapt pacing.
            while let Ok((height, latency)) = done_rx.try_recv() {
                if latency >= 0.0 {
                    successful_blocks += 1;
                    current_delay_ms =
                        recover_delay(current_delay_ms, args.block_delay_ms, args.recovery_step_ms);
                } else {
                    failed_blocks += 1;
                    current_delay_ms = backoff_delay(
                        current_delay_ms,
                        args.error_backoff_ms,
                        args.max_block_delay_ms,
                    );
                    warn!(
                        height,
                        failed_blocks,
                        delay_ms = current_delay_ms,
                        "Execution failed; increasing producer delay"
                    );
                }
            }

            if i % 10 == 0 {
                let elapsed = run_start.elapsed().as_secs_f64();
                let total_txs = total_txs_executed.load(AtomicOrdering::Relaxed);
                let current_tps = if elapsed > 0.0 {
                    total_txs as f64 / elapsed
                } else {
                    0.0
                };
                info!(
                    produced_blocks = i,
                    successful_blocks,
                    failed_blocks,
                    delay_ms = current_delay_ms,
                    tps = format!("{:.1}", current_tps),
                    "Run-forever progress"
                );
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(current_delay_ms)).await;
        }
        // Unreachable
    } else {
        let bench_start = Instant::now();
        let mut successful_blocks = 0u64;
        let mut failed_blocks = 0u64;
        let mut peak_tps_val: f64 = 0.0;
        let mut current_delay_ms = args.block_delay_ms;

        let mut block_latencies = Vec::with_capacity(args.num_blocks);

        for i in 0..args.num_blocks {
            let round = i as u64;
            let mut block_succeeded = false;

            // Create transactions for this block
            let txs: Vec<Transaction> = (0..args.txs_per_block)
                .map(|j| format!("bench_block_{}_tx_{}", i, j).into_bytes())
                .collect();

            metrics.dag_blocks_created.inc();
            metrics.dag_current_round.set(round as i64);

            let block = dag.create_block(round, vec![], txs);
            dag.add_block(block);

            // Wait for execution
            if let Some((height, latency)) = done_rx.recv().await {
                if latency >= 0.0 {
                    block_succeeded = true;
                    successful_blocks += 1;
                    block_latencies.push(latency);

                    let elapsed = bench_start.elapsed().as_secs_f64();
                    let total_txs = total_txs_executed.load(AtomicOrdering::Relaxed);
                    let current_tps = if elapsed > 0.0 {
                        total_txs as f64 / elapsed
                    } else {
                        0.0
                    };

                    if current_tps > peak_tps_val {
                        peak_tps_val = current_tps;
                        metrics.peak_tps.set(peak_tps_val);
                    }
                    metrics.current_tps.set(current_tps);
                    metrics.elapsed_seconds.set(elapsed);

                    if (i + 1) % 10 == 0 || i == 0 {
                        info!(
                            block = i + 1,
                            height,
                            latency_ms = format!("{:.1}", latency * 1000.0),
                            tps = format!("{:.1}", current_tps),
                            "Progress"
                        );
                    }
                } else {
                    failed_blocks += 1;
                    current_delay_ms = backoff_delay(
                        current_delay_ms,
                        args.error_backoff_ms,
                        args.max_block_delay_ms,
                    );
                    warn!(
                        block = i + 1,
                        delay_ms = current_delay_ms,
                        "Block failed; increasing producer delay"
                    );
                }
            }

            if block_succeeded {
                current_delay_ms =
                    recover_delay(current_delay_ms, args.block_delay_ms, args.recovery_step_ms);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(current_delay_ms)).await;
        }

        let total_elapsed = bench_start.elapsed();
        let total_txs = total_txs_executed.load(AtomicOrdering::Relaxed);

        // Query final Reth block number
        let reth_block_num = match engine_api.get_block_number().await {
            Ok(num) => {
                let num_trimmed = if num.starts_with("0x") || num.starts_with("0X") {
                    &num[2..]
                } else {
                    num.as_str()
                };
                let n = u64::from_str_radix(num_trimmed, 16).unwrap_or(0);
                metrics.reth_block_number.set(n as i64);
                n
            }
            Err(_) => 0,
        };

        // ── Print results ──
        info!("");
        info!("═══════════════════════════════════════════════════════════════");
        info!("                     BENCHMARK RESULTS                        ");
        info!("═══════════════════════════════════════════════════════════════");
        info!("");
        info!(
            "  Total time:              {:.2}s",
            total_elapsed.as_secs_f64()
        );
        info!(
            "  Blocks produced:         {} / {}",
            successful_blocks, args.num_blocks
        );
        info!("  Failed blocks:           {}", failed_blocks);
        info!("  Transactions per block:  {}", args.txs_per_block);
        info!("  Total transactions:      {}", total_txs);
        info!("  Reth chain height:       {}", reth_block_num);
        info!("");

        let avg_tps = if total_elapsed.as_secs_f64() > 0.0 {
            total_txs as f64 / total_elapsed.as_secs_f64()
        } else {
            0.0
        };
        let avg_bps = if total_elapsed.as_secs_f64() > 0.0 {
            successful_blocks as f64 / total_elapsed.as_secs_f64()
        } else {
            0.0
        };

        info!("  ── Throughput ──");
        info!("  Average TPS:             {:.2}", avg_tps);
        info!("  Average blocks/sec:      {:.2}", avg_bps);
        info!("  Peak TPS:                {:.2}", peak_tps_val);
        info!("");

        if !block_latencies.is_empty() {
            block_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let min = block_latencies.first().unwrap();
            let max = block_latencies.last().unwrap();
            let avg: f64 = block_latencies.iter().sum::<f64>() / block_latencies.len() as f64;
            let p50 = block_latencies[block_latencies.len() / 2];
            let p95 = block_latencies[(block_latencies.len() as f64 * 0.95) as usize];
            let p99 = block_latencies[(block_latencies.len() as f64 * 0.99) as usize];

            info!("  ── Block Latency ──");
            info!("  Min:                     {:.1}ms", min * 1000.0);
            info!("  Avg:                     {:.1}ms", avg * 1000.0);
            info!("  P50:                     {:.1}ms", p50 * 1000.0);
            info!("  P95:                     {:.1}ms", p95 * 1000.0);
            info!("  P99:                     {:.1}ms", p99 * 1000.0);
            info!("  Max:                     {:.1}ms", max * 1000.0);
        }

        info!("");
        info!("═══════════════════════════════════════════════════════════════");
        info!(
            "Metrics available at http://0.0.0.0:{}/metrics",
            args.metrics_port
        );
        info!("Press Ctrl+C to exit.");
    }

    tokio::select! {
        _ = exec_handle => {},
        _ = tokio::signal::ctrl_c() => {
            info!("Shutting down...");
        }
    }

    Ok(())
}

fn is_retryable_build_error(err: &EngineApiError) -> bool {
    match err {
        EngineApiError::JsonRpc { code, message } => {
            *code == -38003 && message.contains("Invalid payload attributes")
        }
        _ => false,
    }
}

fn backoff_delay(current: u64, backoff_step: u64, max_delay: u64) -> u64 {
    current.saturating_add(backoff_step).min(max_delay)
}

fn recover_delay(current: u64, base: u64, recovery_step: u64) -> u64 {
    if current <= base {
        return base;
    }
    let reduced = current.saturating_sub(recovery_step);
    if reduced < base {
        base
    } else {
        reduced
    }
}

/// Serve Prometheus metrics on an HTTP endpoint.
async fn serve_metrics(metrics: Arc<OrionMetrics>, port: u16) {
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    let addr = format!("0.0.0.0:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind metrics server on {}: {}", addr, e);
            return;
        }
    };

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let metrics = metrics.clone();
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let metrics = metrics.clone();
                async move {
                    if req.uri().path() == "/metrics" {
                        let body = metrics.gather();
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .header("Content-Type", "text/plain; version=0.0.4")
                                .body(Full::new(Bytes::from(body)))
                                .unwrap(),
                        )
                    } else {
                        Ok(Response::builder()
                            .status(404)
                            .body(Full::new(Bytes::from("Not Found")))
                            .unwrap())
                    }
                }
            });

            if let Err(e) =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(io, service)
                    .await
            {
                warn!("Metrics connection error: {}", e);
            }
        });
    }
}

fn load_jwt_secret(path: &str) -> Result<String> {
    if let Ok(contents) = std::fs::read_to_string(path) {
        let secret = contents.trim().to_string();
        let clean = secret.trim_start_matches("0x");
        if clean.len() == 64 && hex::decode(clean).is_ok() {
            return Ok(clean.to_string());
        }
    }
    let clean = path.trim().trim_start_matches("0x");
    if clean.len() == 64 && hex::decode(clean).is_ok() {
        return Ok(clean.to_string());
    }
    color_eyre::eyre::bail!("Invalid JWT secret at '{}': expected 64 hex chars", path)
}

async fn wait_for_reth(
    engine_api: &EngineApiClient,
    max_retries: usize,
    retry_delay_secs: u64,
) -> Result<()> {
    for attempt in 1..=max_retries {
        match engine_api.get_block_by_number("0x0", false).await {
            Ok(_) => {
                info!(attempt, "Connected to Reth");
                return Ok(());
            }
            Err(e) => {
                if attempt % 5 == 1 {
                    warn!(attempt, max_retries, error = %e, "Reth not ready...");
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
            }
        }
    }
    color_eyre::eyre::bail!("Failed to connect to Reth after {} attempts", max_retries)
}
