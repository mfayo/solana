use log::*;
use solana_sdk::account::KeyedAccount;
use solana_sdk::credit_only_account::KeyedCreditOnlyAccount;
use solana_sdk::instruction::InstructionError;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::solana_entrypoint;

solana_entrypoint!(entrypoint);
fn entrypoint(
    program_id: &Pubkey,
    keyed_accounts: &mut [KeyedAccount],
    keyed_credit_only_accounts: &mut [KeyedCreditOnlyAccount],
    data: &[u8],
    tick_height: u64,
) -> Result<(), InstructionError> {
    solana_logger::setup();
    trace!("noop: program_id: {:?}", program_id);
    trace!("noop: keyed_accounts: {:#?}", keyed_accounts);
    trace!(
        "noop: keyed_credit_only_accounts: {:#?}",
        keyed_credit_only_accounts
    );
    trace!("noop: data: {:?}", data);
    trace!("noop: tick_height: {:?}", tick_height);
    Ok(())
}
