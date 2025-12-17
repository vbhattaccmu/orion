use clap::Parser;
use color_eyre::Result;
use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::node::HybridNode;
use orion::types::Transaction;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(author, version, about = "Hybrid DAG + BFT checkpoint consensus node")]
struct Args {
    /// The authority index of this node
    #[clap(long, value_name = "INT", default_value = "0")]
    authority: u32,
    /// Number of validators in the committee
    #[clap(long, value_name = "INT", default_value = "4")]
    committee_size: usize,
    /// Interval (in committed heights) for checkpoint formation
    #[clap(long, value_name = "INT", default_value = "10")]
    checkpoint_interval: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    fmt().with_env_filter(filter).init();

    let args = Args::parse();

    info!("Starting orion-node (authority {})", args.authority);

    // Create committee with dummy validators
    let mut validators = Vec::new();
    for i in 0..args.committee_size {
        let keypair = KeyPair::generate();
        validators.push(ValidatorInfo {
            index: i as u32,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
    }
    let committee = Arc::new(Committee::new(validators));

    // Generate keypair for this node
    let keypair = KeyPair::generate();

    // Create hybrid node
    let node = HybridNode::new(
        committee.clone(),
        args.authority,
        keypair,
        args.checkpoint_interval,
    );

    // Start the node
    let handle = node.start();

    // Simulate some transactions
    let dag = node.dag();
    for i in 0..20 {
        let tx = Transaction::from(format!("tx_{}", i));
        let block = dag.create_block(
            i,
            vec![], // No parents for simplicity
            vec![tx],
        );
        dag.add_block(block);
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Wait a bit for processing
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    info!("Node running. Press Ctrl+C to exit.");

    // Keep running
    tokio::select! {
        _ = handle => {},
        _ = tokio::signal::ctrl_c() => {
            info!("Shutting down...");
        }
    }

    Ok(())
}
