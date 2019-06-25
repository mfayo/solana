use crate::get_segment_from_slot;
use log::*;
use num_derive::FromPrimitive;
use serde_derive::{Deserialize, Serialize};
use solana_sdk::account::Account;
use solana_sdk::account::KeyedAccount;
use solana_sdk::account_utils::State;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::InstructionError;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::syscall::current::Current;
use solana_sdk::syscall::rewards::Rewards;
use std::collections::BTreeMap;

// Todo Tune this for actual use cases when PoRep is feature complete
pub const STORAGE_ACCOUNT_SPACE: u64 = 1024 * 8;
pub const MAX_PROOFS_PER_SEGMENT: usize = 80;

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Credits {
    // current epoch
    epoch: u64,
    // credits earned in the last epoch
    pub last_epoch: u64,
    // currently pending credits
    pub current_epoch: u64,
    // credits ready to be claimed
    pub redeemable: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, FromPrimitive)]
pub enum StorageError {
    InvalidSegment,
    InvalidBlockhash,
    InvalidProofMask,
    DuplicateProof,
    RewardPoolDepleted,
    InvalidOwner,
    ProofLimitReached,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum ProofStatus {
    Skipped,
    Valid,
    NotValid,
}

impl Default for ProofStatus {
    fn default() -> Self {
        ProofStatus::Skipped
    }
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Proof {
    /// The encryption key the replicator used (also used to generate offsets)
    pub signature: Signature,
    /// A "recent" blockhash used to generate the seed
    pub blockhash: Hash,
    /// The resulting sampled state
    pub sha_state: Hash,
    /// The start index of the segment proof is for
    pub segment_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StorageContract {
    Uninitialized, // Must be first (aka, 0)

    ValidatorStorage {
        owner: Pubkey,
        // Most recently advertised slot
        slot: u64,
        // Most recently advertised blockhash
        hash: Hash,
        // Lockouts and Rewards are per segment per replicator. It needs to remain this way until
        // the challenge stage is added.
        lockout_validations: BTreeMap<usize, BTreeMap<Pubkey, Vec<ProofStatus>>>,
        // Used to keep track of ongoing credits
        credits: Credits,
    },

    ReplicatorStorage {
        owner: Pubkey,
        // TODO what to do about duplicate proofs across segments? - Check the blockhashes
        // Map of Proofs per segment, in a Vec
        proofs: BTreeMap<usize, Vec<Proof>>,
        // Map of Rewards per segment, in a BTreeMap based on the validator account that verified
        // the proof. This can be used for challenge stage when its added
        validations: BTreeMap<usize, BTreeMap<Pubkey, Vec<ProofStatus>>>,
        // Used to keep track of ongoing credits
        credits: Credits,
    },

    RewardsPool,
}

// utility function, used by Bank, tests, genesis
pub fn create_validator_storage_account(owner: Pubkey, lamports: u64) -> Account {
    let mut storage_account = Account::new(lamports, STORAGE_ACCOUNT_SPACE as usize, &crate::id());

    storage_account
        .set_state(&StorageContract::ValidatorStorage {
            owner,
            slot: 0,
            hash: Hash::default(),
            lockout_validations: BTreeMap::new(),
            credits: Credits::default(),
        })
        .expect("set_state");

    storage_account
}

pub struct StorageAccount<'a> {
    pub(crate) id: Pubkey,
    account: &'a mut Account,
}

impl<'a> StorageAccount<'a> {
    pub fn new(id: Pubkey, account: &'a mut Account) -> Self {
        Self { id, account }
    }

    pub fn initialize_replicator_storage(&mut self, owner: Pubkey) -> Result<(), InstructionError> {
        let storage_contract = &mut self.account.state()?;
        if let StorageContract::Uninitialized = storage_contract {
            *storage_contract = StorageContract::ReplicatorStorage {
                owner,
                proofs: BTreeMap::new(),
                validations: BTreeMap::new(),
                credits: Credits::default(),
            };
            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::AccountAlreadyInitialized)?
        }
    }

    pub fn initialize_validator_storage(&mut self, owner: Pubkey) -> Result<(), InstructionError> {
        let storage_contract = &mut self.account.state()?;
        if let StorageContract::Uninitialized = storage_contract {
            *storage_contract = StorageContract::ValidatorStorage {
                owner,
                slot: 0,
                hash: Hash::default(),
                lockout_validations: BTreeMap::new(),
                credits: Credits::default(),
            };
            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::AccountAlreadyInitialized)?
        }
    }

    pub fn submit_mining_proof(
        &mut self,
        sha_state: Hash,
        segment_index: usize,
        signature: Signature,
        blockhash: Hash,
        current: Current,
    ) -> Result<(), InstructionError> {
        let mut storage_contract = &mut self.account.state()?;
        if let StorageContract::ReplicatorStorage {
            proofs,
            validations,
            credits,
            ..
        } = &mut storage_contract
        {
            let current_segment = get_segment_from_slot(current.slot);

            // clean up the account
            // TODO check for time correctness - storage seems to run at a delay of about 3
            *proofs = proofs
                .iter()
                .filter(|(segment, _)| **segment >= current_segment.saturating_sub(5))
                .map(|(segment, proofs)| (*segment, proofs.clone()))
                .collect();
            *validations = validations
                .iter()
                .filter(|(segment, _)| **segment >= current_segment.saturating_sub(10))
                .map(|(segment, rewards)| (*segment, rewards.clone()))
                .collect();

            if segment_index >= current_segment {
                // attempt to submit proof for unconfirmed segment
                return Err(InstructionError::CustomError(
                    StorageError::InvalidSegment as u32,
                ));
            }

            debug!(
                "Mining proof submitted with contract {:?} segment_index: {}",
                sha_state, segment_index
            );

            // TODO check that this blockhash is valid and recent
            //            if !is_valid(&blockhash) {
            //                // proof isn't using a recent blockhash
            //                return Err(InstructionError::CustomError(InvalidBlockhash as u32));
            //            }

            let proof = Proof {
                sha_state,
                signature,
                blockhash,
                segment_index,
            };
            // store the proofs in the "current" segment's entry in the hash map.
            let segment_proofs = proofs.entry(current_segment).or_default();
            if segment_proofs.contains(&proof) {
                // do not accept duplicate proofs
                return Err(InstructionError::CustomError(
                    StorageError::DuplicateProof as u32,
                ));
            }
            if segment_proofs.len() >= MAX_PROOFS_PER_SEGMENT {
                // do not accept more than MAX_PROOFS_PER_SEGMENT
                return Err(InstructionError::CustomError(
                    StorageError::ProofLimitReached as u32,
                ));
            }
            update_credits(credits, current.epoch);
            segment_proofs.push(proof);
            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::InvalidArgument)?
        }
    }

    pub fn advertise_storage_recent_blockhash(
        &mut self,
        hash: Hash,
        slot: u64,
        current: Current,
    ) -> Result<(), InstructionError> {
        let mut storage_contract = &mut self.account.state()?;
        if let StorageContract::ValidatorStorage {
            slot: state_slot,
            hash: state_hash,
            lockout_validations,
            credits,
            ..
        } = &mut storage_contract
        {
            let current_segment = get_segment_from_slot(current.slot);
            let original_segment = get_segment_from_slot(*state_slot);
            let segment = get_segment_from_slot(slot);
            debug!(
                "advertise new segment: {} orig: {}",
                segment, current_segment
            );
            if segment < original_segment || segment >= current_segment {
                return Err(InstructionError::CustomError(
                    StorageError::InvalidSegment as u32,
                ));
            }

            *state_slot = slot;
            *state_hash = hash;

            // storage epoch updated, move the lockout_validations to credits
            let (_num_valid, total_validations) = count_valid_proofs(&lockout_validations);
            lockout_validations.clear();
            update_credits(credits, current.epoch);
            credits.current_epoch += total_validations;
            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::InvalidArgument)?
        }
    }

    pub fn proof_validation(
        &mut self,
        me: &Pubkey,
        current: Current,
        segment: u64,
        proofs_per_account: Vec<Vec<ProofStatus>>,
        replicator_accounts: &mut [StorageAccount],
    ) -> Result<(), InstructionError> {
        let mut storage_contract = &mut self.account.state()?;
        if let StorageContract::ValidatorStorage {
            slot: state_slot,
            lockout_validations,
            ..
        } = &mut storage_contract
        {
            let segment_index = segment as usize;
            let state_segment = get_segment_from_slot(*state_slot);

            if segment_index > state_segment {
                return Err(InstructionError::CustomError(
                    StorageError::InvalidSegment as u32,
                ));
            }

            let accounts = replicator_accounts
                .iter_mut()
                .enumerate()
                .filter_map(|(i, account)| {
                    account.account.state().ok().map(|contract| match contract {
                        StorageContract::ReplicatorStorage {
                            proofs: account_proofs,
                            ..
                        } => {
                            //TODO do this better
                            if let Some(segment_proofs) =
                                account_proofs.get(&segment_index).cloned()
                            {
                                if proofs_per_account
                                    .get(i)
                                    .filter(|proofs| proofs.len() == segment_proofs.len())
                                    .is_some()
                                {
                                    Some(account)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                })
                .flatten()
                .collect::<Vec<_>>();

            if accounts.len() != proofs_per_account.len() {
                // don't have all the accounts to validate the proofs_per_account against
                return Err(InstructionError::CustomError(
                    StorageError::InvalidProofMask as u32,
                ));
            }

            let stored_proofs: Vec<_> = proofs_per_account
                .into_iter()
                .zip(accounts.into_iter())
                .filter_map(|(checked_proofs, account)| {
                    if store_validation_result(
                        me,
                        &current,
                        account,
                        segment_index,
                        &checked_proofs,
                    )
                    .is_ok()
                    {
                        Some((account.id, checked_proofs))
                    } else {
                        None
                    }
                })
                .collect();

            // allow validators to store successful validations
            stored_proofs
                .into_iter()
                .for_each(|(replicator_account_id, proof_mask)| {
                    lockout_validations
                        .entry(segment_index)
                        .or_default()
                        .insert(replicator_account_id, proof_mask);
                });

            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::InvalidArgument)?
        }
    }

    pub fn claim_storage_reward(
        &mut self,
        rewards_pool: &mut KeyedAccount,
        current: Current,
        rewards: Rewards,
        owner: &mut StorageAccount,
    ) -> Result<(), InstructionError> {
        let mut storage_contract = &mut self.account.state()?;

        if let StorageContract::ValidatorStorage {
            owner: account_owner,
            credits,
            ..
        } = &mut storage_contract
        {
            if owner.id != *account_owner {
                Err(InstructionError::CustomError(
                    StorageError::InvalidOwner as u32,
                ))?
            }

            update_credits(credits, current.epoch);
            let pending = (credits.redeemable as f64 * rewards.storage_point_value) as u64;
            if rewards_pool.account.lamports < pending {
                Err(InstructionError::CustomError(
                    StorageError::RewardPoolDepleted as u32,
                ))?
            }
            if pending >= 1 {
                rewards_pool.account.lamports -= pending;
                owner.account.lamports += pending;
                //clear credits
                credits.redeemable = 0;
            }
            self.account.set_state(storage_contract)
        } else if let StorageContract::ReplicatorStorage {
            owner: account_owner,
            validations,
            credits,
            ..
        } = &mut storage_contract
        {
            if owner.id != *account_owner {
                Err(InstructionError::CustomError(
                    StorageError::InvalidOwner as u32,
                ))?
            }
            update_credits(credits, current.epoch);
            let (num_validations, _total_proofs) = count_valid_proofs(&validations);
            credits.current_epoch += num_validations;
            validations.clear();
            let reward = (credits.redeemable as f64 * rewards.storage_point_value) as u64;
            if rewards_pool.account.lamports < reward {
                Err(InstructionError::CustomError(
                    StorageError::RewardPoolDepleted as u32,
                ))?
            }
            if reward >= 1 {
                rewards_pool.account.lamports -= reward;
                owner.account.lamports += reward;
                credits.redeemable = 0;
            }
            self.account.set_state(storage_contract)
        } else {
            Err(InstructionError::InvalidArgument)?
        }
    }
}

pub fn create_rewards_pool() -> Account {
    Account::new_data(std::u64::MAX, &StorageContract::RewardsPool, &crate::id()).unwrap()
}

/// Store the result of a proof validation into the replicator account
fn store_validation_result(
    me: &Pubkey,
    current: &Current,
    storage_account: &mut StorageAccount,
    segment: usize,
    proof_mask: &[ProofStatus],
) -> Result<(), InstructionError> {
    let mut storage_contract = storage_account.account.state()?;
    match &mut storage_contract {
        StorageContract::ReplicatorStorage {
            proofs,
            validations,
            credits,
            ..
        } => {
            if !proofs.contains_key(&segment) {
                return Err(InstructionError::InvalidAccountData);
            }

            if proofs.get(&segment).unwrap().len() != proof_mask.len() {
                return Err(InstructionError::InvalidAccountData);
            }

            let (recorded_validations, _) = count_valid_proofs(&validations);
            validations
                .entry(segment)
                .or_default()
                .insert(*me, proof_mask.to_vec());
            update_credits(credits, current.epoch);
            let (total_validations, _) = count_valid_proofs(&validations);
            credits.current_epoch += total_validations - recorded_validations;
        }
        _ => return Err(InstructionError::InvalidAccountData),
    }
    storage_account.account.set_state(&storage_contract)
}

fn update_credits(credits: &mut Credits, current_epoch: u64) {
    if credits.epoch != current_epoch {
        credits.epoch = current_epoch;
        credits.last_epoch = credits.current_epoch;
        credits.redeemable += credits.current_epoch;
        credits.current_epoch = 0;
    }
}

fn count_valid_proofs(
    validations: &BTreeMap<usize, BTreeMap<Pubkey, Vec<ProofStatus>>>,
) -> (u64, u64) {
    let proofs = validations
        .iter()
        .flat_map(|(_, proofs)| {
            proofs
                .iter()
                .flat_map(|(_, proofs)| proofs)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut num = 0;
    for proof in proofs.iter() {
        if let ProofStatus::Valid = proof {
            num += 1;
        }
    }
    (num, proofs.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id;
    use std::collections::BTreeMap;

    #[test]
    fn test_account_data() {
        solana_logger::setup();
        let mut account = Account::default();
        account.data.resize(STORAGE_ACCOUNT_SPACE as usize, 0);
        let storage_account = StorageAccount::new(Pubkey::default(), &mut account);
        // pretend it's a validator op code
        let mut contract = storage_account.account.state().unwrap();
        if let StorageContract::ValidatorStorage { .. } = contract {
            assert!(true)
        }
        if let StorageContract::ReplicatorStorage { .. } = &mut contract {
            panic!("Contract should not decode into two types");
        }

        contract = StorageContract::ValidatorStorage {
            owner: Pubkey::default(),
            slot: 0,
            hash: Hash::default(),
            lockout_validations: BTreeMap::new(),
            credits: Credits::default(),
        };
        storage_account.account.set_state(&contract).unwrap();
        if let StorageContract::ReplicatorStorage { .. } = contract {
            panic!("Wrong contract type");
        }
        contract = StorageContract::ReplicatorStorage {
            owner: Pubkey::default(),
            proofs: BTreeMap::new(),
            validations: BTreeMap::new(),
            credits: Credits::default(),
        };
        storage_account.account.set_state(&contract).unwrap();
        if let StorageContract::ValidatorStorage { .. } = contract {
            panic!("Wrong contract type");
        }
    }

    #[test]
    fn test_process_validation() {
        let mut account = StorageAccount {
            id: Pubkey::default(),
            account: &mut Account {
                lamports: 0,
                data: vec![],
                owner: id(),
                executable: false,
            },
        };
        let segment_index = 0_usize;
        let proof = Proof {
            segment_index,
            ..Proof::default()
        };

        // account has no space
        store_validation_result(
            &Pubkey::default(),
            &mut account,
            segment_index,
            &vec![ProofStatus::default(); 1],
        )
        .unwrap_err();

        account
            .account
            .data
            .resize(STORAGE_ACCOUNT_SPACE as usize, 0);
        let storage_contract = &mut account.account.state().unwrap();
        if let StorageContract::Uninitialized = storage_contract {
            let mut proofs = BTreeMap::new();
            proofs.insert(0, vec![proof.clone()]);
            *storage_contract = StorageContract::ReplicatorStorage {
                owner: Pubkey::default(),
                proofs,
                validations: BTreeMap::new(),
                credits: Credits::default(),
            };
        };
        account.account.set_state(storage_contract).unwrap();

        // proof is valid
        store_validation_result(
            &Pubkey::default(),
            &mut account,
            segment_index,
            &vec![ProofStatus::Valid],
        )
        .unwrap();

        // proof failed verification but we should still be able to store it
        store_validation_result(
            &Pubkey::default(),
            &mut account,
            segment_index,
            &vec![ProofStatus::NotValid],
        )
        .unwrap();
    }
}
