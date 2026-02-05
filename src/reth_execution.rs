use crate::engine_api::{EngineApiClient, EngineApiError};
use crate::types::{CommittedSubDag, ExecutedBlock, StateRoot};
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::info;

/// Execution engine that delegates block building and EVM execution to Reth
/// via the Engine API. Replaces the local `SimpleKvStateMachine` for the
/// Orion-Reth integration.
pub struct RethExecutionEngine {
    engine_api: Arc<EngineApiClient>,
    /// The latest Reth block hash (parent for next block).
    latest_block_hash: Mutex<String>,
    /// Latest timestamp used; must be strictly increasing for Reth.
    latest_timestamp: Mutex<u64>,
    /// Fee recipient address for block rewards.
    fee_recipient: String,
    /// History of executed blocks.
    executed: Mutex<Vec<ExecutedBlock>>,
}

impl RethExecutionEngine {
    pub fn new(engine_api: Arc<EngineApiClient>, fee_recipient: String) -> Self {
        Self {
            engine_api,
            latest_block_hash: Mutex::new(String::new()),
            latest_timestamp: Mutex::new(0),
            fee_recipient,
            executed: Mutex::new(Vec::new()),
        }
    }

    /// Initialize by fetching the genesis block hash from Reth.
    /// Must be called before `execute_subdag`.
    pub async fn init(&self) -> Result<(), EngineApiError> {
        let genesis_hash = self.engine_api.get_genesis_block_hash().await?;
        info!(hash = %genesis_hash, "Fetched genesis block hash from Reth");
        *self.latest_block_hash.lock() = genesis_hash;
        Ok(())
    }

    /// Submit EVM transactions to Reth's transaction pool.
    /// Each transaction in the sub-DAG is treated as a raw tx hex string
    /// and sent via eth_sendRawTransaction.
    pub async fn submit_transactions(&self, txs: &[Vec<u8>]) -> Vec<Result<String, EngineApiError>> {
        let mut results = Vec::with_capacity(txs.len());
        for tx in txs {
            // Convert raw bytes to 0x-prefixed hex
            let tx_hex = format!("0x{}", hex::encode(tx));
            let result = self.engine_api.send_raw_transaction(&tx_hex).await;
            results.push(result);
        }
        results
    }

    /// Execute a committed sub-DAG by building a block in Reth.
    ///
    /// Flow:
    /// 1. (Transactions should already be in Reth's pool — submitted separately or
    ///    via eth_sendTransaction in the demo.)
    /// 2. Call forkchoiceUpdatedV3 with PayloadAttributes to trigger block building.
    /// 3. Call getPayloadV3 to get the built block.
    /// 4. Call newPayloadV3 to validate.
    /// 5. Call forkchoiceUpdatedV3 to finalize.
    pub async fn execute_subdag(
        &self,
        subdag: &CommittedSubDag,
    ) -> Result<ExecutedBlock, EngineApiError> {
        let parent_hash = self.latest_block_hash.lock().clone();

        // Ensure strictly increasing timestamps
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let timestamp = {
            let mut last = self.latest_timestamp.lock();
            let ts = std::cmp::max(now, *last + 1);
            *last = ts;
            ts
        };

        let prev_randao: [u8; 32] = rand::random();

        info!(
            height = subdag.height,
            parent_hash = %parent_hash,
            timestamp,
            blocks_in_subdag = subdag.blocks.len(),
            "Building Reth block for committed sub-DAG"
        );

        let (new_block_hash, _payload) = self
            .engine_api
            .build_and_finalize_block(&parent_hash, timestamp, prev_randao, &self.fee_recipient)
            .await?;

        info!(
            height = subdag.height,
            block_hash = %new_block_hash,
            "Reth block finalized"
        );

        // Update parent hash for next block
        *self.latest_block_hash.lock() = new_block_hash.clone();

        // Convert the Reth block hash into a StateRoot for Orion's type system
        let hash_bytes = hex::decode(new_block_hash.trim_start_matches("0x"))
            .unwrap_or_else(|_| vec![0u8; 32]);
        let mut state_root_bytes = [0u8; 32];
        let len = std::cmp::min(hash_bytes.len(), 32);
        state_root_bytes[..len].copy_from_slice(&hash_bytes[..len]);

        // Collect all transactions from the sub-DAG
        let all_txs: Vec<_> = subdag
            .blocks
            .iter()
            .flat_map(|b| b.transactions.clone())
            .collect();

        let executed = ExecutedBlock {
            height: subdag.height,
            dag_id: subdag.anchor.clone(),
            state_root: StateRoot(state_root_bytes),
            transactions: all_txs,
        };

        self.executed.lock().push(executed.clone());
        Ok(executed)
    }

    pub fn get_executed(&self) -> Vec<ExecutedBlock> {
        self.executed.lock().clone()
    }

    pub fn latest_block_hash(&self) -> String {
        self.latest_block_hash.lock().clone()
    }

    /// Convenience: get a reference to the inner Engine API client.
    pub fn engine_api(&self) -> &EngineApiClient {
        &self.engine_api
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_api::EngineApiConfig;
    use crate::types::{BlockId, DagBlock};

    fn create_test_subdag(height: u64, tx_count: usize) -> CommittedSubDag {
        let mut txs = Vec::new();
        for i in 0..tx_count {
            txs.push(format!("tx_{}", i).into_bytes());
        }
        let block = DagBlock {
            id: BlockId([height as u8; 32]),
            round: height,
            author: 0,
            parents: vec![],
            transactions: txs,
            signature: vec![],
        };
        CommittedSubDag {
            height,
            anchor: block.id.clone(),
            blocks: vec![block],
        }
    }

    #[test]
    fn test_reth_execution_engine_creation() {
        let config = EngineApiConfig {
            auth_endpoint: "http://localhost:8551".into(),
            rpc_endpoint: "http://localhost:8545".into(),
            jwt_secret_hex: "aa".repeat(32),
        };
        let client = Arc::new(EngineApiClient::new(config));
        let engine = RethExecutionEngine::new(
            client,
            "0x0000000000000000000000000000000000000000".into(),
        );

        assert!(engine.get_executed().is_empty());
        assert_eq!(engine.latest_block_hash(), "");
        assert_eq!(
            engine.fee_recipient,
            "0x0000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn test_latest_block_hash_updates() {
        let config = EngineApiConfig {
            auth_endpoint: "http://localhost:8551".into(),
            rpc_endpoint: "http://localhost:8545".into(),
            jwt_secret_hex: "aa".repeat(32),
        };
        let client = Arc::new(EngineApiClient::new(config));
        let engine = RethExecutionEngine::new(
            client,
            "0x0000000000000000000000000000000000000000".into(),
        );

        // Simulate setting block hash (as init would do)
        *engine.latest_block_hash.lock() = "0xdeadbeef".into();
        assert_eq!(engine.latest_block_hash(), "0xdeadbeef");
    }

    #[test]
    fn test_timestamp_strictly_increasing() {
        let config = EngineApiConfig {
            auth_endpoint: "http://localhost:8551".into(),
            rpc_endpoint: "http://localhost:8545".into(),
            jwt_secret_hex: "aa".repeat(32),
        };
        let client = Arc::new(EngineApiClient::new(config));
        let engine = RethExecutionEngine::new(
            client,
            "0x0000000000000000000000000000000000000000".into(),
        );

        // Simulate the timestamp logic used in execute_subdag
        let mut timestamps = Vec::new();
        for _ in 0..5 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let ts = {
                let mut last = engine.latest_timestamp.lock();
                let ts = std::cmp::max(now, *last + 1);
                *last = ts;
                ts
            };
            timestamps.push(ts);
        }

        // Verify strictly increasing
        for i in 1..timestamps.len() {
            assert!(
                timestamps[i] > timestamps[i - 1],
                "Timestamps must be strictly increasing: {} <= {}",
                timestamps[i],
                timestamps[i - 1]
            );
        }
    }

    #[test]
    fn test_subdag_transaction_collection() {
        // Test that execute_subdag would collect all txs from all blocks
        let subdag = CommittedSubDag {
            height: 1,
            anchor: BlockId([0u8; 32]),
            blocks: vec![
                DagBlock {
                    id: BlockId([0u8; 32]),
                    round: 1,
                    author: 0,
                    parents: vec![],
                    transactions: vec![b"tx_a".to_vec(), b"tx_b".to_vec()],
                    signature: vec![],
                },
                DagBlock {
                    id: BlockId([1u8; 32]),
                    round: 1,
                    author: 1,
                    parents: vec![],
                    transactions: vec![b"tx_c".to_vec()],
                    signature: vec![],
                },
            ],
        };

        let all_txs: Vec<_> = subdag
            .blocks
            .iter()
            .flat_map(|b| b.transactions.clone())
            .collect();

        assert_eq!(all_txs.len(), 3);
        assert_eq!(all_txs[0], b"tx_a");
        assert_eq!(all_txs[1], b"tx_b");
        assert_eq!(all_txs[2], b"tx_c");
    }

    #[test]
    fn test_block_hash_to_state_root_conversion() {
        // Test the hex-to-StateRoot conversion logic
        let block_hash = "0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3";
        let hash_bytes = hex::decode(block_hash.trim_start_matches("0x")).unwrap();

        let mut state_root_bytes = [0u8; 32];
        let len = std::cmp::min(hash_bytes.len(), 32);
        state_root_bytes[..len].copy_from_slice(&hash_bytes[..len]);

        let state_root = StateRoot(state_root_bytes);
        assert_eq!(state_root.0[0], 0xd4);
        assert_eq!(state_root.0[1], 0xe5);
        assert_eq!(state_root.0[31], 0xa3);
    }

    #[test]
    fn test_executed_blocks_accumulate() {
        let config = EngineApiConfig {
            auth_endpoint: "http://localhost:8551".into(),
            rpc_endpoint: "http://localhost:8545".into(),
            jwt_secret_hex: "aa".repeat(32),
        };
        let client = Arc::new(EngineApiClient::new(config));
        let engine = RethExecutionEngine::new(
            client,
            "0x0000000000000000000000000000000000000000".into(),
        );

        // Manually push executed blocks to simulate what execute_subdag does
        let block1 = ExecutedBlock {
            height: 1,
            dag_id: BlockId([1u8; 32]),
            state_root: StateRoot([0xaa; 32]),
            transactions: vec![b"tx1".to_vec()],
        };
        let block2 = ExecutedBlock {
            height: 2,
            dag_id: BlockId([2u8; 32]),
            state_root: StateRoot([0xbb; 32]),
            transactions: vec![b"tx2".to_vec(), b"tx3".to_vec()],
        };

        engine.executed.lock().push(block1);
        engine.executed.lock().push(block2);

        let executed = engine.get_executed();
        assert_eq!(executed.len(), 2);
        assert_eq!(executed[0].height, 1);
        assert_eq!(executed[1].height, 2);
        assert_eq!(executed[0].transactions.len(), 1);
        assert_eq!(executed[1].transactions.len(), 2);
    }

    #[test]
    fn test_create_test_subdag_helper() {
        let subdag = create_test_subdag(5, 3);
        assert_eq!(subdag.height, 5);
        assert_eq!(subdag.blocks.len(), 1);
        assert_eq!(subdag.blocks[0].transactions.len(), 3);
        assert_eq!(subdag.blocks[0].round, 5);
    }
}
