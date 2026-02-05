use clap::Parser;
use color_eyre::Result;
use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::dag::DagOrdering;
use orion::engine_api::{EngineApiClient, EngineApiConfig};
use orion::reth_execution::RethExecutionEngine;
use orion::types::{CommittedSubDag, Transaction};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Orion-Reth demo: DAG consensus driving Reth EVM execution via Engine API"
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

    /// Fee recipient address for block rewards
    #[clap(long, default_value = "0x0000000000000000000000000000000000000000")]
    fee_recipient: String,

    /// Number of demo blocks to produce
    #[clap(long, default_value = "10")]
    num_blocks: usize,

    /// Delay between blocks in milliseconds
    #[clap(long, default_value = "2000")]
    block_delay_ms: u64,

    /// Retry delay for Reth connection in seconds
    #[clap(long, default_value = "5")]
    retry_delay_secs: u64,

    /// Maximum retries for Reth connection
    #[clap(long, default_value = "60")]
    max_retries: usize,
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
    info!("║         Orion-Reth Integration Demo              ║");
    info!("║  Orion consensus  -->  Reth EVM execution        ║");
    info!("╚══════════════════════════════════════════════════╝");
    info!("Reth auth endpoint: {}", args.reth_auth_endpoint);
    info!("Reth RPC endpoint:  {}", args.reth_rpc_endpoint);

    // Load JWT secret
    let jwt_secret = load_jwt_secret(&args.jwt_secret_path)?;
    info!("JWT secret loaded ({} hex chars)", jwt_secret.len());

    // Create Engine API client
    let engine_api = Arc::new(EngineApiClient::new(EngineApiConfig {
        auth_endpoint: args.reth_auth_endpoint.clone(),
        rpc_endpoint: args.reth_rpc_endpoint.clone(),
        jwt_secret_hex: jwt_secret,
    }));

    // Wait for Reth to be ready
    info!("Waiting for Reth to become ready...");
    wait_for_reth(&engine_api, args.max_retries, args.retry_delay_secs).await?;
    info!("Reth is ready.");

    // Create Reth execution engine
    let reth_engine = Arc::new(RethExecutionEngine::new(
        engine_api.clone(),
        args.fee_recipient.clone(),
    ));

    // Initialize with genesis
    reth_engine.init().await?;
    info!(
        genesis = %reth_engine.latest_block_hash(),
        "Initialized with Reth genesis block"
    );

    // --- Orion consensus setup ---
    // Single-node committee: quorum=1, every block immediately commits.
    let keypair = KeyPair::generate();
    let validators = vec![ValidatorInfo {
        index: 0,
        public_key: keypair.verifying_key.clone(),
        stake: 1,
    }];
    let committee = Arc::new(Committee::new(validators));
    info!("Created single-validator committee (solo mode for demo)");

    // Create DAG ordering
    let dag = Arc::new(DagOrdering::new(committee.clone(), 0, keypair));

    // Subscribe to committed sub-DAGs
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel::<CommittedSubDag>();
    dag.set_commit_callback(commit_tx);

    // Channel to signal when execution is complete for each height
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u64>();

    // Spawn execution task: on each committed sub-DAG, build a Reth block
    let reth_engine_clone = reth_engine.clone();
    let engine_api_clone = engine_api.clone();
    let exec_handle = tokio::spawn(async move {
        let mut last_height = 0u64;
        while let Some(subdag) = commit_rx.recv().await {
            if subdag.height <= last_height {
                continue;
            }

            // Submit demo transactions to Reth's pool before building the block.
            // In the demo, we send value transfers via a locally signed raw tx.
            let tx_count = subdag
                .blocks
                .iter()
                .map(|b| b.transactions.len())
                .sum::<usize>();

            // For each Orion transaction, create an EVM value transfer in Reth's pool.
            for (i, block) in subdag.blocks.iter().enumerate() {
                for (j, _tx) in block.transactions.iter().enumerate() {
                    // Generate a random recipient address for the demo transfer
                    let recipient_bytes: [u8; 20] = rand::random();
                    let recipient = format!("0x{}", hex::encode(recipient_bytes));

                    // Send a small value transfer from the dev-funded account via eth_sendTransaction
                    match engine_api_clone
                        .send_transaction(
                            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
                            &recipient,
                            "0x1",
                        )
                        .await
                    {
                        Ok(tx_hash) => {
                            info!(
                                height = subdag.height,
                                block_idx = i,
                                tx_idx = j,
                                tx_hash = %tx_hash,
                                "Submitted EVM tx to Reth pool"
                            );
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                "Failed to submit EVM tx (Reth may not support eth_sendTransaction in this mode)"
                            );
                        }
                    }
                }
            }

            // Now build and finalize the Reth block
            match reth_engine_clone.execute_subdag(&subdag).await {
                Ok(executed) => {
                    info!(
                        height = executed.height,
                        block_hash = %reth_engine_clone.latest_block_hash(),
                        orion_txs = tx_count,
                        "Sub-DAG executed via Reth"
                    );
                    last_height = executed.height;
                    let _ = done_tx.send(executed.height);
                }
                Err(e) => {
                    error!(
                        error = %e,
                        height = subdag.height,
                        "Failed to execute sub-DAG via Reth"
                    );
                    let _ = done_tx.send(subdag.height);
                }
            }
        }
    });

    // --- Submit transactions and produce blocks ---
    info!("Producing {} blocks...", args.num_blocks);
    info!("─────────────────────────────────────────────────");

    for i in 0..args.num_blocks {
        let round = i as u64;

        // Create an Orion transaction (the data is just an identifier for the demo)
        let tx = Transaction::from(format!("orion_block_{}_tx", i));

        // Create a DAG block and add it — with committee size 1, this immediately
        // reaches quorum and triggers a commit.
        let block = dag.create_block(round, vec![], vec![tx]);
        dag.add_block(block);

        info!(round, "DAG block created and committed");

        // Wait for the execution to complete for this height
        if let Some(height) = done_rx.recv().await {
            info!(height, "Block execution confirmed");
        }

        if i < args.num_blocks - 1 {
            tokio::time::sleep(tokio::time::Duration::from_millis(args.block_delay_ms)).await;
        }
    }

    // Wait briefly for any remaining processing
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // --- Print summary ---
    info!("");
    info!("═══════════════════════════════════════════════════");
    info!("                    DEMO SUMMARY                   ");
    info!("═══════════════════════════════════════════════════");

    let executed = reth_engine.get_executed();
    info!("Reth blocks produced: {}", executed.len());
    info!(
        "Latest Reth block: {}",
        reth_engine.latest_block_hash()
    );

    // Query Reth for final block number
    match engine_api.get_block_number().await {
        Ok(num) => {
            let num_trimmed = if num.starts_with("0x") || num.starts_with("0X") {
                &num[2..]
            } else {
                num.as_str()
            };
            let block_num = u64::from_str_radix(num_trimmed, 16).unwrap_or(0);
            info!("Reth chain height: {} (0x{:x})", block_num, block_num);
        }
        Err(e) => warn!(error = %e, "Failed to query Reth block number"),
    }

    info!("");
    info!("Block details:");
    for block in &executed {
        info!(
            "  Height {:>3} | Block: 0x{}... | Txs: {}",
            block.height,
            hex::encode(&block.state_root.0[..4]),
            block.transactions.len(),
        );
    }

    info!("═══════════════════════════════════════════════════");
    info!("Demo complete. Running until SIGTERM/SIGINT (e.g. docker stop).");

    // Keep container alive until shutdown signal.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = sigterm.recv() => {},
                    _ = tokio::signal::ctrl_c() => {},
                }
            }
            Err(e) => {
                warn!(error = %e, "SIGTERM handler unavailable, falling back to Ctrl+C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }

    info!("Shutting down demo...");
    Ok(())
}

fn load_jwt_secret(path: &str) -> Result<String> {
    // Try to read as file
    if let Ok(contents) = std::fs::read_to_string(path) {
        let secret = contents.trim().to_string();
        let clean = secret.trim_start_matches("0x");
        if clean.len() == 64 && hex::decode(clean).is_ok() {
            return Ok(clean.to_string());
        }
    }

    // Try treating the argument itself as a hex string
    let clean = path.trim().trim_start_matches("0x");
    if clean.len() == 64 && hex::decode(clean).is_ok() {
        return Ok(clean.to_string());
    }

    color_eyre::eyre::bail!(
        "Invalid JWT secret at '{}': expected 64 hex chars (32 bytes)",
        path
    )
}

async fn wait_for_reth(
    engine_api: &EngineApiClient,
    max_retries: usize,
    retry_delay_secs: u64,
) -> Result<()> {
    for attempt in 1..=max_retries {
        match engine_api.get_block_by_number("0x0", false).await {
            Ok(_) => {
                info!(attempt, "Connected to Reth successfully");
                return Ok(());
            }
            Err(e) => {
                if attempt % 5 == 1 {
                    warn!(
                        attempt,
                        max_retries,
                        error = %e,
                        "Reth not ready, retrying..."
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
            }
        }
    }
    color_eyre::eyre::bail!("Failed to connect to Reth after {} attempts", max_retries)
}
