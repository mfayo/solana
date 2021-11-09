#![allow(clippy::integer_arithmetic)]

use solana_sdk::instruction::CompiledInstruction;

/// A concurrent implementation for writing accounts into the PostgreSQL in parallel.
use {
    crate::accountsdb_plugin_postgres::{
        AccountsDbPluginPostgresConfig, AccountsDbPluginPostgresError,
    },
    chrono::Utc,
    crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender},
    log::*,
    postgres::{Client, NoTls, Statement},
    postgres_types::ToSql,
    solana_accountsdb_plugin_interface::accountsdb_plugin_interface::{
        AccountsDbPluginError, ReplicaAccountInfo, ReplicaTransactionInfo, SlotStatus,
    },
    solana_measure::measure::Measure,
    solana_metrics::*,
    solana_runtime::bank::RewardType,
    solana_sdk::{
        message::{
            v0::{self, AddressMapIndexes},
            MappedAddresses, MappedMessage, Message, MessageHeader, SanitizedMessage,
        },
        timing::AtomicInterval,
        transaction::TransactionError,
    },
    solana_transaction_status::{
        InnerInstructions, Reward, TransactionStatusMeta, TransactionTokenBalance,
    },
    std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread::{self, sleep, Builder, JoinHandle},
        time::Duration,
    },
    tokio_postgres::types,
};

/// The maximum asynchronous requests allowed in the channel to avoid excessive
/// memory usage. The downside -- calls after this threshold is reached can get blocked.
const MAX_ASYNC_REQUESTS: usize = 40960;
const DEFAULT_POSTGRES_PORT: u16 = 5432;
const DEFAULT_THREADS_COUNT: usize = 100;
const DEFAULT_ACCOUNTS_INSERT_BATCH_SIZE: usize = 10;
const ACCOUNT_COLUMN_COUNT: usize = 9;
const DEFAULT_PANIC_ON_DB_ERROR: bool = false;

struct PostgresSqlClientWrapper {
    client: Client,
    update_account_stmt: Statement,
    bulk_account_insert_stmt: Statement,
    update_slot_with_parent_stmt: Statement,
    update_slot_without_parent_stmt: Statement,
    update_transaction_log_stmt: Statement,
}

pub struct SimplePostgresClient {
    batch_size: usize,
    pending_account_updates: Vec<DbAccountInfo>,
    client: Mutex<PostgresSqlClientWrapper>,
}

struct PostgresClientWorker {
    client: SimplePostgresClient,
    /// Indicating if accounts notification during startup is done.
    is_startup_done: bool,
}

impl Eq for DbAccountInfo {}

#[derive(Clone, PartialEq, Debug)]
pub struct DbAccountInfo {
    pub pubkey: Vec<u8>,
    pub lamports: i64,
    pub owner: Vec<u8>,
    pub executable: bool,
    pub rent_epoch: i64,
    pub data: Vec<u8>,
    pub slot: i64,
    pub write_version: i64,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "CompiledInstruction")]
pub struct DbCompiledInstruction {
    pub program_id_index: i16,
    pub accounts: Vec<i16>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "InnerInstructions")]
pub struct DbInnerInstructions {
    pub index: i16,
    pub instructions: Vec<DbCompiledInstruction>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "TransactionTokenBalance")]
pub struct DbTransactionTokenBalance {
    account_index: i16,
    mint: String,
    ui_token_amount: Option<f64>,
    owner: String,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "Reward")]
pub struct DbReward {
    pubkey: String,
    lamports: i64,
    post_balance: i64,
    reward_type: Option<String>,
    commission: Option<i16>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "TransactionStatusMeta")]
pub struct DbTransactionStatusMeta {
    status: Option<String>,
    fee: i64,
    pre_balances: Vec<i64>,
    post_balances: Vec<i64>,
    inner_instructions: Option<Vec<DbInnerInstructions>>,
    log_messages: Option<Vec<String>>,
    pre_token_balances: Option<Vec<DbTransactionTokenBalance>>,
    post_token_balances: Option<Vec<DbTransactionTokenBalance>>,
    rewards: Option<Vec<DbReward>>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "TransactionMessageHeader")]
pub struct DbTransactionMessageHeader {
    num_required_signatures: i16,
    num_readonly_signed_accounts: i16,
    num_readonly_unsigned_accounts: i16,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "TransactionMessage")]
pub struct DbTransactionMessage {
    header: DbTransactionMessageHeader,
    account_keys: Vec<Vec<u8>>,
    recent_blockhash: Vec<u8>,
    instructions: Vec<DbCompiledInstruction>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "AddressMapIndexes")]
pub struct DbAddressMapIndexes {
    writable: Vec<i16>,
    readonly: Vec<i16>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "TransactionMessageV0")]
pub struct DbTransactionMessageV0 {
    header: DbTransactionMessageHeader,
    account_keys: Vec<Vec<u8>>,
    recent_blockhash: Vec<u8>,
    instructions: Vec<DbCompiledInstruction>,
    address_map_indexes: Vec<DbAddressMapIndexes>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "MappedAddresses")]
pub struct DbMappedAddresses {
    writable: Vec<Vec<u8>>,
    readonly: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, ToSql)]
#[postgres(name = "MappedMessage")]
pub struct DbMappedMessage {
    message: DbTransactionMessageV0,
    mapped_addresses: DbMappedAddresses,
}

pub struct DbTransaction {
    signature: Vec<u8>,
    is_vote: bool,
    slot: i64,
    message_type: i16,
    legacy_message: Option<DbTransactionMessage>,
    v0_mapped_message: Option<DbMappedMessage>,
    message_hash: Vec<u8>,
    meta: DbTransactionStatusMeta,
    signatures: Vec<Vec<u8>>,
}

impl From<&AddressMapIndexes> for DbAddressMapIndexes {
    fn from(address_map_indexes: &AddressMapIndexes) -> Self {
        Self {
            writable: address_map_indexes
                .writable
                .iter()
                .map(|address_idx| *address_idx as i16)
                .collect(),
            readonly: address_map_indexes
                .readonly
                .iter()
                .map(|address_idx| *address_idx as i16)
                .collect(),
        }
    }
}

impl From<&MappedAddresses> for DbMappedAddresses {
    fn from(mapped_addresses: &MappedAddresses) -> Self {
        Self {
            writable: mapped_addresses
                .writable
                .iter()
                .map(|pubkey| pubkey.as_ref().to_vec())
                .collect(),
            readonly: mapped_addresses
                .readonly
                .iter()
                .map(|pubkey| pubkey.as_ref().to_vec())
                .collect(),
        }
    }
}

impl From<&MessageHeader> for DbTransactionMessageHeader {
    fn from(header: &MessageHeader) -> Self {
        Self {
            num_required_signatures: header.num_required_signatures as i16,
            num_readonly_signed_accounts: header.num_readonly_signed_accounts as i16,
            num_readonly_unsigned_accounts: header.num_readonly_unsigned_accounts as i16,
        }
    }
}

impl From<&CompiledInstruction> for DbCompiledInstruction {
    fn from(instrtuction: &CompiledInstruction) -> Self {
        Self {
            program_id_index: instrtuction.program_id_index as i16,
            accounts: instrtuction
                .accounts
                .iter()
                .map(|account_idx| *account_idx as i16)
                .collect(),
            data: instrtuction.data.clone(),
        }
    }
}

impl From<&Message> for DbTransactionMessage {
    fn from(message: &Message) -> Self {
        Self {
            header: DbTransactionMessageHeader::from(&message.header),
            account_keys: message
                .account_keys
                .iter()
                .map(|key| key.as_ref().to_vec())
                .collect(),
            recent_blockhash: message.recent_blockhash.as_ref().to_vec(),
            instructions: message
                .instructions
                .iter()
                .map(DbCompiledInstruction::from)
                .collect(),
        }
    }
}

impl From<&v0::Message> for DbTransactionMessageV0 {
    fn from(message: &v0::Message) -> Self {
        Self {
            header: DbTransactionMessageHeader::from(&message.header),
            account_keys: message
                .account_keys
                .iter()
                .map(|key| key.as_ref().to_vec())
                .collect(),
            recent_blockhash: message.recent_blockhash.as_ref().to_vec(),
            instructions: message
                .instructions
                .iter()
                .map(DbCompiledInstruction::from)
                .collect(),
            address_map_indexes: message
                .address_map_indexes
                .iter()
                .map(DbAddressMapIndexes::from)
                .collect(),
        }
    }
}

impl From<&MappedMessage> for DbMappedMessage {
    fn from(message: &MappedMessage) -> Self {
        Self {
            message: DbTransactionMessageV0::from(&message.message),
            mapped_addresses: DbMappedAddresses::from(&message.mapped_addresses),
        }
    }
}

impl From<&InnerInstructions> for DbInnerInstructions {
    fn from(instructions: &InnerInstructions) -> Self {
        Self {
            index: instructions.index as i16,
            instructions: instructions
                .instructions
                .iter()
                .map(DbCompiledInstruction::from)
                .collect(),
        }
    }
}

fn get_reward_type(reward: &Option<RewardType>) -> Option<String> {
    reward.as_ref().map(|reward_type| match reward_type {
        RewardType::Fee => "fee".to_string(),
        RewardType::Rent => "rent".to_string(),
        RewardType::Staking => "staking".to_string(),
        RewardType::Voting => "voting".to_string(),
    })
}

impl From<&Reward> for DbReward {
    fn from(reward: &Reward) -> Self {
        Self {
            pubkey: reward.pubkey.clone(),
            lamports: reward.lamports as i64,
            post_balance: reward.post_balance as i64,
            reward_type: get_reward_type(&reward.reward_type),
            commission: reward
                .commission
                .as_ref()
                .map(|commission| *commission as i16),
        }
    }
}

fn get_transaction_status(result: &Result<(), TransactionError>) -> Option<String> {
    if result.is_ok() {
        return None;
    }

    let err = match result.as_ref().err().unwrap() {
        TransactionError::AccountInUse => "AccountInUse",
        TransactionError::AccountLoadedTwice => "AccountLoadedTwice",
        TransactionError::AccountNotFound => "AccountNotFound",
        TransactionError::ProgramAccountNotFound => "ProgramAccountNotFound",
        TransactionError::InsufficientFundsForFee => "InsufficientFundsForFee",
        TransactionError::InvalidAccountForFee => "InvalidAccountForFee",
        TransactionError::AlreadyProcessed => "AlreadyProcessed",
        TransactionError::BlockhashNotFound => "BlockhashNotFound",
        TransactionError::InstructionError(idx, error) => {
            return Some(format!("InstructionError: idx ({}), error: {}", idx, error));
        }
        TransactionError::CallChainTooDeep => "CallChainTooDeep",
        TransactionError::MissingSignatureForFee => "MissingSignatureForFee",
        TransactionError::InvalidAccountIndex => "InvalidAccountIndex",
        TransactionError::SignatureFailure => "SignatureFailure",
        TransactionError::InvalidProgramForExecution => "InvalidProgramForExecution",
        TransactionError::SanitizeFailure => "SanitizeFailure",
        TransactionError::ClusterMaintenance => "ClusterMaintenance",
        TransactionError::AccountBorrowOutstanding => "AccountBorrowOutstanding",
        TransactionError::WouldExceedMaxBlockCostLimit => "WouldExceedMaxBlockCostLimit",
        TransactionError::UnsupportedVersion => "UnsupportedVersion",
        TransactionError::InvalidWritableAccount => "InvalidWritableAccount",
    };

    Some(err.to_string())
}

impl From<&TransactionTokenBalance> for DbTransactionTokenBalance {
    fn from(token_balance: &TransactionTokenBalance) -> Self {
        Self {
            account_index: token_balance.account_index as i16,
            mint: token_balance.mint.clone(),
            ui_token_amount: token_balance.ui_token_amount.ui_amount,
            owner: token_balance.owner.clone(),
        }
    }
}

impl From<&TransactionStatusMeta> for DbTransactionStatusMeta {
    fn from(meta: &TransactionStatusMeta) -> Self {
        Self {
            status: get_transaction_status(&meta.status),
            fee: meta.fee as i64,
            pre_balances: meta
                .pre_balances
                .iter()
                .map(|balance| *balance as i64)
                .collect(),
            post_balances: meta
                .post_balances
                .iter()
                .map(|balance| *balance as i64)
                .collect(),
            inner_instructions: meta
                .inner_instructions
                .as_ref()
                .map(|instructions| instructions.iter().map(DbInnerInstructions::from).collect()),
            log_messages: meta.log_messages.clone(),
            pre_token_balances: meta.pre_token_balances.as_ref().map(|balances| {
                balances
                    .iter()
                    .map(DbTransactionTokenBalance::from)
                    .collect()
            }),
            post_token_balances: meta.post_token_balances.as_ref().map(|balances| {
                balances
                    .iter()
                    .map(DbTransactionTokenBalance::from)
                    .collect()
            }),
            rewards: meta
                .rewards
                .as_ref()
                .map(|rewards| rewards.iter().map(DbReward::from).collect()),
        }
    }
}

pub(crate) fn abort() -> ! {
    #[cfg(not(test))]
    {
        // standard error is usually redirected to a log file, cry for help on standard output as
        // well
        eprintln!("Validator process aborted. The validator log may contain further details");
        std::process::exit(1);
    }

    #[cfg(test)]
    panic!("process::exit(1) is intercepted for friendly test failure...");
}

impl DbAccountInfo {
    fn new<T: ReadableAccountInfo>(account: &T, slot: u64) -> DbAccountInfo {
        let data = account.data().to_vec();
        Self {
            pubkey: account.pubkey().to_vec(),
            lamports: account.lamports() as i64,
            owner: account.owner().to_vec(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch() as i64,
            data,
            slot: slot as i64,
            write_version: account.write_version(),
        }
    }
}

pub trait ReadableAccountInfo: Sized {
    fn pubkey(&self) -> &[u8];
    fn owner(&self) -> &[u8];
    fn lamports(&self) -> i64;
    fn executable(&self) -> bool;
    fn rent_epoch(&self) -> i64;
    fn data(&self) -> &[u8];
    fn write_version(&self) -> i64;
}

impl ReadableAccountInfo for DbAccountInfo {
    fn pubkey(&self) -> &[u8] {
        &self.pubkey
    }

    fn owner(&self) -> &[u8] {
        &self.owner
    }

    fn lamports(&self) -> i64 {
        self.lamports
    }

    fn executable(&self) -> bool {
        self.executable
    }

    fn rent_epoch(&self) -> i64 {
        self.rent_epoch
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn write_version(&self) -> i64 {
        self.write_version
    }
}

impl<'a> ReadableAccountInfo for ReplicaAccountInfo<'a> {
    fn pubkey(&self) -> &[u8] {
        self.pubkey
    }

    fn owner(&self) -> &[u8] {
        self.owner
    }

    fn lamports(&self) -> i64 {
        self.lamports as i64
    }

    fn executable(&self) -> bool {
        self.executable
    }

    fn rent_epoch(&self) -> i64 {
        self.rent_epoch as i64
    }

    fn data(&self) -> &[u8] {
        self.data
    }

    fn write_version(&self) -> i64 {
        self.write_version as i64
    }
}

pub trait PostgresClient {
    fn join(&mut self) -> thread::Result<()> {
        Ok(())
    }

    fn update_account(
        &mut self,
        account: DbAccountInfo,
        is_startup: bool,
    ) -> Result<(), AccountsDbPluginError>;

    fn update_slot_status(
        &mut self,
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<(), AccountsDbPluginError>;

    fn notify_end_of_startup(&mut self) -> Result<(), AccountsDbPluginError>;

    fn log_transaction(
        &mut self,
        transaction_log_info: LogTransactionRequest,
    ) -> Result<(), AccountsDbPluginError>;
}

impl SimplePostgresClient {
    fn connect_to_db(
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Client, AccountsDbPluginError> {
        let port = config.port.unwrap_or(DEFAULT_POSTGRES_PORT);

        let connection_str = if let Some(connection_str) = &config.connection_str {
            connection_str.clone()
        } else {
            if config.host.is_none() || config.user.is_none() {
                let msg = format!(
                    "\"connection_str\": {:?}, or \"host\": {:?} \"user\": {:?} must be specified",
                    config.connection_str, config.host, config.user
                );
                return Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginPostgresError::ConfigurationError { msg },
                )));
            }
            format!(
                "host={} user={} port={}",
                config.host.as_ref().unwrap(),
                config.user.as_ref().unwrap(),
                port
            )
        };

        match Client::connect(&connection_str, NoTls) {
            Err(err) => {
                let msg = format!(
                    "Error in connecting to the PostgreSQL database: {:?} connection_str: {:?}",
                    err, connection_str
                );
                error!("{}", msg);
                Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginPostgresError::DataStoreConnectionError { msg },
                )))
            }
            Ok(client) => Ok(client),
        }
    }

    fn build_bulk_account_insert_statement(
        client: &mut Client,
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Statement, AccountsDbPluginError> {
        let batch_size = config
            .batch_size
            .unwrap_or(DEFAULT_ACCOUNTS_INSERT_BATCH_SIZE);
        let mut stmt = String::from("INSERT INTO account AS acct (pubkey, slot, owner, lamports, executable, rent_epoch, data, write_version, updated_on) VALUES");
        for j in 0..batch_size {
            let row = j * ACCOUNT_COLUMN_COUNT;
            let val_str = format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                row + 1,
                row + 2,
                row + 3,
                row + 4,
                row + 5,
                row + 6,
                row + 7,
                row + 8,
                row + 9,
            );

            if j == 0 {
                stmt = format!("{} {}", &stmt, val_str);
            } else {
                stmt = format!("{}, {}", &stmt, val_str);
            }
        }

        let handle_conflict = "ON CONFLICT (pubkey) DO UPDATE SET slot=excluded.slot, owner=excluded.owner, lamports=excluded.lamports, executable=excluded.executable, rent_epoch=excluded.rent_epoch, \
            data=excluded.data, write_version=excluded.write_version, updated_on=excluded.updated_on WHERE acct.slot < excluded.slot OR (\
            acct.slot = excluded.slot AND acct.write_version < excluded.write_version)";

        stmt = format!("{} {}", stmt, handle_conflict);

        info!("{}", stmt);
        let bulk_stmt = client.prepare(&stmt);

        match bulk_stmt {
            Err(err) => {
                return Err(AccountsDbPluginError::Custom(Box::new(AccountsDbPluginPostgresError::DataSchemaError {
                    msg: format!(
                        "Error in preparing for the accounts update PostgreSQL database: {} host: {:?} user: {:?} config: {:?}",
                        err, config.host, config.user, config
                    ),
                })));
            }
            Ok(update_account_stmt) => Ok(update_account_stmt),
        }
    }

    fn build_single_account_upsert_statement(
        client: &mut Client,
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Statement, AccountsDbPluginError> {
        let stmt = "INSERT INTO account AS acct (pubkey, slot, owner, lamports, executable, rent_epoch, data, write_version, updated_on) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
        ON CONFLICT (pubkey) DO UPDATE SET slot=excluded.slot, owner=excluded.owner, lamports=excluded.lamports, executable=excluded.executable, rent_epoch=excluded.rent_epoch, \
        data=excluded.data, write_version=excluded.write_version, updated_on=excluded.updated_on  WHERE acct.slot < excluded.slot OR (\
        acct.slot = excluded.slot AND acct.write_version < excluded.write_version)";

        let stmt = client.prepare(stmt);

        match stmt {
            Err(err) => {
                return Err(AccountsDbPluginError::Custom(Box::new(AccountsDbPluginPostgresError::DataSchemaError {
                    msg: format!(
                        "Error in preparing for the accounts update PostgreSQL database: {} host: {:?} user: {:?} config: {:?}",
                        err, config.host, config.user, config
                    ),
                })));
            }
            Ok(update_account_stmt) => Ok(update_account_stmt),
        }
    }

    fn build_transaction_log_upsert_statement(
        client: &mut Client,
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Statement, AccountsDbPluginError> {
        let stmt = "INSERT INTO transaction AS txn (signature, is_vote, slot, message_type, legacy_message, \
        v0_mapped_message, signatures, message_hash, meta, updated_on) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)";

        let stmt = client.prepare(stmt);

        match stmt {
            Err(err) => {
                return Err(AccountsDbPluginError::Custom(Box::new(AccountsDbPluginPostgresError::DataSchemaError {
                    msg: format!(
                        "Error in preparing for the transaction update PostgreSQL database: ({}) host: {:?} user: {:?} config: {:?}",
                        err, config.host, config.user, config
                    ),
                })));
            }
            Ok(stmt) => Ok(stmt),
        }
    }

    fn build_slot_upsert_statement_with_parent(
        client: &mut Client,
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Statement, AccountsDbPluginError> {
        let stmt = "INSERT INTO slot (slot, parent, status, updated_on) \
        VALUES ($1, $2, $3, $4) \
        ON CONFLICT (slot) DO UPDATE SET parent=excluded.parent, status=excluded.status, updated_on=excluded.updated_on";

        let stmt = client.prepare(stmt);

        match stmt {
            Err(err) => {
                return Err(AccountsDbPluginError::Custom(Box::new(AccountsDbPluginPostgresError::DataSchemaError {
                    msg: format!(
                        "Error in preparing for the slot update PostgreSQL database: {} host: {:?} user: {:?} config: {:?}",
                        err, config.host, config.user, config
                    ),
                })));
            }
            Ok(stmt) => Ok(stmt),
        }
    }

    fn build_slot_upsert_statement_without_parent(
        client: &mut Client,
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<Statement, AccountsDbPluginError> {
        let stmt = "INSERT INTO slot (slot, status, updated_on) \
        VALUES ($1, $2, $3) \
        ON CONFLICT (slot) DO UPDATE SET status=excluded.status, updated_on=excluded.updated_on";

        let stmt = client.prepare(stmt);

        match stmt {
            Err(err) => {
                return Err(AccountsDbPluginError::Custom(Box::new(AccountsDbPluginPostgresError::DataSchemaError {
                    msg: format!(
                        "Error in preparing for the slot update PostgreSQL database: {} host: {:?} user: {:?} config: {:?}",
                        err, config.host, config.user, config
                    ),
                })));
            }
            Ok(stmt) => Ok(stmt),
        }
    }

    /// Internal function for updating or inserting a single account
    fn upsert_account_internal(
        account: &DbAccountInfo,
        statement: &Statement,
        client: &mut Client,
    ) -> Result<(), AccountsDbPluginError> {
        let lamports = account.lamports() as i64;
        let rent_epoch = account.rent_epoch() as i64;
        let updated_on = Utc::now().naive_utc();
        let result = client.query(
            statement,
            &[
                &account.pubkey(),
                &account.slot,
                &account.owner(),
                &lamports,
                &account.executable(),
                &rent_epoch,
                &account.data(),
                &account.write_version(),
                &updated_on,
            ],
        );

        if let Err(err) = result {
            let msg = format!(
                "Failed to persist the update of account to the PostgreSQL database. Error: {:?}",
                err
            );
            error!("{}", msg);
            return Err(AccountsDbPluginError::AccountsUpdateError { msg });
        }

        Ok(())
    }

    /// Update or insert a single account
    fn upsert_account(&mut self, account: &DbAccountInfo) -> Result<(), AccountsDbPluginError> {
        let client = self.client.get_mut().unwrap();
        let statement = &client.update_account_stmt;
        let client = &mut client.client;
        Self::upsert_account_internal(account, statement, client)
    }

    /// Insert accounts in batch to reduce network overhead
    fn insert_accounts_in_batch(
        &mut self,
        account: DbAccountInfo,
    ) -> Result<(), AccountsDbPluginError> {
        self.pending_account_updates.push(account);

        if self.pending_account_updates.len() == self.batch_size {
            let mut measure = Measure::start("accountsdb-plugin-postgres-prepare-values");

            let mut values: Vec<&(dyn types::ToSql + Sync)> =
                Vec::with_capacity(self.batch_size * ACCOUNT_COLUMN_COUNT);
            let updated_on = Utc::now().naive_utc();
            for j in 0..self.batch_size {
                let account = &self.pending_account_updates[j];

                values.push(&account.pubkey);
                values.push(&account.slot);
                values.push(&account.owner);
                values.push(&account.lamports);
                values.push(&account.executable);
                values.push(&account.rent_epoch);
                values.push(&account.data);
                values.push(&account.write_version);
                values.push(&updated_on);
            }
            measure.stop();
            inc_new_counter_debug!(
                "accountsdb-plugin-postgres-prepare-values-us",
                measure.as_us() as usize,
                10000,
                10000
            );

            let mut measure = Measure::start("accountsdb-plugin-postgres-update-account");
            let client = self.client.get_mut().unwrap();
            let result = client
                .client
                .query(&client.bulk_account_insert_stmt, &values);

            self.pending_account_updates.clear();
            if let Err(err) = result {
                let msg = format!(
                    "Failed to persist the update of account to the PostgreSQL database. Error: {:?}",
                    err
                );
                error!("{}", msg);
                return Err(AccountsDbPluginError::AccountsUpdateError { msg });
            }
            measure.stop();
            inc_new_counter_debug!(
                "accountsdb-plugin-postgres-update-account-us",
                measure.as_us() as usize,
                10000,
                10000
            );
            inc_new_counter_debug!(
                "accountsdb-plugin-postgres-update-account-count",
                self.batch_size,
                10000,
                10000
            );
        }
        Ok(())
    }

    /// Flush any left over accounts in batch which are not processed in the last batch
    fn flush_buffered_writes(&mut self) -> Result<(), AccountsDbPluginError> {
        if self.pending_account_updates.is_empty() {
            return Ok(());
        }

        let client = self.client.get_mut().unwrap();
        let statement = &client.update_account_stmt;
        let client = &mut client.client;

        for account in self.pending_account_updates.drain(..) {
            Self::upsert_account_internal(&account, statement, client)?;
        }

        Ok(())
    }

    pub fn new(config: &AccountsDbPluginPostgresConfig) -> Result<Self, AccountsDbPluginError> {
        info!("Creating SimplePostgresClient...");
        let mut client = Self::connect_to_db(config)?;
        let bulk_account_insert_stmt =
            Self::build_bulk_account_insert_statement(&mut client, config)?;
        let update_account_stmt = Self::build_single_account_upsert_statement(&mut client, config)?;

        let update_slot_with_parent_stmt =
            Self::build_slot_upsert_statement_with_parent(&mut client, config)?;
        let update_slot_without_parent_stmt =
            Self::build_slot_upsert_statement_without_parent(&mut client, config)?;
        let update_transaction_log_stmt =
            Self::build_transaction_log_upsert_statement(&mut client, config)?;

        let batch_size = config
            .batch_size
            .unwrap_or(DEFAULT_ACCOUNTS_INSERT_BATCH_SIZE);
        info!("Created SimplePostgresClient.");
        Ok(Self {
            batch_size,
            pending_account_updates: Vec::with_capacity(batch_size),
            client: Mutex::new(PostgresSqlClientWrapper {
                client,
                update_account_stmt,
                bulk_account_insert_stmt,
                update_slot_with_parent_stmt,
                update_slot_without_parent_stmt,
                update_transaction_log_stmt,
            }),
        })
    }
}

impl PostgresClient for SimplePostgresClient {
    fn update_account(
        &mut self,
        account: DbAccountInfo,
        is_startup: bool,
    ) -> Result<(), AccountsDbPluginError> {
        trace!(
            "Updating account {} with owner {} at slot {}",
            bs58::encode(account.pubkey()).into_string(),
            bs58::encode(account.owner()).into_string(),
            account.slot,
        );
        if !is_startup {
            return self.upsert_account(&account);
        }
        self.insert_accounts_in_batch(account)
    }

    fn update_slot_status(
        &mut self,
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<(), AccountsDbPluginError> {
        info!("Updating slot {:?} at with status {:?}", slot, status);

        let slot = slot as i64; // postgres only supports i64
        let parent = parent.map(|parent| parent as i64);
        let updated_on = Utc::now().naive_utc();
        let status_str = status.as_str();
        let client = self.client.get_mut().unwrap();

        let result = match parent {
            Some(parent) => client.client.execute(
                &client.update_slot_with_parent_stmt,
                &[&slot, &parent, &status_str, &updated_on],
            ),
            None => client.client.execute(
                &client.update_slot_without_parent_stmt,
                &[&slot, &status_str, &updated_on],
            ),
        };

        match result {
            Err(err) => {
                let msg = format!(
                    "Failed to persist the update of slot to the PostgreSQL database. Error: {:?}",
                    err
                );
                error!("{:?}", msg);
                return Err(AccountsDbPluginError::SlotStatusUpdateError { msg });
            }
            Ok(rows) => {
                assert_eq!(1, rows, "Expected one rows to be updated a time");
            }
        }

        Ok(())
    }

    fn notify_end_of_startup(&mut self) -> Result<(), AccountsDbPluginError> {
        self.flush_buffered_writes()
    }

    fn log_transaction(
        &mut self,
        transaction_log_info: LogTransactionRequest,
    ) -> Result<(), AccountsDbPluginError> {
        let client = self.client.get_mut().unwrap();
        let statement = &client.update_transaction_log_stmt;
        let client = &mut client.client;
        let updated_on = Utc::now().naive_utc();

        let transaction_info = transaction_log_info.transaction_info;
        let result = client.query(
            statement,
            &[
                &transaction_info.signature,
                &transaction_info.is_vote,
                &transaction_info.slot,
                &transaction_info.message_type,
                &transaction_info.legacy_message,
                &transaction_info.v0_mapped_message,
                &transaction_info.signatures,
                &transaction_info.message_hash,
                &transaction_info.meta,
                &updated_on,
            ],
        );

        if let Err(err) = result {
            let msg = format!(
                "Failed to persist the update of transaction log to the PostgreSQL database. Error: {:?}",
                err
            );
            error!("{}", msg);
            return Err(AccountsDbPluginError::AccountsUpdateError { msg });
        }

        Ok(())
    }
}

struct UpdateAccountRequest {
    account: DbAccountInfo,
    is_startup: bool,
}

struct UpdateSlotRequest {
    slot: u64,
    parent: Option<u64>,
    slot_status: SlotStatus,
}

pub struct LogTransactionRequest {
    transaction_info: DbTransaction,
}

#[warn(clippy::large_enum_variant)]
enum DbWorkItem {
    UpdateAccount(Box<UpdateAccountRequest>),
    UpdateSlot(Box<UpdateSlotRequest>),
    LogTransaction(Box<LogTransactionRequest>),
}

impl PostgresClientWorker {
    fn new(config: AccountsDbPluginPostgresConfig) -> Result<Self, AccountsDbPluginError> {
        let result = SimplePostgresClient::new(&config);
        match result {
            Ok(client) => Ok(PostgresClientWorker {
                client,
                is_startup_done: false,
            }),
            Err(err) => {
                error!("Error in creating SimplePostgresClient: {}", err);
                Err(err)
            }
        }
    }

    fn do_work(
        &mut self,
        receiver: Receiver<DbWorkItem>,
        exit_worker: Arc<AtomicBool>,
        is_startup_done: Arc<AtomicBool>,
        startup_done_count: Arc<AtomicUsize>,
        panic_on_db_errors: bool,
    ) -> Result<(), AccountsDbPluginError> {
        while !exit_worker.load(Ordering::Relaxed) {
            let mut measure = Measure::start("accountsdb-plugin-postgres-worker-recv");
            let work = receiver.recv_timeout(Duration::from_millis(500));
            measure.stop();
            inc_new_counter_debug!(
                "accountsdb-plugin-postgres-worker-recv-us",
                measure.as_us() as usize,
                100000,
                100000
            );
            match work {
                Ok(work) => match work {
                    DbWorkItem::UpdateAccount(request) => {
                        if let Err(err) = self
                            .client
                            .update_account(request.account, request.is_startup)
                        {
                            error!("Failed to update account: ({})", err);
                            if panic_on_db_errors {
                                abort();
                            }
                        }
                    }
                    DbWorkItem::UpdateSlot(request) => {
                        if let Err(err) = self.client.update_slot_status(
                            request.slot,
                            request.parent,
                            request.slot_status,
                        ) {
                            error!("Failed to update slot: ({})", err);
                            if panic_on_db_errors {
                                abort();
                            }
                        }
                    }
                    DbWorkItem::LogTransaction(transaction_log_info) => {
                        self.client.log_transaction(*transaction_log_info)?;
                    }
                },
                Err(err) => match err {
                    RecvTimeoutError::Timeout => {
                        if !self.is_startup_done && is_startup_done.load(Ordering::Relaxed) {
                            if let Err(err) = self.client.notify_end_of_startup() {
                                error!("Error in notifying end of startup: ({})", err);
                                if panic_on_db_errors {
                                    abort();
                                }
                            }
                            self.is_startup_done = true;
                            startup_done_count.fetch_add(1, Ordering::Relaxed);
                        }

                        continue;
                    }
                    _ => {
                        error!("Error in receiving the item {:?}", err);
                        if panic_on_db_errors {
                            abort();
                        }
                        break;
                    }
                },
            }
        }
        Ok(())
    }
}
pub struct ParallelPostgresClient {
    workers: Vec<JoinHandle<Result<(), AccountsDbPluginError>>>,
    exit_worker: Arc<AtomicBool>,
    is_startup_done: Arc<AtomicBool>,
    startup_done_count: Arc<AtomicUsize>,
    initialized_worker_count: Arc<AtomicUsize>,
    sender: Sender<DbWorkItem>,
    last_report: AtomicInterval,
}

impl ParallelPostgresClient {
    pub fn new(config: &AccountsDbPluginPostgresConfig) -> Result<Self, AccountsDbPluginError> {
        info!("Creating ParallelPostgresClient...");
        let (sender, receiver) = bounded(MAX_ASYNC_REQUESTS);
        let exit_worker = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::default();
        let is_startup_done = Arc::new(AtomicBool::new(false));
        let startup_done_count = Arc::new(AtomicUsize::new(0));
        let worker_count = config.threads.unwrap_or(DEFAULT_THREADS_COUNT);
        let initialized_worker_count = Arc::new(AtomicUsize::new(0));
        for i in 0..worker_count {
            let cloned_receiver = receiver.clone();
            let exit_clone = exit_worker.clone();
            let is_startup_done_clone = is_startup_done.clone();
            let startup_done_count_clone = startup_done_count.clone();
            let initialized_worker_count_clone = initialized_worker_count.clone();
            let config = config.clone();
            let worker = Builder::new()
                .name(format!("worker-{}", i))
                .spawn(move || -> Result<(), AccountsDbPluginError> {
                    let panic_on_db_errors = *config
                        .panic_on_db_errors
                        .as_ref()
                        .unwrap_or(&DEFAULT_PANIC_ON_DB_ERROR);
                    let result = PostgresClientWorker::new(config);

                    match result {
                        Ok(mut worker) => {
                            initialized_worker_count_clone.fetch_add(1, Ordering::Relaxed);
                            worker.do_work(
                                cloned_receiver,
                                exit_clone,
                                is_startup_done_clone,
                                startup_done_count_clone,
                                panic_on_db_errors,
                            )?;
                            Ok(())
                        }
                        Err(err) => {
                            error!("Error when making connection to database: ({})", err);
                            if panic_on_db_errors {
                                abort();
                            }
                            Err(err)
                        }
                    }
                })
                .unwrap();

            workers.push(worker);
        }

        info!("Created ParallelPostgresClient.");
        Ok(Self {
            last_report: AtomicInterval::default(),
            workers,
            exit_worker,
            is_startup_done,
            startup_done_count,
            initialized_worker_count,
            sender,
        })
    }

    pub fn join(&mut self) -> thread::Result<()> {
        self.exit_worker.store(true, Ordering::Relaxed);
        while !self.workers.is_empty() {
            let worker = self.workers.pop();
            if worker.is_none() {
                break;
            }
            let worker = worker.unwrap();
            let result = worker.join().unwrap();
            if result.is_err() {
                error!("The worker thread has failed: {:?}", result);
            }
        }

        Ok(())
    }

    pub fn update_account(
        &mut self,
        account: &ReplicaAccountInfo,
        slot: u64,
        is_startup: bool,
    ) -> Result<(), AccountsDbPluginError> {
        if self.last_report.should_update(30000) {
            datapoint_debug!(
                "postgres-plugin-stats",
                ("message-queue-length", self.sender.len() as i64, i64),
            );
        }
        let mut measure = Measure::start("accountsdb-plugin-posgres-create-work-item");
        let wrk_item = DbWorkItem::UpdateAccount(Box::new(UpdateAccountRequest {
            account: DbAccountInfo::new(account, slot),
            is_startup,
        }));

        measure.stop();

        inc_new_counter_debug!(
            "accountsdb-plugin-posgres-create-work-item-us",
            measure.as_us() as usize,
            100000,
            100000
        );

        let mut measure = Measure::start("accountsdb-plugin-posgres-send-msg");

        if let Err(err) = self.sender.send(wrk_item) {
            return Err(AccountsDbPluginError::AccountsUpdateError {
                msg: format!(
                    "Failed to update the account {:?}, error: {:?}",
                    bs58::encode(account.pubkey()).into_string(),
                    err
                ),
            });
        }

        measure.stop();
        inc_new_counter_debug!(
            "accountsdb-plugin-posgres-send-msg-us",
            measure.as_us() as usize,
            100000,
            100000
        );

        Ok(())
    }

    pub fn update_slot_status(
        &mut self,
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<(), AccountsDbPluginError> {
        if let Err(err) = self
            .sender
            .send(DbWorkItem::UpdateSlot(Box::new(UpdateSlotRequest {
                slot,
                parent,
                slot_status: status,
            })))
        {
            return Err(AccountsDbPluginError::SlotStatusUpdateError {
                msg: format!("Failed to update the slot {:?}, error: {:?}", slot, err),
            });
        }
        Ok(())
    }

    pub fn notify_end_of_startup(&mut self) -> Result<(), AccountsDbPluginError> {
        info!("Notifying the end of startup");
        // Ensure all items in the queue has been received by the workers
        while !self.sender.is_empty() {
            sleep(Duration::from_millis(100));
        }
        self.is_startup_done.store(true, Ordering::Relaxed);

        // Wait for all worker threads to be done with flushing
        while self.startup_done_count.load(Ordering::Relaxed)
            != self.initialized_worker_count.load(Ordering::Relaxed)
        {
            info!(
                "Startup done count: {}, good worker thread count: {}",
                self.startup_done_count.load(Ordering::Relaxed),
                self.initialized_worker_count.load(Ordering::Relaxed)
            );
            sleep(Duration::from_millis(100));
        }

        info!("Done with notifying the end of startup");
        Ok(())
    }

    fn build_db_transaction(slot: u64, transaction_info: &ReplicaTransactionInfo) -> DbTransaction {
        DbTransaction {
            signature: transaction_info.signature.as_ref().to_vec(),
            is_vote: transaction_info.is_vote,
            slot: slot as i64,
            message_type: match transaction_info.transaction.message() {
                SanitizedMessage::Legacy(_) => 0,
                SanitizedMessage::V0(_) => 1,
            },
            legacy_message: match transaction_info.transaction.message() {
                SanitizedMessage::Legacy(legacy_message) => {
                    Some(DbTransactionMessage::from(legacy_message))
                }
                _ => None,
            },
            v0_mapped_message: match transaction_info.transaction.message() {
                SanitizedMessage::V0(mapped_message) => Some(DbMappedMessage::from(mapped_message)),
                _ => None,
            },
            signatures: transaction_info
                .transaction
                .signatures()
                .iter()
                .map(|signature| signature.as_ref().to_vec())
                .collect(),
            message_hash: transaction_info
                .transaction
                .message_hash()
                .as_ref()
                .to_vec(),
            meta: DbTransactionStatusMeta::from(transaction_info.transaction_status_meta),
        }
    }

    fn build_transaction_request(
        slot: u64,
        transaction_info: &ReplicaTransactionInfo,
    ) -> LogTransactionRequest {
        LogTransactionRequest {
            transaction_info: Self::build_db_transaction(slot, transaction_info),
        }
    }

    pub fn log_transaction_info(
        &mut self,
        transaction_info: &ReplicaTransactionInfo,
        slot: u64,
    ) -> Result<(), AccountsDbPluginError> {
        let wrk_item = DbWorkItem::LogTransaction(Box::new(Self::build_transaction_request(
            slot,
            transaction_info,
        )));

        if let Err(err) = self.sender.send(wrk_item) {
            return Err(AccountsDbPluginError::SlotStatusUpdateError {
                msg: format!("Failed to update the transaction, error: {:?}", err),
            });
        }
        Ok(())
    }
}

pub struct PostgresClientBuilder {}

impl PostgresClientBuilder {
    pub fn build_pararallel_postgres_client(
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<ParallelPostgresClient, AccountsDbPluginError> {
        ParallelPostgresClient::new(config)
    }

    pub fn build_simple_postgres_client(
        config: &AccountsDbPluginPostgresConfig,
    ) -> Result<SimplePostgresClient, AccountsDbPluginError> {
        SimplePostgresClient::new(config)
    }
}
