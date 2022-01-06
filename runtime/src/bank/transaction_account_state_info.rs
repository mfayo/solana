use {
    crate::{
        account_rent_state::{check_rent_state, RentState},
        bank::Bank,
        message_processor::ProcessedMessageInfo,
    },
    solana_sdk::{
        message::SanitizedMessage, transaction::Result, transaction_context::TransactionContext,
    },
};

pub(crate) struct TransactionAccountStateInfo {
    rent_state: Option<RentState>, // None: readonly account
}

impl Bank {
    pub(crate) fn get_transaction_account_state_info(
        &self,
        transaction_context: &TransactionContext,
        message: &SanitizedMessage,
    ) -> Vec<TransactionAccountStateInfo> {
        (0..transaction_context.get_number_of_accounts())
            .map(|i| {
                let rent_state = if message.is_writable(i) {
                    let account = transaction_context.get_account_at_index(i).borrow();
                    Some(RentState::from_account(
                        &account,
                        &self.rent_collector().rent,
                    ))
                } else {
                    None
                };
                TransactionAccountStateInfo { rent_state }
            })
            .collect()
    }

    pub(crate) fn verify_transaction_account_state_changes(
        process_result: &mut Result<ProcessedMessageInfo>,
        pre_state_infos: &[TransactionAccountStateInfo],
        post_state_infos: &[TransactionAccountStateInfo],
        transaction_context: &TransactionContext,
    ) {
        if process_result.is_ok() {
            for (i, (pre_state_info, post_state_info)) in
                pre_state_infos.iter().zip(post_state_infos).enumerate()
            {
                if let Err(err) = check_rent_state(
                    pre_state_info.rent_state.as_ref(),
                    post_state_info.rent_state.as_ref(),
                    transaction_context,
                    i,
                ) {
                    *process_result = Err(err)
                }
            }
        }
    }
}
