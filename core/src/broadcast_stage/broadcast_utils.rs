use {
    crate::result::Result,
    bincode::serialized_size,
    crossbeam_channel::Receiver,
    solana_entry::entry::Entry,
    solana_ledger::shred::shred_data::ShredData,
    solana_poh::poh_recorder::WorkingBankEntry,
    solana_runtime::bank::Bank,
    solana_sdk::clock::Slot,
    std::{
        sync::Arc,
        time::{Duration, Instant},
    },
};

pub(super) struct ReceiveResults {
    pub entries: Vec<Entry>,
    pub time_elapsed: Duration,
    pub bank: Arc<Bank>,
    pub last_tick_height: u64,
}

#[derive(Clone)]
pub struct UnfinishedSlotInfo {
    pub next_shred_index: u32,
    pub(crate) next_code_index: u32,
    pub slot: Slot,
    pub parent: Slot,
}

#[derive(Default)]
pub(crate) struct RecvEntriesContext {
    pending_entry: Option<WorkingBankEntry>,
}

pub(super) fn recv_slot_entries(
    ctx: &mut RecvEntriesContext,
    receiver: &Receiver<WorkingBankEntry>,
    receiver_coalesce_ms: u64,
) -> Result<ReceiveResults> {
    let target_batch_byte_count: u64 =
        32 * ShredData::capacity(/*merkle_proof_size*/ None).unwrap() as u64;
    let mut batch_byte_count: u64 = 8; // Vec len
    let timer = Duration::new(1, 0);
    let recv_start = Instant::now();
    let mut entries = Vec::default();
    let mut pending_state = None;

    if let Some((bank, (entry, last_tick_height))) = ctx.pending_entry.take() {
        assert!(last_tick_height <= bank.max_tick_height());
        batch_byte_count += serialized_size(&entry)?;
        entries.push(entry);
        pending_state = Some((bank, last_tick_height));
    }

    let (mut bank, (entry, mut last_tick_height)) = match receiver.recv_timeout(timer) {
        Ok(working_bank_entry) => working_bank_entry,
        Err(e) => {
            if !entries.is_empty() {
                let (bank, last_tick_height) = pending_state.unwrap();
                return Ok(ReceiveResults {
                    entries,
                    time_elapsed: recv_start.elapsed(),
                    bank,
                    last_tick_height,
                });
            }
            Err(e)?
        }
    };

    batch_byte_count += serialized_size(&entry)?;
    entries.push(entry);
    let mut slot = bank.slot();
    let mut max_tick_height = bank.max_tick_height();

    assert!(last_tick_height <= max_tick_height);

    if last_tick_height != max_tick_height && batch_byte_count < target_batch_byte_count {
        let mut max_wait = Duration::from_millis(receiver_coalesce_ms);
        let mut now = Instant::now();
        while let Ok((try_bank, (entry, tick_height))) = receiver.recv_timeout(max_wait) {
            let entry_bytes = serialized_size(&entry)?;
            if batch_byte_count + entry_bytes > target_batch_byte_count {
                ctx.pending_entry = Some((try_bank, (entry, tick_height)));
                break;
            }

            // If the bank changed, that implies the previous slot was interrupted and we do not have to
            // broadcast its entries.
            if try_bank.slot() != slot {
                warn!("Broadcast for slot: {} interrupted", bank.slot());
                entries.clear();
                batch_byte_count = 8; // Vec len
                bank = try_bank;
                slot = bank.slot();
                max_tick_height = bank.max_tick_height();
            }
            last_tick_height = tick_height;
            batch_byte_count += entry_bytes;
            entries.push(entry);

            assert!(last_tick_height <= max_tick_height);
            if last_tick_height == max_tick_height {
                break;
            }

            max_wait = max_wait.saturating_sub(now.elapsed());
            now = Instant::now();
        }
    }

    let time_elapsed = recv_start.elapsed();
    Ok(ReceiveResults {
        entries,
        time_elapsed,
        bank,
        last_tick_height,
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::broadcast_stage::DEFAULT_ENTRY_COALESCE_MS,
        crossbeam_channel::unbounded,
        solana_ledger::genesis_utils::{create_genesis_config, GenesisConfigInfo},
        solana_sdk::{
            genesis_config::GenesisConfig, pubkey::Pubkey, system_transaction,
            transaction::Transaction,
        },
    };

    fn setup_test() -> (GenesisConfig, Arc<Bank>, Transaction) {
        let GenesisConfigInfo {
            genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(2);
        let bank0 = Arc::new(Bank::new_for_tests(&genesis_config));
        let tx = system_transaction::transfer(
            &mint_keypair,
            &solana_sdk::pubkey::new_rand(),
            1,
            genesis_config.hash(),
        );

        (genesis_config, bank0, tx)
    }

    #[test]
    fn test_recv_slot_entries_1() {
        let (genesis_config, bank0, tx) = setup_test();

        let bank1 = Arc::new(Bank::new_from_parent(&bank0, &Pubkey::default(), 1));
        let (s, r) = unbounded();
        let mut last_hash = genesis_config.hash();

        assert!(bank1.max_tick_height() > 1);
        let entries: Vec<_> = (1..bank1.max_tick_height() + 1)
            .map(|i| {
                let entry = Entry::new(&last_hash, 1, vec![tx.clone()]);
                last_hash = entry.hash;
                s.send((bank1.clone(), (entry.clone(), i))).unwrap();
                entry
            })
            .collect();

        let mut res_entries = vec![];
        let mut last_tick_height = 0;
        let mut recv_entries_ctx = RecvEntriesContext::default();
        while let Ok(result) =
            recv_slot_entries(&mut recv_entries_ctx, &r, DEFAULT_ENTRY_COALESCE_MS)
        {
            assert_eq!(result.bank.slot(), bank1.slot());
            last_tick_height = result.last_tick_height;
            res_entries.extend(result.entries);
        }
        assert_eq!(last_tick_height, bank1.max_tick_height());
        assert_eq!(res_entries, entries);
    }

    #[test]
    fn test_recv_slot_entries_2() {
        let (genesis_config, bank0, tx) = setup_test();

        let bank1 = Arc::new(Bank::new_from_parent(&bank0, &Pubkey::default(), 1));
        let bank2 = Arc::new(Bank::new_from_parent(&bank1, &Pubkey::default(), 2));
        let (s, r) = unbounded();

        let mut last_hash = genesis_config.hash();
        assert!(bank1.max_tick_height() > 1);
        // Simulate slot 2 interrupting slot 1's transmission
        let expected_last_height = bank1.max_tick_height();
        let last_entry = (1..=bank1.max_tick_height())
            .map(|tick_height| {
                let entry = Entry::new(&last_hash, 1, vec![tx.clone()]);
                last_hash = entry.hash;
                // Interrupt slot 1 right before the last tick
                if tick_height == expected_last_height {
                    s.send((bank2.clone(), (entry.clone(), tick_height)))
                        .unwrap();
                    Some(entry)
                } else {
                    s.send((bank1.clone(), (entry, tick_height))).unwrap();
                    None
                }
            })
            .last()
            .unwrap()
            .unwrap();

        let mut res_entries = vec![];
        let mut last_tick_height = 0;
        let mut bank_slot = 0;
        let mut recv_entries_ctx = RecvEntriesContext::default();
        while let Ok(result) =
            recv_slot_entries(&mut recv_entries_ctx, &r, DEFAULT_ENTRY_COALESCE_MS)
        {
            bank_slot = result.bank.slot();
            last_tick_height = result.last_tick_height;
            res_entries = result.entries;
        }
        assert_eq!(bank_slot, bank2.slot());
        assert_eq!(last_tick_height, expected_last_height);
        assert_eq!(res_entries, vec![last_entry]);
    }
}
