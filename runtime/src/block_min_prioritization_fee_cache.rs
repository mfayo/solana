use {
    crate::{
        block_min_prioritization_fee::*,
        block_min_prioritization_fee_cache_query::BlockMinPrioritizationFeeCacheQuery,
        block_min_prioritization_fee_cache_update::BlockMinPrioritizationFeeCacheUpdate,
    },
    log::*,
    solana_sdk::{clock::Slot, pubkey::Pubkey, transaction::SanitizedTransaction},
    std::collections::HashMap,
};

/// The maximum number of blocks to keep in `BlockMinPrioritizationFeeCache`; States from
/// up to 150 recent blocks should be sufficient to estimate minimal prioritization fee to
/// land transactions to current block.
const NUMBER_OF_RECENT_BLOCKS: usize = 150;

/// Holds up to NUMBER_OF_RECENT_BLOCKS recent block's min prioritization fee for block,
/// and for each writable accounts per block.
pub struct BlockMinPrioritizationFeeCache {
    cache: HashMap<Slot, BlockMinPrioritizationFee>,
}

impl Default for BlockMinPrioritizationFeeCache {
    fn default() -> Self {
        Self::new(NUMBER_OF_RECENT_BLOCKS)
    }
}

impl BlockMinPrioritizationFeeCache {
    pub fn new(capacity: usize) -> Self {
        BlockMinPrioritizationFeeCache {
            cache: HashMap::with_capacity(capacity),
        }
    }

    #[allow(dead_code)]
    fn get_block_min_prioritization_fee(&self, slot: &Slot) -> Option<&BlockMinPrioritizationFee> {
        self.cache.get(slot)
    }

    fn get_mut_block_min_prioritization_fee(
        &mut self,
        slot: &Slot,
    ) -> Option<&mut BlockMinPrioritizationFee> {
        self.cache.get_mut(slot)
    }

    fn get_or_add_mut_block_min_prioritization_fee(
        &mut self,
        slot: &Slot,
    ) -> &mut BlockMinPrioritizationFee {
        self.cache
            .entry(*slot)
            .or_insert_with(BlockMinPrioritizationFee::default)
    }
}

impl BlockMinPrioritizationFeeCacheUpdate for BlockMinPrioritizationFeeCache {
    /// Update block's min prioritization fee with `txs`,
    /// Returns updated min prioritization fee for `slot`
    fn update_transactions<'a>(
        &mut self,
        slot: Slot,
        txs: impl Iterator<Item = &'a SanitizedTransaction>,
    ) -> Option<u64> {
        let block = self.get_or_add_mut_block_min_prioritization_fee(&slot);

        for sanitized_tx in txs {
            match block.update_for_transaction(sanitized_tx) {
                Err(BlockMinPrioritizationFeeError::FailGetTransactionPriorityDetails) => {
                    debug!("TODO -- fail get tx priority details")
                } //self.inc_fail_get_transaction_priority_details_count(),
                Err(BlockMinPrioritizationFeeError::FailGetTransactionAccountLocks) => {
                    debug!("TODO -- fail get account locks")
                } //self.inc_fail_get_transaction_account_locks_count(),
                _ => debug!("TODO -- succeeded"), //self.inc_success_transaction_update_count(),
            }
        }
        block.get_block_fee()
    }

    /// bank is completely replayed from blockstore, prune irrelevant accounts to save space,
    /// its fee stats can be made available to queries
    fn finalize_block(&mut self, slot: Slot) {
        if let Some(block) = self.get_mut_block_min_prioritization_fee(&slot) {
            block.prune_irrelevant_accounts();
            let _ = block.mark_block_completed();
        } else {
            debug!("TODO"); //self.inc_fail_finalize_block_not_found();
        }
    }
}

impl BlockMinPrioritizationFeeCacheQuery for BlockMinPrioritizationFeeCache {
    /// Returns number of blocks that have finalized min fees collection
    fn available_block_count(&self) -> usize {
        self.cache
            .iter()
            .filter(|(_slot, block_min_prioritization_fee)| {
                block_min_prioritization_fee.is_finalized()
            })
            .count()
    }

    /// Query block minimum fees from finalized blocks in cache,
    /// Returns a vector of fee; call site can use it to produce
    /// average, or top 5% etc.
    fn get_block_min_prioritization_fees(&self) -> Vec<u64> {
        self.cache
            .iter()
            .filter_map(|(_slot, block_min_prioritization_fee)| {
                block_min_prioritization_fee
                    .is_finalized()
                    .then(|| block_min_prioritization_fee.get_block_fee())
            })
            .flatten()
            .collect()
    }

    /// Query given account minimum fees from finalized blocks in cache,
    /// Returns a vector of fee; call site can use it to produce
    /// average, or top 5% etc.
    fn get_account_min_prioritization_fees(&self, account_key: &Pubkey) -> Vec<u64> {
        self.cache
            .iter()
            .filter_map(|(_slot, block_min_prioritization_fee)| {
                block_min_prioritization_fee
                    .is_finalized()
                    .then(|| block_min_prioritization_fee.get_account_fee(account_key))
            })
            .flatten()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_sdk::{
            compute_budget::ComputeBudgetInstruction, message::Message, pubkey::Pubkey,
            system_instruction, transaction::Transaction,
        },
    };

    fn build_sanitized_transaction_for_test(
        compute_unit_price: u64,
        signer_account: &Pubkey,
        write_account: &Pubkey,
    ) -> SanitizedTransaction {
        let transaction = Transaction::new_unsigned(Message::new(
            &[
                system_instruction::transfer(signer_account, write_account, 1),
                ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price),
            ],
            Some(signer_account),
        ));

        SanitizedTransaction::try_from_legacy_transaction(transaction).unwrap()
    }

    #[test]
    fn test_block_min_prioritization_fee_cache_update() {
        solana_logger::setup();
        let write_account_a = Pubkey::new_unique();
        let write_account_b = Pubkey::new_unique();
        let write_account_c = Pubkey::new_unique();

        // Set up test with 3 transactions, in format of [fee, write-accounts...],
        // Shall expect fee cache is updated in following sequence:
        // transaction                    block min prioritization fee cache
        // [fee, write_accounts...]  -->  [block, account_a, account_b, account_c]
        // -----------------------------------------------------------------------
        // [5,   a, b             ]  -->  [5,     5,         5,         nil      ]
        // [9,      b, c          ]  -->  [5,     5,         5,         9        ]
        // [2,   a,    c          ]  -->  [2,     2,         5,         2        ]
        //
        let txs = vec![
            build_sanitized_transaction_for_test(5, &write_account_a, &write_account_b),
            build_sanitized_transaction_for_test(9, &write_account_b, &write_account_c),
            build_sanitized_transaction_for_test(2, &write_account_a, &write_account_c),
        ];

        let slot = 1;

        let mut block_min_prioritization_fee_cache = BlockMinPrioritizationFeeCache::default();
        assert_eq!(
            2,
            block_min_prioritization_fee_cache
                .update_transactions(slot, txs.iter())
                .unwrap()
        );

        // assert block min fee and account a, b, c fee accordingly
        {
            let block_min_fee = block_min_prioritization_fee_cache
                .get_block_min_prioritization_fee(&slot)
                .unwrap();
            assert_eq!(2, block_min_fee.get_block_fee().unwrap());
            assert_eq!(2, block_min_fee.get_account_fee(&write_account_a).unwrap());
            assert_eq!(5, block_min_fee.get_account_fee(&write_account_b).unwrap());
            assert_eq!(2, block_min_fee.get_account_fee(&write_account_c).unwrap());
            // assert unknown account d fee
            assert!(block_min_fee
                .get_account_fee(&Pubkey::new_unique())
                .is_none());
            // assert unknown slot
            assert!(block_min_prioritization_fee_cache
                .get_block_min_prioritization_fee(&100)
                .is_none());
        }

        // assert after prune, account a and c should be removed from cache to save space
        {
            block_min_prioritization_fee_cache.finalize_block(slot);
            let block_min_fee = block_min_prioritization_fee_cache
                .get_block_min_prioritization_fee(&slot)
                .unwrap();
            assert_eq!(2, block_min_fee.get_block_fee().unwrap());
            assert!(block_min_fee.get_account_fee(&write_account_a).is_none());
            assert_eq!(5, block_min_fee.get_account_fee(&write_account_b).unwrap());
            assert!(block_min_fee.get_account_fee(&write_account_c).is_none());
        }
    }

    #[test]
    fn test_available_block_count() {
        let mut block_min_prioritization_fee_cache = BlockMinPrioritizationFeeCache::default();

        assert!(block_min_prioritization_fee_cache
            .get_or_add_mut_block_min_prioritization_fee(&1)
            .mark_block_completed()
            .is_ok());
        assert!(block_min_prioritization_fee_cache
            .get_or_add_mut_block_min_prioritization_fee(&2)
            .mark_block_completed()
            .is_ok());
        block_min_prioritization_fee_cache.get_or_add_mut_block_min_prioritization_fee(&3);

        assert_eq!(
            2,
            block_min_prioritization_fee_cache.available_block_count()
        );
    }

    fn assert_vec_eq(expected: &mut Vec<u64>, actual: &mut Vec<u64>) {
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_get_block_min_prioritization_fees() {
        solana_logger::setup();
        let write_account_a = Pubkey::new_unique();
        let write_account_b = Pubkey::new_unique();
        let write_account_c = Pubkey::new_unique();

        let mut block_min_prioritization_fee_cache = BlockMinPrioritizationFeeCache::default();

        // Assert no min fee from empty cache
        assert!(block_min_prioritization_fee_cache
            .get_block_min_prioritization_fees()
            .is_empty());

        // Assert after add one transaction for slot 1
        {
            let txs = vec![build_sanitized_transaction_for_test(
                5,
                &write_account_a,
                &write_account_b,
            )];
            assert_eq!(
                5,
                block_min_prioritization_fee_cache
                    .update_transactions(1, txs.iter())
                    .unwrap()
            );
            // before block is marked as completed
            assert!(block_min_prioritization_fee_cache
                .get_block_min_prioritization_fees()
                .is_empty());
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(1);
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache.get_block_min_prioritization_fees()
            );
        }

        // Assert after add one transaction for slot 2
        {
            let txs = vec![build_sanitized_transaction_for_test(
                9,
                &write_account_b,
                &write_account_c,
            )];
            assert_eq!(
                9,
                block_min_prioritization_fee_cache
                    .update_transactions(2, txs.iter())
                    .unwrap()
            );
            // before block is marked as completed
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache.get_block_min_prioritization_fees()
            );
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(2);
            assert_vec_eq(
                &mut vec![5, 9],
                &mut block_min_prioritization_fee_cache.get_block_min_prioritization_fees(),
            );
        }

        // Assert after add one transaction for slot 3
        {
            let txs = vec![build_sanitized_transaction_for_test(
                2,
                &write_account_a,
                &write_account_c,
            )];
            assert_eq!(
                2,
                block_min_prioritization_fee_cache
                    .update_transactions(3, txs.iter())
                    .unwrap()
            );
            // before block is marked as completed
            assert_vec_eq(
                &mut vec![5, 9],
                &mut block_min_prioritization_fee_cache.get_block_min_prioritization_fees(),
            );
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(3);
            assert_vec_eq(
                &mut vec![5, 9, 2],
                &mut block_min_prioritization_fee_cache.get_block_min_prioritization_fees(),
            );
        }
    }

    #[test]
    fn test_get_account_min_prioritization_fees() {
        solana_logger::setup();
        let write_account_a = Pubkey::new_unique();
        let write_account_b = Pubkey::new_unique();
        let write_account_c = Pubkey::new_unique();

        let mut block_min_prioritization_fee_cache = BlockMinPrioritizationFeeCache::default();

        // Assert no min fee from empty cache
        assert!(block_min_prioritization_fee_cache
            .get_account_min_prioritization_fees(&write_account_a)
            .is_empty());
        assert!(block_min_prioritization_fee_cache
            .get_account_min_prioritization_fees(&write_account_b)
            .is_empty());
        assert!(block_min_prioritization_fee_cache
            .get_account_min_prioritization_fees(&write_account_c)
            .is_empty());

        // Assert after add one transaction for slot 1
        {
            let txs = vec![
                build_sanitized_transaction_for_test(5, &write_account_a, &write_account_b),
                build_sanitized_transaction_for_test(
                    0,
                    &Pubkey::new_unique(),
                    &Pubkey::new_unique(),
                ),
            ];
            block_min_prioritization_fee_cache.update_transactions(1, txs.iter());
            // before block is marked as completed
            assert!(block_min_prioritization_fee_cache
                .get_account_min_prioritization_fees(&write_account_a)
                .is_empty());
            assert!(block_min_prioritization_fee_cache
                .get_account_min_prioritization_fees(&write_account_b)
                .is_empty());
            assert!(block_min_prioritization_fee_cache
                .get_account_min_prioritization_fees(&write_account_c)
                .is_empty());
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(1);
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_a)
            );
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_b)
            );
            assert!(block_min_prioritization_fee_cache
                .get_account_min_prioritization_fees(&write_account_c)
                .is_empty());
        }

        // Assert after add one transaction for slot 2
        {
            let txs = vec![
                build_sanitized_transaction_for_test(9, &write_account_b, &write_account_c),
                build_sanitized_transaction_for_test(
                    0,
                    &Pubkey::new_unique(),
                    &Pubkey::new_unique(),
                ),
            ];
            block_min_prioritization_fee_cache.update_transactions(2, txs.iter());
            // before block is marked as completed
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_a)
            );
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_b)
            );
            assert!(block_min_prioritization_fee_cache
                .get_account_min_prioritization_fees(&write_account_c)
                .is_empty());
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(2);
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_a)
            );
            assert_vec_eq(
                &mut vec![5, 9],
                &mut block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_b),
            );
            assert_eq!(
                vec![9],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_c)
            );
        }

        // Assert after add one transaction for slot 3
        {
            let txs = vec![
                build_sanitized_transaction_for_test(2, &write_account_a, &write_account_c),
                build_sanitized_transaction_for_test(
                    0,
                    &Pubkey::new_unique(),
                    &Pubkey::new_unique(),
                ),
            ];
            block_min_prioritization_fee_cache.update_transactions(3, txs.iter());
            // before block is marked as completed
            assert_eq!(
                vec![5],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_a)
            );
            assert_vec_eq(
                &mut vec![5, 9],
                &mut block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_b),
            );
            assert_eq!(
                vec![9],
                block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_c)
            );
            // after block is completed
            block_min_prioritization_fee_cache.finalize_block(3);
            assert_vec_eq(
                &mut vec![5, 2],
                &mut block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_a),
            );
            assert_vec_eq(
                &mut vec![5, 9],
                &mut block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_b),
            );
            assert_vec_eq(
                &mut vec![9, 2],
                &mut block_min_prioritization_fee_cache
                    .get_account_min_prioritization_fees(&write_account_c),
            );
        }
    }
}
