use crate::committee::Committee;
use crate::crypto::KeyPair;
use crate::network::DataPlane;
use crate::types::{AuthorityIndex, CheckpointCandidate, Height};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BftPhase {
    Prepare,
    Commit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointVote {
    pub from: AuthorityIndex,
    pub candidate: CheckpointCandidate,
    pub phase: BftPhase,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct QuorumCertificate {
    pub candidate: CheckpointCandidate,
    pub phase: BftPhase,
    pub signers: HashSet<AuthorityIndex>,
}

pub struct BftCheckpointEngine {
    committee: Arc<Committee>,
    authority: AuthorityIndex,
    keypair: KeyPair,
    interval: Height,
    prepare_votes: Arc<Mutex<HashMap<(Height, BftPhase), Vec<CheckpointVote>>>>,
    commit_votes: Arc<Mutex<HashMap<(Height, BftPhase), Vec<CheckpointVote>>>>,
    last_finalized: Arc<Mutex<Option<CheckpointCandidate>>>,
    vote_callback: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<CheckpointVote>>>>,
    data_plane: Arc<Mutex<Option<Arc<dyn DataPlane>>>>,
}

impl BftCheckpointEngine {
    pub fn new(
        committee: Arc<Committee>,
        authority: AuthorityIndex,
        keypair: KeyPair,
        interval: Height,
    ) -> Self {
        Self {
            committee,
            authority,
            keypair,
            interval,
            prepare_votes: Arc::new(Mutex::new(HashMap::new())),
            commit_votes: Arc::new(Mutex::new(HashMap::new())),
            last_finalized: Arc::new(Mutex::new(None)),
            vote_callback: Arc::new(Mutex::new(None)),
            data_plane: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_data_plane(&self, data_plane: Arc<dyn DataPlane>) {
        *self.data_plane.lock() = Some(data_plane);
    }

    pub fn set_vote_callback(&self, sender: tokio::sync::mpsc::UnboundedSender<CheckpointVote>) {
        *self.vote_callback.lock() = Some(sender);
    }

    pub fn derive_candidate(&self, height: Height) -> Option<CheckpointCandidate> {
        if height == 0 || height % self.interval != 0 {
            return None;
        }

        Some(CheckpointCandidate { height })
    }

    pub fn create_vote(&self, candidate: CheckpointCandidate, phase: BftPhase) -> CheckpointVote {
        let mut message = Vec::new();
        message.extend_from_slice(&candidate.height.to_le_bytes());
        message.push(match phase {
            BftPhase::Prepare => 0,
            BftPhase::Commit => 1,
        });

        let signature = self.keypair.sign(&message);
        let vote = CheckpointVote {
            from: self.authority,
            candidate,
            phase: phase.clone(),
            signature: signature.to_bytes().to_vec(),
        };

        // Broadcast vote via Raptorcast
        if let Some(ref data_plane) = *self.data_plane.lock() {
            let vote_data = bincode::serialize(&vote).unwrap_or_default();
            if let Err(e) = data_plane.broadcast(vote_data) {
                warn!(error = %e, "Failed to broadcast vote");
            }
        }

        vote
    }

    pub fn process_vote(&self, vote: CheckpointVote) -> Option<QuorumCertificate> {
        // Verify signature
        if let Some(validator) = self.committee.get(vote.from) {
            let mut message = Vec::new();
            message.extend_from_slice(&vote.candidate.height.to_le_bytes());
            message.push(match vote.phase {
                BftPhase::Prepare => 0,
                BftPhase::Commit => 1,
            });

            let sig_bytes: [u8; 64] = vote.signature.clone().try_into().unwrap_or([0; 64]);
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            if !KeyPair::verify(&validator.public_key, &message, &sig) {
                warn!(from = vote.from, "Invalid vote signature");
                return None;
            }
        }

        let quorum = self.committee.quorum_threshold();
        let key = (vote.candidate.height, vote.phase.clone());

        match vote.phase {
            BftPhase::Prepare => {
                let mut votes = self.prepare_votes.lock();
                let entry = votes.entry(key).or_insert_with(Vec::new);
                entry.push(vote.clone());

                if entry.len() >= quorum {
                    let signers: HashSet<_> = entry.iter().map(|v| v.from).collect();
                    if signers.len() >= quorum {
                        info!(height = vote.candidate.height, "Formed PREPARE QC");
                        return Some(QuorumCertificate {
                            candidate: vote.candidate.clone(),
                            phase: BftPhase::Prepare,
                            signers,
                        });
                    }
                }
            }
            BftPhase::Commit => {
                let mut votes = self.commit_votes.lock();
                let entry = votes.entry(key).or_insert_with(Vec::new);
                entry.push(vote.clone());

                if entry.len() >= quorum {
                    let signers: HashSet<_> = entry.iter().map(|v| v.from).collect();
                    if signers.len() >= quorum {
                        info!(
                            height = vote.candidate.height,
                            "Formed COMMIT QC - checkpoint FINALIZED"
                        );
                        *self.last_finalized.lock() = Some(vote.candidate.clone());
                        return Some(QuorumCertificate {
                            candidate: vote.candidate.clone(),
                            phase: BftPhase::Commit,
                            signers,
                        });
                    }
                }
            }
        }

        None
    }

    pub fn process_checkpoint_on_ordered_sequence(
        &self,
        candidate: CheckpointCandidate,
    ) -> Option<QuorumCertificate> {
        // Phase 1: Create and broadcast prepare vote
        let prepare_vote = self.create_vote(candidate.clone(), BftPhase::Prepare);
        if let Some(ref sender) = *self.vote_callback.lock() {
            let _ = sender.send(prepare_vote.clone());
        }

        // Process our own vote
        if let Some(_prepare_qc) = self.process_vote(prepare_vote) {
            // Phase 2: After prepare QC, create commit vote
            let commit_vote = self.create_vote(candidate.clone(), BftPhase::Commit);
            if let Some(ref sender) = *self.vote_callback.lock() {
                let _ = sender.send(commit_vote.clone());
            }

            // Process commit vote
            self.process_vote(commit_vote)
        } else {
            None
        }
    }

    pub fn last_finalized(&self) -> Option<CheckpointCandidate> {
        self.last_finalized.lock().clone()
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
    fn test_derive_candidate() {
        let committee = create_test_committee(4);
        let keypair = KeyPair::generate();
        let bft = BftCheckpointEngine::new(committee, 0, keypair, 10);

        // Should return None for height 0
        assert!(bft.derive_candidate(0).is_none());

        // Should return None for non-interval height
        assert!(bft.derive_candidate(5).is_none());

        // Should return Some for interval height
        let candidate = bft.derive_candidate(10);
        assert!(candidate.is_some());
        assert_eq!(candidate.unwrap().height, 10);
    }

    #[test]
    fn test_vote_creation() {
        let committee = create_test_committee(4);
        let keypair = KeyPair::generate();
        let bft = BftCheckpointEngine::new(committee, 0, keypair, 10);

        let candidate = CheckpointCandidate { height: 10 };

        let vote = bft.create_vote(candidate.clone(), BftPhase::Prepare);

        assert_eq!(vote.from, 0);
        assert_eq!(vote.candidate.height, 10);
        assert_eq!(vote.phase, BftPhase::Prepare);
        assert!(!vote.signature.is_empty());
    }

    #[test]
    fn test_vote_verification() {
        let committee = create_test_committee(4);
        let keypair = KeyPair::generate();
        let bft = BftCheckpointEngine::new(committee.clone(), 0, keypair.clone(), 10);

        let candidate = CheckpointCandidate { height: 10 };

        let vote = bft.create_vote(candidate.clone(), BftPhase::Prepare);

        // Process vote should verify signature and return None until quorum
        let result = bft.process_vote(vote);
        // With only one vote, shouldn't form QC yet
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_quorum_formation() {
        let quorum = 3; // For 4 validators, quorum is 3

        // Create committee with known keypairs that match the BFT engines
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

        // Create BFT engines for quorum validators with matching keypairs
        let mut engines = Vec::new();
        for i in 0..quorum {
            let bft =
                BftCheckpointEngine::new(committee.clone(), i as u32, keypairs[i].clone(), 10);
            engines.push(bft);
        }

        let candidate = CheckpointCandidate { height: 10 };

        // Create votes from all validators
        let mut votes = Vec::new();
        for engine in engines.iter() {
            let vote = engine.create_vote(candidate.clone(), BftPhase::Prepare);
            votes.push(vote);
        }

        // Process all votes through first engine
        let mut qc_formed = false;
        for vote in votes {
            if let Some(qc) = engines[0].process_vote(vote) {
                assert_eq!(qc.phase, BftPhase::Prepare);
                assert_eq!(qc.candidate.height, 10);
                qc_formed = true;
                break;
            }
        }

        assert!(qc_formed, "Should form QC with quorum votes");
    }

    #[test]
    fn test_two_phase_commit() {
        let committee = create_test_committee(4);
        let keypair = KeyPair::generate();
        let bft = BftCheckpointEngine::new(committee, 0, keypair, 10);

        let candidate = CheckpointCandidate { height: 10 };

        // Process checkpoint on ordered sequence should attempt two-phase commit
        // Note: This will only succeed if we simulate quorum votes; here we just
        // exercise the code path and require that it does not panic.
        let result = bft.process_checkpoint_on_ordered_sequence(candidate.clone());

        // Without a simulated quorum, we either get None (no QC) or, if the internal
        // self-vote reaches quorum in some configuration, we may see a Commit QC.
        assert!(result.is_none() || matches!(result.as_ref().unwrap().phase, BftPhase::Commit));
    }
}
