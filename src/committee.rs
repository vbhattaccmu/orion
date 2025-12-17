use crate::crypto::VerifyingKey;
use crate::types::{AuthorityIndex, RoundNumber};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Committee {
    validators: Vec<ValidatorInfo>,
    validator_map: HashMap<AuthorityIndex, usize>,
}

#[derive(Clone, Debug)]
pub struct ValidatorInfo {
    pub index: AuthorityIndex,
    pub public_key: VerifyingKey,
    pub stake: u64,
}

impl Committee {
    pub fn new(validators: Vec<ValidatorInfo>) -> Self {
        let validator_map = validators
            .iter()
            .enumerate()
            .map(|(i, v)| (v.index, i))
            .collect();
        Self {
            validators,
            validator_map,
        }
    }

    pub fn size(&self) -> usize {
        self.validators.len()
    }

    pub fn get(&self, index: AuthorityIndex) -> Option<&ValidatorInfo> {
        self.validator_map.get(&index).map(|&i| &self.validators[i])
    }

    pub fn validators(&self) -> &[ValidatorInfo] {
        &self.validators
    }

    pub fn quorum_threshold(&self) -> usize {
        let n = self.size();
        (2 * n) / 3 + 1
    }

    pub fn fault_tolerance(&self) -> usize {
        (self.size() - 1) / 3
    }

    pub fn elect_leader(&self, round: RoundNumber) -> AuthorityIndex {
        let n = self.size();
        let index = (round % n as u64) as usize;
        self.validators[index].index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;

    fn create_test_committee(size: usize) -> Committee {
        let mut validators = Vec::new();
        for i in 0..size {
            let keypair = KeyPair::generate();
            validators.push(ValidatorInfo {
                index: i as u32,
                public_key: keypair.verifying_key.clone(),
                stake: 1,
            });
        }
        Committee::new(validators)
    }

    #[test]
    fn test_committee_creation() {
        let committee = create_test_committee(4);
        assert_eq!(committee.size(), 4);
    }

    #[test]
    fn test_quorum_threshold() {
        let committee = create_test_committee(4);
        // For 4 validators: (2*4)/3 + 1 = 3
        assert_eq!(committee.quorum_threshold(), 3);

        let committee = create_test_committee(7);
        // For 7 validators: (2*7)/3 + 1 = 5
        assert_eq!(committee.quorum_threshold(), 5);
    }

    #[test]
    fn test_fault_tolerance() {
        let committee = create_test_committee(4);
        // For 4 validators: (4-1)/3 = 1
        assert_eq!(committee.fault_tolerance(), 1);

        let committee = create_test_committee(7);
        // For 7 validators: (7-1)/3 = 2
        assert_eq!(committee.fault_tolerance(), 2);
    }

    #[test]
    fn test_leader_election() {
        let committee = create_test_committee(4);

        // Round 0 should elect validator 0
        assert_eq!(committee.elect_leader(0), 0);

        // Round 1 should elect validator 1
        assert_eq!(committee.elect_leader(1), 1);

        // Round 4 should wrap around to validator 0
        assert_eq!(committee.elect_leader(4), 0);
    }

    #[test]
    fn test_get_validator() {
        let committee = create_test_committee(4);

        assert!(committee.get(0).is_some());
        assert!(committee.get(3).is_some());
        assert!(committee.get(4).is_none());
    }
}
