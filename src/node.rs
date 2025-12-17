use crate::bft::{BftCheckpointEngine, CheckpointVote};
use crate::committee::Committee;
use crate::crypto::KeyPair;
use crate::dag::DagOrdering;
use crate::execution::{ExecutionEngine, SimpleKvStateMachine};
use crate::network::DataPlane;
use crate::raptorcast::{Raptorcast, RaptorcastConfig};
use crate::types::{CommittedSubDag, DagBlock};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct HybridNode {
    dag: Arc<DagOrdering>,
    bft: Arc<BftCheckpointEngine>,
    execution: Arc<ExecutionEngine>,
    #[allow(dead_code)]
    raptorcast: Arc<Raptorcast>,
    data_plane: Arc<dyn DataPlane>,
}

impl HybridNode {
    /// Create a new node with a custom data plane (for testing with shared network)
    pub fn new_with_data_plane(
        committee: Arc<Committee>,
        authority: crate::types::AuthorityIndex,
        keypair: KeyPair,
        checkpoint_interval: crate::types::Height,
        data_plane: Arc<dyn DataPlane>,
    ) -> Self {
        let dag = Arc::new(DagOrdering::new(
            committee.clone(),
            authority,
            keypair.clone(),
        ));
        dag.set_data_plane(data_plane.clone());

        let bft = Arc::new(BftCheckpointEngine::new(
            committee.clone(),
            authority,
            keypair,
            checkpoint_interval,
        ));
        bft.set_data_plane(data_plane.clone());

        let state_machine = Arc::new(SimpleKvStateMachine::new());
        let execution = Arc::new(ExecutionEngine::new(state_machine));

        // Create a dummy raptorcast for compatibility (not used when using custom data plane)
        let config = RaptorcastConfig::default();
        let raptorcast = Arc::new(Raptorcast::new(config));

        Self {
            dag,
            bft,
            execution,
            raptorcast,
            data_plane,
        }
    }
}

impl HybridNode {
    pub fn new(
        committee: Arc<Committee>,
        authority: crate::types::AuthorityIndex,
        keypair: KeyPair,
        checkpoint_interval: crate::types::Height,
    ) -> Self {
        let data_plane: Arc<dyn DataPlane> = Arc::new(crate::network::InMemoryDataPlane::new());

        let dag = Arc::new(DagOrdering::new(
            committee.clone(),
            authority,
            keypair.clone(),
        ));
        dag.set_data_plane(data_plane.clone());

        let bft = Arc::new(BftCheckpointEngine::new(
            committee.clone(),
            authority,
            keypair,
            checkpoint_interval,
        ));
        bft.set_data_plane(data_plane.clone());

        let state_machine = Arc::new(SimpleKvStateMachine::new());
        let execution = Arc::new(ExecutionEngine::new(state_machine));

        let config = RaptorcastConfig::default();
        let raptorcast = Arc::new(Raptorcast::new(config));

        Self {
            dag,
            bft,
            execution,
            raptorcast,
            data_plane,
        }
    }

    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let dag = self.dag.clone();
        let bft = self.bft.clone();
        let execution = self.execution.clone();
        let data_plane = self.data_plane.clone();
        let (dag_sender, mut dag_receiver) = mpsc::unbounded_channel::<CommittedSubDag>();
        dag.set_commit_callback(dag_sender);

        let (vote_sender, mut vote_receiver) = mpsc::unbounded_channel::<CheckpointVote>();
        bft.set_vote_callback(vote_sender);

        let (exec_sender, mut exec_receiver) = mpsc::unbounded_channel::<CommittedSubDag>();

        let mut network_receiver = data_plane.subscribe();
        let dag_clone = dag.clone();
        let bft_clone = bft.clone();

        tokio::spawn(async move {
            while let Some(message) = network_receiver.recv().await {
                // Try to deserialize as block first
                if let Ok(block) = bincode::deserialize::<DagBlock>(&message) {
                    // Verify and add block
                    if dag_clone.add_block(block) {
                        info!("Received and processed block from network");
                    }
                } else if let Ok(vote) = bincode::deserialize::<CheckpointVote>(&message) {
                    // Process vote and log when a QC is formed.
                    if let Some(qc) = bft_clone.process_vote(vote) {
                        info!(
                            height = qc.candidate.height,
                            phase = ?qc.phase,
                            "Formed QC from network vote"
                        );
                    }
                } else {
                    warn!("Failed to deserialize network message");
                }
            }
        });

        let bft_clone = bft.clone();
        let exec_sender_clone = exec_sender.clone();
        tokio::spawn(async move {
            while let Some(subdag) = dag_receiver.recv().await {
                info!(
                    height = subdag.height,
                    "DAG committed sub-dag - ordering finalized"
                );

                if let Err(e) = exec_sender_clone.send(subdag.clone()) {
                    warn!(error = ?e, "Failed to send sub-dag to execution pipeline");
                }

                if let Some(candidate) = bft_clone.derive_candidate(subdag.height) {
                    if let Some(qc) =
                        bft_clone.process_checkpoint_on_ordered_sequence(candidate.clone())
                    {
                        info!(
                            height = qc.candidate.height,
                            phase = ?qc.phase,
                            "BFT checkpoint QC formed on ordered sequence"
                        );
                    }
                }

                while let Ok(vote) = vote_receiver.try_recv() {
                    let _ = bft_clone.process_vote(vote);
                }
            }
        });

        let execution_clone = execution.clone();
        tokio::spawn(async move {
            let mut last_executed_height = 0u64;

            while let Some(subdag) = exec_receiver.recv().await {
                info!(
                    height = subdag.height,
                    "Executing committed sub-dag asynchronously from BFT"
                );

                // Ensure we execute each height at most once, even if duplicates are received.
                if subdag.height > last_executed_height {
                    let executed = execution_clone.execute_subdag(&subdag);
                    info!(height = executed.height, "Executed committed sub-dag");
                    last_executed_height = executed.height;
                }
            }
        })
    }

    pub fn dag(&self) -> &Arc<DagOrdering> {
        &self.dag
    }

    pub fn bft(&self) -> &Arc<BftCheckpointEngine> {
        &self.bft
    }

    pub fn execution(&self) -> &Arc<ExecutionEngine> {
        &self.execution
    }
}
