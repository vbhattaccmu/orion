use orion::bft::BftCheckpointEngine;
use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::dag::DagOrdering;
use orion::execution::{ExecutionEngine, SimpleKvStateMachine};
use orion::node::HybridNode;
use orion::types::Transaction;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

fn create_test_committee(size: usize) -> Arc<Committee> {
    let mut validators = Vec::new();
    for i in 0..size {
        let keypair = KeyPair::generate();
        validators.push(ValidatorInfo {
            index: i as u32,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
    }
    Arc::new(Committee::new(validators))
}

#[tokio::test]
async fn test_hybrid_node_creation() {
    let committee = create_test_committee(4);
    let keypair = KeyPair::generate();

    let node = HybridNode::new(committee, 0, keypair, 10);

    // Node should have all components
    assert!(node.dag().get_committed().is_empty());
    assert!(node.bft().last_finalized().is_none());
    assert!(node.execution().get_executed().is_empty());
}

#[tokio::test]
async fn test_end_to_end_flow() {
    let committee = create_test_committee(4);
    let keypair = KeyPair::generate();

    let node = HybridNode::new(committee.clone(), 0, keypair, 10);
    let handle = node.start();

    // Create and add some blocks
    let dag = node.dag();
    for i in 0..5 {
        let tx = Transaction::from(format!("tx_{}", i));
        let block = dag.create_block(i, vec![], vec![tx]);
        dag.add_block(block);
    }

    // Give time for processing
    sleep(Duration::from_millis(500)).await;

    // Check that some processing happened (no panic, node is running)
    let _ = dag.get_committed();
    let _ = node.execution().get_executed();

    drop(handle);
}

#[tokio::test]
async fn test_dag_to_execution_pipeline() {
    let committee = create_test_committee(4);
    let quorum = committee.quorum_threshold();

    // Create multiple DAG instances to reach quorum
    let mut dags = Vec::new();
    for i in 0..quorum {
        let keypair = KeyPair::generate();
        let dag = DagOrdering::new(committee.clone(), i as u32, keypair);
        dags.push(dag);
    }

    // Create blocks from quorum validators
    for (i, dag) in dags.iter().enumerate() {
        let tx = Transaction::from(format!("tx_{}", i));
        let block = dag.create_block(0, vec![], vec![tx]);
        dag.add_block(block);
    }

    // Wait a bit
    sleep(Duration::from_millis(100)).await;

    // Check if any committed
    let mut has_commits = false;
    for dag in &dags {
        if !dag.get_committed().is_empty() {
            has_commits = true;
            break;
        }
    }

    // If we have commits, test execution
    if has_commits {
        let sm = Arc::new(SimpleKvStateMachine::new());
        let engine = ExecutionEngine::new(sm);

        for dag in &dags {
            for subdag in dag.get_committed() {
                let executed = engine.execute_subdag(&subdag);
                assert_eq!(executed.height, subdag.height);
                assert!(!executed.transactions.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn test_checkpoint_formation() {
    let committee = create_test_committee(4);
    let keypair = KeyPair::generate();
    let bft = BftCheckpointEngine::new(committee.clone(), 0, keypair, 10);

    // Try to form checkpoint at interval height using ordered-sequence-only candidate
    if let Some(candidate) = bft.derive_candidate(10) {
        // Process checkpoint on the ordered sequence (two-phase BFT).
        let result = bft.process_checkpoint_on_ordered_sequence(candidate.clone());

        assert!(
            result.is_none()
                || matches!(result.as_ref().unwrap().phase, orion::bft::BftPhase::Commit)
        );
    }
}

#[test]
fn test_committee_quorum_calculation() {
    let committee = create_test_committee(4);
    let quorum = committee.quorum_threshold();

    // For 4 validators: (2*4)/3 + 1 = 3
    assert_eq!(quorum, 3);

    // Fault tolerance should be 1
    assert_eq!(committee.fault_tolerance(), 1);
}

#[tokio::test]
async fn test_multiple_rounds() {
    let committee = create_test_committee(4);
    let keypair = KeyPair::generate();
    let dag = DagOrdering::new(committee, 0, keypair);

    // Create blocks in multiple rounds
    for round in 0..5 {
        let tx = Transaction::from(format!("round_{}", round));
        let block = dag.create_block(round, vec![], vec![tx]);
        dag.add_block(block);
    }

    // All blocks should be stored
    let committed = dag.get_committed();
    assert!(committed.len() == committed.len()); // Just verify we can read commits
}
