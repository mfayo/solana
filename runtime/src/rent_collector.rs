//! calculate and collect rent from Accounts
use solana_sdk::{
    account::Account,
    clock::Epoch,
    epoch_schedule::EpochSchedule,
    genesis_config::{GenesisConfig, OperatingMode},
    incinerator,
    pubkey::Pubkey,
    rent::Rent,
    sysvar,
};

#[derive(Serialize, Debug, Deserialize, Clone, PartialEq)]
pub struct RentCollector {
    pub epoch: Epoch,
    pub epoch_schedule: EpochSchedule,
    pub slots_per_year: f64,
    pub rent: Rent,
    // serde(skip) is needed not to break abi
    // Also, wrap this with Option so that we can spot any uninitialized codepath (like
    // snapshot restore)
    #[serde(skip)]
    pub operating_mode: Option<OperatingMode>,
}

impl Default for RentCollector {
    fn default() -> Self {
        Self {
            epoch: Epoch::default(),
            epoch_schedule: EpochSchedule::default(),
            // derive default value using GenesisConfig::default()
            slots_per_year: GenesisConfig::default().slots_per_year(),
            rent: Rent::default(),
            operating_mode: Option::default(),
        }
    }
}

impl RentCollector {
    pub fn new(
        epoch: Epoch,
        epoch_schedule: &EpochSchedule,
        slots_per_year: f64,
        rent: &Rent,
        operating_mode: OperatingMode,
    ) -> Self {
        Self {
            epoch,
            epoch_schedule: *epoch_schedule,
            slots_per_year,
            rent: *rent,
            operating_mode: Some(operating_mode),
        }
    }

    pub fn clone_with_epoch(&self, epoch: Epoch, operating_mode: OperatingMode) -> Self {
        Self {
            epoch,
            operating_mode: Some(operating_mode),
            ..self.clone()
        }
    }

    fn enable_new_behavior(&self) -> bool {
        match self.operating_mode.unwrap() {
            OperatingMode::Development => true,
            OperatingMode::Preview => self.epoch >= Epoch::max_value(),
            OperatingMode::Stable => self.epoch >= Epoch::max_value(),
        }
    }

    // updates this account's lamports and status and returns
    //  the account rent collected, if any
    //
    #[must_use = "add to Bank::collected_rent"]
    pub fn collect_from_existing_account(&self, address: &Pubkey, account: &mut Account) -> u64 {
        if account.executable
            || account.rent_epoch > self.epoch
            || sysvar::check_id(&account.owner)
            || *address == incinerator::id()
        {
            0
        } else {
            let slots_elapsed: u64 = (account.rent_epoch..=self.epoch)
                .map(|epoch| self.epoch_schedule.get_slots_in_epoch(epoch + 1))
                .sum();

            let (rent_due, exempt) = self.rent.due(
                account.lamports,
                account.data.len(),
                slots_elapsed as f64 / self.slots_per_year,
            );

            if exempt || rent_due != 0 {
                if account.lamports > rent_due {
                    account.rent_epoch = self.epoch
                        + if self.enable_new_behavior() && exempt {
                            // Rent isn't collected for the next epoch
                            // Make sure to check exempt status later in curent epoch again
                            0
                        } else {
                            // Rent is collected for next epoch
                            1
                        };
                    account.lamports -= rent_due;
                    rent_due
                } else {
                    let rent_charged = account.lamports;
                    *account = Account::default();
                    rent_charged
                }
            } else {
                // maybe collect rent later, leave account alone
                0
            }
        }
    }

    #[must_use = "add to Bank::collected_rent"]
    pub fn collect_from_created_account(&self, address: &Pubkey, account: &mut Account) -> u64 {
        // initialize rent_epoch as created at this epoch
        account.rent_epoch = self.epoch;
        self.collect_from_existing_account(address, account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_from_account_created_and_existing() {
        let old_lamports = 1000;
        let old_epoch = 1;
        let new_epoch = 3;

        let (mut created_account, mut existing_account) = {
            let mut account = Account::default();
            account.lamports = old_lamports;
            account.rent_epoch = old_epoch;

            (account.clone(), account)
        };

        let rent_collector =
            RentCollector::default().clone_with_epoch(new_epoch, OperatingMode::Development);

        // collect rent on a newly-created account
        let collected =
            rent_collector.collect_from_created_account(&Pubkey::new_rand(), &mut created_account);
        assert!(created_account.lamports < old_lamports);
        assert_eq!(created_account.lamports + collected, old_lamports);
        assert_ne!(created_account.rent_epoch, old_epoch);

        // collect rent on a already-existing account
        let collected = rent_collector
            .collect_from_existing_account(&Pubkey::new_rand(), &mut existing_account);
        assert!(existing_account.lamports < old_lamports);
        assert_eq!(existing_account.lamports + collected, old_lamports);
        assert_ne!(existing_account.rent_epoch, old_epoch);

        // newly created account should be collected for less rent; thus more remaining balance
        assert!(created_account.lamports > existing_account.lamports);
        assert_eq!(created_account.rent_epoch, existing_account.rent_epoch);
    }

    #[test]
    fn test_rent_exempt_temporal_escape() {
        let mut account = Account::default();
        let epoch = 3;
        let huge_lamports = 123_456_789_012;
        let tiny_lamports = 789_012;
        let mut collected;
        let pubkey = Pubkey::new_rand();

        account.lamports = huge_lamports;
        assert_eq!(account.rent_epoch, 0);

        // create a tested rent collector
        let rent_collector =
            RentCollector::default().clone_with_epoch(epoch, OperatingMode::Development);

        // first mark account as being collected while being rent-exempt
        collected = rent_collector.collect_from_existing_account(&pubkey, &mut account);
        assert_eq!(account.lamports, huge_lamports);
        assert_eq!(collected, 0);

        // decrease the balance not to be rent-exempt
        account.lamports = tiny_lamports;

        // ... and trigger another rent collection on the same epoch and check that rent is working
        collected = rent_collector.collect_from_existing_account(&pubkey, &mut account);
        assert_eq!(account.lamports, tiny_lamports - collected);
        assert_ne!(collected, 0);
    }
}
