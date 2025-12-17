use crate::committee::Committee;
use crate::crypto::KeyPair;
use crate::network::DataPlane;
use crate::types::{AuthorityIndex, BlockId, CommittedSubDag, DagBlock, RoundNumber, Transaction};
use blake2::{Blake2s256, Digest};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

pub struct DagOrdering {
    committee: Arc<Committee>,
    authority: AuthorityIndex,
    keypair: KeyPair,
    pub block_store: Arc<Mutex<HashMap<BlockId, DagBlock>>>,
    round_blocks: Arc<Mutex<HashMap<RoundNumber, Vec<BlockId>>>>,
    committed: Arc<Mutex<Vec<CommittedSubDag>>>,
    commit_callback: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<CommittedSubDag>>>>,
    data_plane: Arc<Mutex<Option<Arc<dyn DataPlane>>>>,
}

impl DagOrdering {
    pub fn new(committee: Arc<Committee>, authority: AuthorityIndex, keypair: KeyPair) -> Self {
        Self {
            committee,
            authority,
            keypair,
            block_store: Arc::new(Mutex::new(HashMap::new())),
            round_blocks: Arc::new(Mutex::new(HashMap::new())),
            committed: Arc::new(Mutex::new(Vec::new())),
            commit_callback: Arc::new(Mutex::new(None)),
            data_plane: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_data_plane(&self, data_plane: Arc<dyn DataPlane>) {
        *self.data_plane.lock() = Some(data_plane);
    }

    pub fn set_commit_callback(&self, sender: tokio::sync::mpsc::UnboundedSender<CommittedSubDag>) {
        *self.commit_callback.lock() = Some(sender);
    }

    pub fn create_block(
        &self,
        round: RoundNumber,
        parents: Vec<BlockId>,
        transactions: Vec<Transaction>,
    ) -> DagBlock {
        let mut hasher = Blake2s256::new();
        hasher.update(&round.to_le_bytes());
        hasher.update(&self.authority.to_le_bytes());
        for parent in &parents {
            hasher.update(&parent.0);
        }
        for tx in &transactions {
            hasher.update(tx);
        }
        let id_bytes = hasher.finalize().into();
        let block_id = BlockId(id_bytes);

        let mut message = Vec::new();
        message.extend_from_slice(&block_id.0);
        message.extend_from_slice(&round.to_le_bytes());
        let signature = self.keypair.sign(&message);

        let block = DagBlock {
            id: block_id,
            round,
            author: self.authority,
            parents,
            transactions,
            signature: signature.to_bytes().to_vec(),
        };

        // Broadcast block via Raptorcast
        if let Some(ref data_plane) = *self.data_plane.lock() {
            let block_data = bincode::serialize(&block).unwrap_or_default();
            if let Err(e) = data_plane.broadcast(block_data) {
                warn!(error = %e, "Failed to broadcast block");
            }
        }

        block
    }

    pub fn add_block(&self, block: DagBlock) -> bool {
        // Verify signature
        if let Some(validator) = self.committee.get(block.author) {
            let mut message = Vec::new();
            message.extend_from_slice(&block.id.0);
            message.extend_from_slice(&block.round.to_le_bytes());
            let sig_bytes: [u8; 64] = block.signature.clone().try_into().unwrap_or([0; 64]);
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            if !KeyPair::verify(&validator.public_key, &message, &sig) {
                warn!(author = block.author, "Invalid block signature");
                return false;
            }
        }

        let mut store = self.block_store.lock();
        if store.contains_key(&block.id) {
            return false;
        }

        store.insert(block.id.clone(), block.clone());
        drop(store);

        let mut rounds = self.round_blocks.lock();
        rounds
            .entry(block.round)
            .or_insert_with(Vec::new)
            .push(block.id.clone());
        drop(rounds);

        // Try to commit
        self.try_commit(block.round);

        true
    }

    fn try_commit(&self, round: RoundNumber) {
        let rounds = self.round_blocks.lock();
        let blocks = rounds.get(&round).cloned();
        drop(rounds);

        if let Some(block_ids) = blocks {
            // Simple commit rule: commit when we have quorum of blocks in a round
            let quorum = self.committee.quorum_threshold();
            if block_ids.len() >= quorum {
                let store = self.block_store.lock();
                let mut committed_blocks = Vec::new();
                for id in &block_ids {
                    if let Some(block) = store.get(id) {
                        committed_blocks.push(block.clone());
                    }
                }
                drop(store);

                if !committed_blocks.is_empty() {
                    let anchor = committed_blocks[0].id.clone();
                    let height = {
                        let committed = self.committed.lock();
                        committed.len() as u64 + 1
                    };

                    let subdag = CommittedSubDag {
                        height,
                        anchor,
                        blocks: committed_blocks,
                    };

                    {
                        let mut committed = self.committed.lock();
                        committed.push(subdag.clone());
                    }

                    info!(
                        height = subdag.height,
                        blocks = subdag.blocks.len(),
                        "Committed sub-dag"
                    );

                    if let Some(ref sender) = *self.commit_callback.lock() {
                        let _ = sender.send(subdag);
                    }
                }
            }
        }
    }

    pub fn get_block(&self, id: &BlockId) -> Option<DagBlock> {
        self.block_store.lock().get(id).cloned()
    }

    pub fn get_committed(&self) -> Vec<CommittedSubDag> {
        self.committed.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::committee::ValidatorInfo;
    use crate::crypto::KeyPair;

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

    #[test]
    fn test_block_creation() {
        let committee = create_test_committee(4);
        let keypair = KeyPair::generate();
        let dag = DagOrdering::new(committee, 0, keypair);

        let block = dag.create_block(0, vec![], vec![b"tx1".to_vec()]);

        assert_eq!(block.round, 0);
        assert_eq!(block.author, 0);
        assert_eq!(block.transactions.len(), 1);
        assert!(!block.signature.is_empty());
    }

    #[test]
    fn test_block_add_and_retrieve() {
        // Create committee with known keypairs
        let mut validators = Vec::new();
        let keypair = KeyPair::generate();
        validators.push(ValidatorInfo {
            index: 0,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
        for i in 1..4 {
            let kp = KeyPair::generate();
            validators.push(ValidatorInfo {
                index: i as u32,
                public_key: kp.verifying_key.clone(),
                stake: 1,
            });
        }
        let committee = Arc::new(Committee::new(validators));
        let dag = DagOrdering::new(committee, 0, keypair);

        let block = dag.create_block(0, vec![], vec![b"tx1".to_vec()]);
        let block_id = block.id.clone();

        assert!(dag.add_block(block));

        let retrieved = dag.get_block(&block_id);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().transactions.len(), 1);
    }

    #[test]
    fn test_duplicate_block_rejection() {
        // Create committee with known keypair
        let mut validators = Vec::new();
        let keypair = KeyPair::generate();
        validators.push(ValidatorInfo {
            index: 0,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
        for i in 1..4 {
            let kp = KeyPair::generate();
            validators.push(ValidatorInfo {
                index: i as u32,
                public_key: kp.verifying_key.clone(),
                stake: 1,
            });
        }
        let committee = Arc::new(Committee::new(validators));
        let dag = DagOrdering::new(committee, 0, keypair);

        let block = dag.create_block(0, vec![], vec![b"tx1".to_vec()]);

        assert!(dag.add_block(block.clone()));
        assert!(!dag.add_block(block)); // Duplicate should be rejected
    }

    #[tokio::test]
    async fn test_commit_with_quorum() {
        let quorum = 3; // For 4 validators, quorum is 3

        // Create committee with known keypairs that match the DAG instances
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

        // Create multiple DAG instances for different validators with matching keypairs
        let mut dags = Vec::new();
        for i in 0..quorum {
            let dag = DagOrdering::new(committee.clone(), i as u32, keypairs[i].clone());
            dags.push(dag);
        }

        // Create blocks from quorum validators and add them to ALL DAG instances
        // (simulating network broadcast where all nodes see all blocks)
        let mut all_blocks = Vec::new();
        for (i, dag) in dags.iter().enumerate() {
            let block = dag.create_block(0, vec![], vec![format!("tx{}", i).into_bytes()]);
            all_blocks.push(block);
        }

        // Add all blocks to all DAG instances (simulating network)
        for dag in &dags {
            for block in &all_blocks {
                dag.add_block(block.clone());
            }
        }

        // Give a moment for async processing
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Check that at least one DAG has committed
        let mut committed = false;
        for dag in &dags {
            if !dag.get_committed().is_empty() {
                committed = true;
                break;
            }
        }
        assert!(committed, "Should have committed with quorum");
    }

    #[test]
    fn test_block_with_parents() {
        // Create committee with known keypair
        let mut validators = Vec::new();
        let keypair = KeyPair::generate();
        validators.push(ValidatorInfo {
            index: 0,
            public_key: keypair.verifying_key.clone(),
            stake: 1,
        });
        for i in 1..4 {
            let kp = KeyPair::generate();
            validators.push(ValidatorInfo {
                index: i as u32,
                public_key: kp.verifying_key.clone(),
                stake: 1,
            });
        }
        let committee = Arc::new(Committee::new(validators));
        let dag = DagOrdering::new(committee, 0, keypair);

        let parent_block = dag.create_block(0, vec![], vec![b"tx1".to_vec()]);
        let parent_id = parent_block.id.clone();
        dag.add_block(parent_block);

        let child_block = dag.create_block(1, vec![parent_id.clone()], vec![b"tx2".to_vec()]);
        assert_eq!(child_block.parents.len(), 1);
        assert_eq!(child_block.parents[0], parent_id);
    }
}
