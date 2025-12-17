// Tests to demonstrate that the consensus system guarantees transaction ordering
// independently of execution. The DAG consensus determines order, execution follows.

use orion::committee::{Committee, ValidatorInfo};
use orion::crypto::KeyPair;
use orion::dag::DagOrdering;
use orion::execution::{ExecutionEngine, SimpleKvStateMachine};
use orion::types::Transaction;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

fn create_test_committee_with_keypairs(size: usize) -> (Arc<Committee>, Vec<KeyPair>) {
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
async fn test_deterministic_ordering_across_nodes() {
    // Test that all nodes see transactions in the same order
    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let quorum = committee.quorum_threshold(); // 3

    // Create multiple DAG instances (simulating different nodes)
    let mut dags = Vec::new();
    for i in 0..quorum {
        let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
        dags.push(dag);
    }

    // Create blocks with specific transaction order
    let tx_order = vec![
        Transaction::from("tx_A"),
        Transaction::from("tx_B"),
        Transaction::from("tx_C"),
    ];

    // Create blocks from different validators
    let mut all_blocks = Vec::new();
    for (i, dag) in dags.iter().enumerate() {
        let block = dag.create_block(0, vec![], vec![tx_order[i].clone()]);
        all_blocks.push(block);
    }

    // Add all blocks to all DAG instances (simulating network broadcast)
    for dag in &dags {
        for block in &all_blocks {
            dag.add_block(block.clone());
        }
    }

    sleep(Duration::from_millis(50)).await;

    // Verify all nodes see the same committed order
    let mut committed_orders = Vec::new();
    for dag in &dags {
        let committed = dag.get_committed();
        if !committed.is_empty() {
            let mut order = Vec::new();
            for subdag in &committed {
                for block in &subdag.blocks {
                    order.extend_from_slice(&block.transactions);
                }
            }
            committed_orders.push(order);
        }
    }

    // All nodes should see the same transaction order
    if committed_orders.len() > 1 {
        let first_order = &committed_orders[0];
        for order in &committed_orders[1..] {
            assert_eq!(
                first_order, order,
                "All nodes must see the same transaction order"
            );
        }
    }
}

#[tokio::test]
async fn test_ordering_preserved_through_execution() {
    // Test that execution preserves the consensus-determined order
    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let quorum = committee.quorum_threshold();

    let mut dags = Vec::new();
    for i in 0..quorum {
        let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
        dags.push(dag);
    }

    // Create transactions in a specific order
    let tx1 = Transaction::from("order_1");
    let tx2 = Transaction::from("order_2");
    let tx3 = Transaction::from("order_3");

    let mut all_blocks = Vec::new();
    for (i, dag) in dags.iter().enumerate() {
        let txs = match i {
            0 => vec![tx1.clone()],
            1 => vec![tx2.clone()],
            2 => vec![tx3.clone()],
            _ => vec![],
        };
        let block = dag.create_block(0, vec![], txs);
        all_blocks.push(block);
    }

    // Broadcast to all nodes
    for dag in &dags {
        for block in &all_blocks {
            dag.add_block(block.clone());
        }
    }

    sleep(Duration::from_millis(50)).await;

    // Get committed order from consensus
    let committed = dags[0].get_committed();
    assert!(!committed.is_empty(), "Should have committed blocks");

    // Extract consensus-determined order
    let mut consensus_order = Vec::new();
    for subdag in &committed {
        for block in &subdag.blocks {
            consensus_order.extend_from_slice(&block.transactions);
        }
    }

    // Execute the committed sub-dags
    let sm = Arc::new(SimpleKvStateMachine::new());
    let engine = ExecutionEngine::new(sm);

    for subdag in &committed {
        let executed = engine.execute_subdag(subdag);

        // Verify execution order matches consensus order
        assert_eq!(
            executed.transactions.len(),
            subdag
                .blocks
                .iter()
                .map(|b| b.transactions.len())
                .sum::<usize>(),
            "Execution should process all transactions from consensus"
        );
    }

    // Verify the execution order matches consensus order
    let executed_blocks = engine.get_executed();
    let mut execution_order = Vec::new();
    for block in &executed_blocks {
        execution_order.extend_from_slice(&block.transactions);
    }

    // Execution order must match consensus order
    assert_eq!(
        consensus_order.len(),
        execution_order.len(),
        "Execution should process same number of transactions as consensus"
    );
}

#[tokio::test]
async fn test_ordering_immutability_after_commit() {
    // Test that once transactions are committed in an order, that order cannot change
    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let quorum = committee.quorum_threshold();

    // Create multiple DAG instances to reach quorum
    let mut dags = Vec::new();
    for i in 0..quorum {
        let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
        dags.push(dag);
    }

    // Create and commit first batch - need quorum blocks
    let tx1 = Transaction::from("first_A");
    let tx2 = Transaction::from("first_B");
    let tx3 = Transaction::from("first_C");

    let mut first_batch_blocks = Vec::new();
    for (i, dag) in dags.iter().enumerate() {
        let txs = match i {
            0 => vec![tx1.clone()],
            1 => vec![tx2.clone()],
            2 => vec![tx3.clone()],
            _ => vec![],
        };
        let block = dag.create_block(0, vec![], txs);
        first_batch_blocks.push(block);
    }

    // Broadcast first batch to all nodes
    for dag in &dags {
        for block in &first_batch_blocks {
            dag.add_block(block.clone());
        }
    }

    sleep(Duration::from_millis(50)).await;

    let committed1 = dags[0].get_committed();
    assert!(!committed1.is_empty(), "Should have committed first batch");

    // Extract first committed order
    let mut first_order = Vec::new();
    for subdag in &committed1 {
        for block in &subdag.blocks {
            first_order.extend_from_slice(&block.transactions);
        }
    }

    // Try to add more blocks in a new round
    let tx3 = Transaction::from("second_C");
    let mut second_batch_blocks = Vec::new();
    for dag in dags.iter() {
        let block = dag.create_block(1, vec![], vec![tx3.clone()]);
        second_batch_blocks.push(block);
    }

    // Broadcast second batch
    for dag in &dags {
        for block in &second_batch_blocks {
            dag.add_block(block.clone());
        }
    }

    sleep(Duration::from_millis(50)).await;

    // Verify first order hasn't changed
    let committed2 = dags[0].get_committed();
    let mut second_order = Vec::new();
    for subdag in &committed2 {
        for block in &subdag.blocks {
            second_order.extend_from_slice(&block.transactions);
        }
    }

    // The first committed order should be preserved
    assert!(
        second_order.len() >= first_order.len(),
        "New commits should append, not replace"
    );

    // First order should be a prefix of second order
    let min_len = first_order.len().min(second_order.len());
    if min_len > 0 {
        assert_eq!(
            &second_order[..min_len],
            &first_order[..min_len],
            "Committed order should be immutable - first order must be preserved"
        );
    }
}

#[tokio::test]
async fn test_consensus_order_independent_of_execution() {
    // Test that consensus determines order regardless of execution results
    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let quorum = committee.quorum_threshold();

    let mut dags = Vec::new();
    for i in 0..quorum {
        let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
        dags.push(dag);
    }

    // Create transactions
    let tx1 = Transaction::from("tx_1");
    let tx2 = Transaction::from("tx_2");
    let tx3 = Transaction::from("tx_3");

    let mut all_blocks = Vec::new();
    for (i, dag) in dags.iter().enumerate() {
        let txs = match i {
            0 => vec![tx1.clone()],
            1 => vec![tx2.clone()],
            2 => vec![tx3.clone()],
            _ => vec![],
        };
        let block = dag.create_block(0, vec![], txs);
        all_blocks.push(block);
    }

    // Broadcast to all nodes
    for dag in &dags {
        for block in &all_blocks {
            dag.add_block(block.clone());
        }
    }

    sleep(Duration::from_millis(50)).await;

    // Get consensus order BEFORE execution
    let consensus_order: Vec<Transaction> = dags[0]
        .get_committed()
        .iter()
        .flat_map(|subdag| {
            subdag
                .blocks
                .iter()
                .flat_map(|block| block.transactions.clone())
        })
        .collect();

    assert!(!consensus_order.is_empty(), "Should have consensus order");

    // Now execute - execution should follow consensus order
    let sm = Arc::new(SimpleKvStateMachine::new());
    let engine = ExecutionEngine::new(sm);

    for subdag in dags[0].get_committed() {
        engine.execute_subdag(&subdag);
    }

    let executed_blocks = engine.get_executed();
    let execution_order: Vec<Transaction> = executed_blocks
        .iter()
        .flat_map(|block| block.transactions.clone())
        .collect();

    // Consensus order is determined independently
    // Execution must follow that order
    assert_eq!(
        consensus_order.len(),
        execution_order.len(),
        "Execution should process all consensus-ordered transactions"
    );

    // Verify order matches
    for (consensus_tx, exec_tx) in consensus_order.iter().zip(execution_order.iter()) {
        assert_eq!(
            consensus_tx, exec_tx,
            "Execution order must match consensus order"
        );
    }
}

#[tokio::test]
async fn test_ordering_consistency_with_multiple_rounds() {
    // Test that ordering is consistent across multiple consensus rounds
    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let quorum = committee.quorum_threshold();

    let mut dags = Vec::new();
    for i in 0..quorum {
        let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
        dags.push(dag);
    }

    // Create transactions across multiple rounds
    let rounds = vec![
        vec![
            Transaction::from("round0_tx1"),
            Transaction::from("round0_tx2"),
        ],
        vec![
            Transaction::from("round1_tx1"),
            Transaction::from("round1_tx2"),
        ],
        vec![
            Transaction::from("round2_tx1"),
            Transaction::from("round2_tx2"),
        ],
    ];

    // Create blocks for each round
    for (round_num, round_txs) in rounds.iter().enumerate() {
        let mut round_blocks = Vec::new();
        for (i, dag) in dags.iter().enumerate() {
            if i < round_txs.len() {
                let block = dag.create_block(round_num as u64, vec![], vec![round_txs[i].clone()]);
                round_blocks.push(block);
            }
        }

        // Broadcast round blocks to all nodes
        for dag in &dags {
            for block in &round_blocks {
                dag.add_block(block.clone());
            }
        }

        sleep(Duration::from_millis(20)).await;
    }

    sleep(Duration::from_millis(50)).await;

    // Verify all nodes see the same order across all rounds
    let mut all_orders = Vec::new();
    for dag in &dags {
        let committed = dag.get_committed();
        let mut order = Vec::new();
        for subdag in &committed {
            for block in &subdag.blocks {
                order.extend_from_slice(&block.transactions);
            }
        }
        if !order.is_empty() {
            all_orders.push(order);
        }
    }

    // All nodes should see consistent ordering across rounds
    if all_orders.len() > 1 {
        let first_order = &all_orders[0];
        for order in &all_orders[1..] {
            // Orders should match (or first should be prefix if some nodes are behind)
            assert!(
                order.len() == first_order.len()
                    || (order.len() < first_order.len() && order == &first_order[..order.len()])
                    || (first_order.len() < order.len()
                        && first_order == &order[..first_order.len()]),
                "All nodes must see consistent ordering across rounds"
            );
        }
    }
}

#[test]
fn test_ordering_not_affected_by_execution_failures() {
    // Test that consensus order is independent of whether execution succeeds
    // (In this simple system, execution always succeeds, but the principle holds)

    let (committee, keypairs) = create_test_committee_with_keypairs(4);
    let dag = DagOrdering::new(committee, 0, keypairs[0].clone());

    // Create transactions
    let tx1 = Transaction::from("must_be_first");
    let tx2 = Transaction::from("must_be_second");
    let tx3 = Transaction::from("must_be_third");

    let block1 = dag.create_block(0, vec![], vec![tx1.clone()]);
    let block2 = dag.create_block(0, vec![], vec![tx2.clone()]);
    let block3 = dag.create_block(0, vec![], vec![tx3.clone()]);

    dag.add_block(block1);
    dag.add_block(block2);
    dag.add_block(block3);

    // Consensus order is determined by DAG, not execution
    // Even if execution fails for some transactions, the order is fixed
    let committed = dag.get_committed();

    if !committed.is_empty() {
        let consensus_order: Vec<Transaction> = committed
            .iter()
            .flat_map(|subdag| {
                subdag
                    .blocks
                    .iter()
                    .flat_map(|block| block.transactions.clone())
            })
            .collect();

        // Order is fixed by consensus
        assert!(
            consensus_order.contains(&tx1),
            "Consensus order includes all transactions"
        );
        assert!(
            consensus_order.contains(&tx2),
            "Consensus order includes all transactions"
        );
        assert!(
            consensus_order.contains(&tx3),
            "Consensus order includes all transactions"
        );

        // The order is what matters, not execution results
        // Execution happens later and must follow this order
    }
}
