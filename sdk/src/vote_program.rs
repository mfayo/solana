//! Vote program
//! Receive and processes votes from validators

use crate::hash::Hash;
use crate::native_program::ProgramError;
use crate::pubkey::Pubkey;
use bincode::{deserialize, serialize_into, serialized_size, ErrorKind};
use std::collections::{HashMap, VecDeque};

pub const VOTE_PROGRAM_ID: [u8; 32] = [
    132, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

pub fn check_id(program_id: &Pubkey) -> bool {
    program_id.as_ref() == VOTE_PROGRAM_ID
}

pub fn id() -> Pubkey {
    Pubkey::new(&VOTE_PROGRAM_ID)
}

// Maximum number of votes to keep around
pub const MAX_VOTE_HISTORY: usize = 32;

#[derive(Serialize, Default, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct Vote {
    // TODO: add signature of the state here as well
    /// A vote for height tick_height
    pub tick_height: u64,
}

#[derive(Serialize, Default, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct BlockDescription {
    pub tick_height: u64,              // The height of this block
    pub entry_id: Hash,                // ID of the latest entry in the ledger
    pub state_hash: Hash, // A hash representing all state after interpreting the ledger
    pub weights: HashMap<Pubkey, u64>, // A map of voter IDs to their respective weights
}

impl BlockDescription {
    pub fn new(
        tick_height: u64,
        entry_id: Hash,
        state_hash: Hash,
        weights: HashMap<Pubkey, u64>,
    ) -> Self {
        Self {
            tick_height,
            entry_id,
            state_hash,
            weights,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum VoteInstruction {
    /// Register a new "vote account" to represent a particular validator in the Vote Contract,
    /// and initialize the VoteState for this "vote account"
    /// * Transaction::keys[0] - the validator id
    /// * Transaction::keys[1] - the new "vote account" to be associated with the validator
    /// identified by keys[0] for voting
    RegisterAccount,
    NewVote(Vote),

    /// Propose a new block to be voted on.
    /// * Transaction::keys[0] - the leader's signed pubkey
    /// * Transaction::keys[1] - a key to a new account holding a WeightedElection instance
    ProposeBlock(BlockDescription),

    /// Vote on a block.
    /// * Transaction::keys[0] - the validators's signed pubkey
    /// * Transaction::keys[1] - a key to an account holding the WeightedElection instance
    Vote,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteProgram {
    pub votes: VecDeque<Vote>,
    pub node_id: Pubkey,
}

pub fn get_max_size() -> usize {
    // Upper limit on the size of the Vote State. Equal to
    // sizeof(VoteProgram) when votes.len() is MAX_VOTE_HISTORY
    let mut vote_program = VoteProgram::default();
    vote_program.votes = VecDeque::from(vec![Vote::default(); MAX_VOTE_HISTORY]);
    serialized_size(&vote_program).unwrap() as usize
}

impl VoteProgram {
    pub fn deserialize(input: &[u8]) -> Result<VoteProgram, ProgramError> {
        deserialize(input).map_err(|_| ProgramError::InvalidUserdata)
    }

    pub fn serialize(self: &VoteProgram, output: &mut [u8]) -> Result<(), ProgramError> {
        serialize_into(output, self).map_err(|err| match *err {
            ErrorKind::SizeLimit => ProgramError::UserdataTooSmall,
            _ => ProgramError::GenericError,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde() {
        let mut buffer: Vec<u8> = vec![0; get_max_size()];
        let mut vote_program = VoteProgram::default();
        vote_program.votes = (0..MAX_VOTE_HISTORY).map(|_| Vote::default()).collect();
        vote_program.serialize(&mut buffer).unwrap();
        assert_eq!(VoteProgram::deserialize(&buffer).unwrap(), vote_program);
    }
}
