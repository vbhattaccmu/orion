// Example: How an application connects to the hybrid consensus system

use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::node::HybridNode;
use orion::types::Transaction;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Hybrid Consensus Application Connection Example ===\n");

    // Step 1: Create or load committee
    println!("1. Creating committee...");
    let mut validators = Vec::new();
    let mut keypairs = Vec::new();
    for i in 0..4 {
        let keypair = KeyPair::generate();
        keypairs.push(keypair.clone());
        validators.push(ValidatorInfo {
            index: i as u32,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
    }
    let committee = Arc::new(Committee::new(validators));
    println!(
        "   ✓ Committee created with {} validators\n",
        committee.size()
    );

    // Step 2: Create hybrid node
    println!("2. Creating hybrid consensus node...");
    let node_keypair = keypairs[0].clone();
    let node = HybridNode::new(
        committee.clone(),
        0, // authority index
        node_keypair,
        10, // checkpoint interval (every 10 blocks)
    );
    println!("   ✓ Node created\n");

    // Step 3: Start the node (this starts the consensus and execution pipeline)
    println!("3. Starting consensus node...");
    let _handle = node.start();
    println!("   ✓ Node started\n");

    // Step 4: Subscribe to committed transactions (ordered by consensus)
    println!("4. Subscribing to committed transactions...");
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel();
    node.dag().set_commit_callback(commit_tx);

    let _commit_handle = tokio::spawn(async move {
        while let Some(subdag) = commit_rx.recv().await {
            println!("\n   📦 COMMIT RECEIVED:");
            println!("      Height: {}", subdag.height);
            println!("      Blocks: {}", subdag.blocks.len());
            let tx_count: usize = subdag.blocks.iter().map(|b| b.transactions.len()).sum();
            println!("      Transactions: {}", tx_count);
            println!("      → These transactions are now ORDERED by consensus");
        }
    });
    println!("   ✓ Subscribed to commits\n");

    // Step 5: Submit transactions to the consensus
    println!("5. Submitting transactions...");
    let dag = node.dag();
    for i in 0..15 {
        let tx_data = format!("app_tx_{}", i);
        let tx = Transaction::from(tx_data.clone());

        // Create a block with the transaction
        let block = dag.create_block(
            i,      // round number
            vec![], // parent blocks (empty for simplicity)
            vec![tx],
        );

        // Add block to DAG (triggers consensus)
        dag.add_block(block);
        println!("   ✓ Submitted transaction: {}", tx_data);

        sleep(Duration::from_millis(50)).await;
    }
    println!("\n   ✓ All transactions submitted\n");

    // Step 6: Wait for processing
    println!("6. Waiting for consensus and execution...");
    sleep(Duration::from_secs(2)).await;
    println!("   ✓ Processing complete\n");

    // Step 7: Query executed state
    println!("7. Querying executed state...");
    let executed_blocks = node.execution().get_executed();
    println!("   ✓ Found {} executed blocks", executed_blocks.len());

    for block in &executed_blocks {
        println!("\n   📊 EXECUTED BLOCK:");
        println!("      Height: {}", block.height);
        println!(
            "      State Root: {:?}",
            hex::encode(&block.state_root.0[..8])
        );
        println!("      Transactions: {}", block.transactions.len());
        println!("      → State has been updated based on ordered transactions");
    }

    // Step 8: Check checkpoint status
    println!("\n8. Checking checkpoint status...");
    if let Some(checkpoint) = node.bft().last_finalized() {
        println!("   ✓ Checkpoint finalized:");
        println!("      Height: {}", checkpoint.height);
        println!(
            "      State Root: {:?}",
            hex::encode(&checkpoint.state_root.0[..8])
        );
        println!("      → This checkpoint is FINALIZED via BFT");
    } else {
        println!("   ⏳ No checkpoint finalized yet (need more blocks)");
    }

    println!("\n=== Example Complete ===");
    println!("\nKey Takeaways:");
    println!("1. Transactions are submitted to DAG consensus");
    println!("2. Consensus determines the ORDER of transactions");
    println!("3. Execution processes transactions in consensus order");
    println!("4. Checkpoints provide strong finality via BFT");
    println!("5. Order is immutable once committed");

    // Keep running for a bit to see commits
    sleep(Duration::from_secs(1)).await;

    Ok(())
}
