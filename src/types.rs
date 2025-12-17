use serde::{Deserialize, Serialize};

pub type AuthorityIndex = u32;
pub type RoundNumber = u64;
pub type Height = u64;
pub type Transaction = Vec<u8>;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct BlockId(pub [u8; 32]);

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct StateRoot(pub [u8; 32]);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DagBlock {
    pub id: BlockId,
    pub round: RoundNumber,
    pub author: AuthorityIndex,
    pub parents: Vec<BlockId>,
    pub transactions: Vec<Transaction>,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommittedSubDag {
    pub height: Height,
    pub anchor: BlockId,
    pub blocks: Vec<DagBlock>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct CheckpointCandidate {
    pub height: Height,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutedBlock {
    pub height: Height,
    pub dag_id: BlockId,
    pub state_root: StateRoot,
    pub transactions: Vec<Transaction>,
}
