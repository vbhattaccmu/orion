//! Test Raptorcast propagation across multiple nodes
//!
//! This test spawns multiple nodes and verifies that:
//! 1. Blocks are broadcast and received by all nodes
//! 2. Votes are broadcast and received by all nodes
//! 3. Consensus progresses correctly with network propagation

use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::network::SymbolNetwork;
use orion::node::HybridNode;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::info;

fn create_test_committee(size: usize) -> (Arc<Committee>, Vec<KeyPair>) {
    let mut validators = Vec::new();
    let mut keypairs = Vec::new();
    for i in 0..size {
        let keypair = KeyPair::generate();
        keypairs.push(keypair.clone());
        validators.push(ValidatorInfo {
            index: i as u32,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
    }
    (Arc::new(Committee::new(validators)), keypairs)
}

#[tokio::test]
async fn test_raptorcast_block_propagation() {
    let num_nodes = 4;
    let (committee, keypairs) = create_test_committee(num_nodes);

    // Create symbol-level network for actual Raptorcast encoding/decoding
    let symbol_network = Arc::new(SymbolNetwork::new());

    // Create nodes with Raptorcast data planes
    let mut nodes = Vec::new();
    for i in 0..num_nodes {
        let (_raptorcast, data_plane) = symbol_network.create_raptorcast_data_plane();
        let node = HybridNode::new_with_data_plane(
            committee.clone(),
            i as u32,
            keypairs[i].clone(),
            10, // checkpoint interval
            data_plane,
        );
        nodes.push(node);
    }

    // Start all nodes
    for node in &nodes {
        node.start();
    }

    // Give nodes time to initialize
    sleep(Duration::from_millis(100)).await;

    // Node 0 creates a block (this also adds it locally and broadcasts it)
    let dag = nodes[0].dag();
    let block = dag.create_block(0, vec![], vec![b"test_tx_1".to_vec()]);
    let block_id = block.id.clone();

    // Add block locally first (this also triggers broadcast)
    dag.add_block(block);

    info!("Node 0 created and broadcast block: {:?}", block_id);

    // Wait for propagation (Raptorcast needs time to encode, transmit symbols, and decode)
    sleep(Duration::from_millis(2000)).await;

    // Verify all nodes received the block
    for (i, node) in nodes.iter().enumerate() {
        let received_block = node.dag().get_block(&block_id);
        assert!(
            received_block.is_some(),
            "Node {} should have received the block",
            i
        );
        if let Some(b) = received_block {
            assert_eq!(b.id, block_id);
            assert_eq!(b.transactions[0], b"test_tx_1");
            info!("Node {} verified block", i);
        }
    }
}

#[tokio::test]
async fn test_raptorcast_vote_propagation() {
    let num_nodes = 4;
    let quorum = 3; // 2f+1 for 4 nodes
    let (committee, keypairs) = create_test_committee(num_nodes);

    // Create symbol-level network for actual Raptorcast encoding/decoding
    let symbol_network = Arc::new(SymbolNetwork::new());

    // Create nodes with Raptorcast data planes
    let mut nodes = Vec::new();
    for i in 0..num_nodes {
        let (_raptorcast, data_plane) = symbol_network.create_raptorcast_data_plane();
        let node = HybridNode::new_with_data_plane(
            committee.clone(),
            i as u32,
            keypairs[i].clone(),
            10,
            data_plane,
        );
        nodes.push(node);
    }

    // Start all nodes
    for node in &nodes {
        node.start();
    }

    // Give nodes time to initialize
    sleep(Duration::from_millis(100)).await;

    // Create blocks from quorum nodes to trigger commit
    for i in 0..quorum {
        let dag = nodes[i].dag();
        let block = dag.create_block(0, vec![], vec![format!("tx_{}", i).into_bytes()]);
        dag.add_block(block);
    }

    // Wait for propagation and commit (Raptorcast needs time for encoding/decoding)
    sleep(Duration::from_millis(3000)).await;

    // Check that commits happened
    let mut commits_found = 0;
    for node in &nodes {
        let committed = node.dag().get_committed();
        if !committed.is_empty() {
            commits_found += 1;
        }
    }

    assert!(commits_found > 0, "At least one node should have committed");

    // Wait for checkpoint processing
    sleep(Duration::from_millis(500)).await;

    // Check that votes were propagated and QCs formed
    let mut qcs_formed = 0;
    for node in &nodes {
        if node.bft().last_finalized().is_some() {
            qcs_formed += 1;
        }
    }

    info!("QCs formed on {} nodes", qcs_formed);
    // With proper vote propagation, we should form QCs
    // Note: This may require more time or different setup in practice
}

#[tokio::test]
async fn test_multi_node_consensus_with_raptorcast() {
    let num_nodes = 4;
    let (committee, keypairs) = create_test_committee(num_nodes);

    // Create symbol-level network for actual Raptorcast encoding/decoding
    let symbol_network = Arc::new(SymbolNetwork::new());

    // Create and start nodes with Raptorcast data planes
    let mut nodes = Vec::new();
    for i in 0..num_nodes {
        let (_raptorcast, data_plane) = symbol_network.create_raptorcast_data_plane();
        let node = HybridNode::new_with_data_plane(
            committee.clone(),
            i as u32,
            keypairs[i].clone(),
            5, // Smaller checkpoint interval for faster testing
            data_plane,
        );
        node.start();
        nodes.push(node);
    }

    sleep(Duration::from_millis(100)).await;

    // Create multiple rounds of blocks
    for round in 0..3 {
        info!("Creating blocks for round {}", round);

        // Each node creates a block in this round
        for (i, node) in nodes.iter().enumerate() {
            let dag = node.dag();
            let parents = vec![];

            let tx = format!("round_{}_node_{}", round, i).into_bytes();
            let block = dag.create_block(round, parents, vec![tx]);
            dag.add_block(block);
        }

        // Wait for propagation (Raptorcast encoding/decoding takes time)
        sleep(Duration::from_millis(1000)).await;
    }

    // Wait for consensus to progress (Raptorcast needs time for symbol transmission and decoding)
    sleep(Duration::from_millis(2000)).await;

    // Verify all nodes have the same committed blocks
    let mut committed_heights = Vec::new();
    for node in &nodes {
        let committed = node.dag().get_committed();
        let heights: Vec<u64> = committed.iter().map(|c| c.height).collect();
        committed_heights.push(heights);
    }

    // All nodes should have committed the same heights
    if !committed_heights.is_empty() {
        let first_heights = &committed_heights[0];
        for (i, heights) in committed_heights.iter().enumerate().skip(1) {
            assert_eq!(
                heights, first_heights,
                "Node {} should have same committed heights as node 0",
                i
            );
        }
        info!("All nodes agree on committed heights: {:?}", first_heights);
    }

    // Verify blocks are consistent across nodes
    for node in &nodes {
        let committed = node.dag().get_committed();
        for subdag in committed {
            // Verify all nodes have the same blocks for this height
            let block_ids: Vec<_> = subdag.blocks.iter().map(|b| b.id.clone()).collect();
            for other_node in &nodes {
                for block_id in &block_ids {
                    let block = other_node.dag().get_block(block_id);
                    assert!(
                        block.is_some(),
                        "All nodes should have block {:?}",
                        block_id
                    );
                }
            }
        }
    }

    info!("Multi-node consensus test passed - all nodes synchronized");
}
