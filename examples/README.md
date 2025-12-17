## Orion Examples

This directory contains examples showing how to integrate the **Orion** consensus engine (crate: `orion`) into an application.

### Submitting Transactions

Applications submit transactions by creating blocks in the DAG:

```rust
use orion::node::HybridNode;
use orion::types::Transaction;

// Get the DAG instance from the node
let dag = node.dag();

// Create a transaction
let tx = Transaction::from(b"your transaction data".to_vec());

// Create a block with the transaction
let block = dag.create_block(
    round_number,      // Current round
    parent_blocks,     // Parent block IDs (for DAG structure)
    vec![tx],          // Transactions to include
);

// Add the block to the DAG (this triggers consensus)
dag.add_block(block);
```

### Subscribing to Committed Transactions

Applications can subscribe to committed sub-dags to know when transactions are ordered:

```rust
use tokio::sync::mpsc;

// Create a channel to receive commits
let (tx, mut rx) = mpsc::unbounded_channel();

// Set the callback on the DAG
dag.set_commit_callback(tx);

// Listen for commits in a background task
tokio::spawn(async move {
    while let Some(subdag) = rx.recv().await {
        println!("Committed at height: {}", subdag.height);
        for block in subdag.blocks {
            for tx in block.transactions {
                // Process ordered transaction
                println!("Ordered TX: {:?}", tx);
            }
        }
    }
});
```

### Querying Executed State

Applications can query the execution layer for executed blocks and state:

```rust
// Get the execution engine from the node
let execution = node.execution();

// Get all executed blocks
let executed_blocks = execution.get_executed();

for block in executed_blocks {
    println!("Executed at height: {}", block.height);
    println!("State root: {:?}", block.state_root);
    println!("Transactions: {:?}", block.transactions);
}
```

### Checking Checkpoint Status

Applications can check BFT checkpoint finalization:

```rust
// Get the BFT engine from the node
let bft = node.bft();

// Check last finalized checkpoint
if let Some(checkpoint) = bft.last_finalized() {
    println!("Last finalized checkpoint at height: {}", checkpoint.height);
    println!("State root: {:?}", checkpoint.state_root);
}
```

### Complete Application Example

```rust
use orion::node::HybridNode;
use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::types::Transaction;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create or load committee
    let committee = create_committee();
    
    // 2. Create node
    let keypair = KeyPair::generate();
    let node = HybridNode::new(
        committee,
        0,  // authority index
        keypair,
        10, // checkpoint interval
    );
    
    // 3. Start the node
    let _handle = node.start();
    
    // 4. Subscribe to commits
    let (commit_tx, mut commit_rx) = mpsc::unbounded_channel();
    node.dag().set_commit_callback(commit_tx);
    
    tokio::spawn(async move {
        while let Some(subdag) = commit_rx.recv().await {
            println!("New commit at height {}", subdag.height);
        }
    });
    
    // 5. Submit transactions
    let dag = node.dag();
    for i in 0..10 {
        let tx = Transaction::from(format!("tx_{}", i));
        let block = dag.create_block(i, vec![], vec![tx]);
        dag.add_block(block);
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    
    // 6. Query executed state
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    let executed = node.execution().get_executed();
    println!("Executed {} blocks", executed.len());
    
    Ok(())
}
```


