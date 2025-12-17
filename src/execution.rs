use crate::types::{CommittedSubDag, ExecutedBlock, StateRoot, Transaction};
use blake2::{Blake2s256, Digest};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

pub trait StateMachine: Send + Sync {
    fn apply(&self, height: u64, transactions: Vec<Transaction>) -> StateRoot;
}

pub struct SimpleKvStateMachine {
    state: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>,
}

impl SimpleKvStateMachine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl StateMachine for SimpleKvStateMachine {
    fn apply(&self, _height: u64, transactions: Vec<Transaction>) -> StateRoot {
        let mut state = self.state.lock();
        for tx in transactions {
            // Simple interpretation: first byte is op, rest is key\0value
            if tx.len() > 2 && tx[0] == 0 {
                if let Some(pos) = tx[1..].iter().position(|&b| b == 0) {
                    let key = tx[1..pos + 1].to_vec();
                    let value = tx[pos + 2..].to_vec();
                    state.insert(key, value);
                }
            }
        }

        // Compute state root
        let mut hasher = Blake2s256::new();
        let mut entries: Vec<_> = state.iter().collect();
        entries.sort_by_key(|(k, _)| *k);
        for (k, v) in entries {
            hasher.update(k);
            hasher.update(v);
        }
        StateRoot(hasher.finalize().into())
    }
}

pub struct ExecutionEngine {
    state_machine: Arc<dyn StateMachine>,
    executed: Arc<Mutex<Vec<ExecutedBlock>>>,
}

impl ExecutionEngine {
    pub fn new(state_machine: Arc<dyn StateMachine>) -> Self {
        Self {
            state_machine,
            executed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn execute_subdag(&self, subdag: &CommittedSubDag) -> ExecutedBlock {
        let mut all_txs = Vec::new();
        for block in &subdag.blocks {
            all_txs.extend_from_slice(&block.transactions);
        }

        let state_root = self.state_machine.apply(subdag.height, all_txs.clone());

        let executed = ExecutedBlock {
            height: subdag.height,
            dag_id: subdag.anchor.clone(),
            state_root,
            transactions: all_txs,
        };

        {
            let mut executed_list = self.executed.lock();
            executed_list.push(executed.clone());
        }

        info!(height = executed.height, "Executed sub-dag");

        executed
    }

    pub fn get_executed(&self) -> Vec<ExecutedBlock> {
        self.executed.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BlockId, DagBlock};

    fn create_test_subdag(height: u64, transactions: Vec<Transaction>) -> CommittedSubDag {
        let mut blocks = Vec::new();
        for (i, txs) in transactions.chunks(2).enumerate() {
            blocks.push(DagBlock {
                id: BlockId([i as u8; 32]),
                round: height,
                author: 0,
                parents: vec![],
                transactions: txs.to_vec(),
                signature: vec![],
            });
        }

        CommittedSubDag {
            height,
            anchor: blocks[0].id.clone(),
            blocks,
        }
    }

    #[test]
    fn test_state_machine_apply() {
        let sm = SimpleKvStateMachine::new();

        let mut tx1 = vec![0u8]; // op code
        tx1.extend_from_slice(b"key1");
        tx1.push(0);
        tx1.extend_from_slice(b"value1");

        let state_root = sm.apply(1, vec![tx1]);

        // State root should be deterministic
        assert_ne!(state_root.0, [0; 32]);
    }

    #[test]
    fn test_execution_engine() {
        let sm = Arc::new(SimpleKvStateMachine::new());
        let engine = ExecutionEngine::new(sm);

        let mut tx1 = vec![0u8];
        tx1.extend_from_slice(b"key1");
        tx1.push(0);
        tx1.extend_from_slice(b"value1");

        let subdag = create_test_subdag(1, vec![tx1]);
        let executed = engine.execute_subdag(&subdag);

        assert_eq!(executed.height, 1);
        assert_eq!(executed.transactions.len(), 1);
        assert_ne!(executed.state_root.0, [0; 32]);
    }

    #[test]
    fn test_multiple_executions() {
        let sm = Arc::new(SimpleKvStateMachine::new());
        let engine = ExecutionEngine::new(sm);

        let mut tx1 = vec![0u8];
        tx1.extend_from_slice(b"key1");
        tx1.push(0);
        tx1.extend_from_slice(b"value1");

        let mut tx2 = vec![0u8];
        tx2.extend_from_slice(b"key2");
        tx2.push(0);
        tx2.extend_from_slice(b"value2");

        let subdag1 = create_test_subdag(1, vec![tx1]);
        let subdag2 = create_test_subdag(2, vec![tx2]);

        engine.execute_subdag(&subdag1);
        engine.execute_subdag(&subdag2);

        let executed = engine.get_executed();
        assert_eq!(executed.len(), 2);
        assert_eq!(executed[0].height, 1);
        assert_eq!(executed[1].height, 2);
    }

    #[test]
    fn test_state_root_determinism() {
        let sm1 = Arc::new(SimpleKvStateMachine::new());
        let sm2 = Arc::new(SimpleKvStateMachine::new());

        let mut tx = vec![0u8];
        tx.extend_from_slice(b"key");
        tx.push(0);
        tx.extend_from_slice(b"value");

        let root1 = sm1.apply(1, vec![tx.clone()]);
        let root2 = sm2.apply(1, vec![tx]);

        // Same transactions should produce same state root
        assert_eq!(root1.0, root2.0);
    }
}
