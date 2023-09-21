use {
    super::VersionedTransaction,
    crate::{
        compute_budget_processor::process_compute_budget_instruction, feature_set::FeatureSet,
        message::VersionedMessage, sanitize::SanitizeError, signature::Signature,
        simple_vote_transaction_checker::is_simple_vote_transaction,
        transaction_meta::TransactionMeta,
    },
    solana_program::message::SanitizedVersionedMessage,
};

/// Wraps a sanitized `VersionedTransaction` to provide a safe API
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedVersionedTransaction {
    /// List of signatures
    pub(crate) signatures: Vec<Signature>,
    /// Message to sign.
    pub(crate) message: SanitizedVersionedMessage,
    /// Meta data
    pub(crate) transaction_meta: TransactionMeta,
}

impl SanitizedVersionedTransaction {
    pub fn try_new(
        tx: VersionedTransaction,
        is_simple_vote_tx: Option<bool>,
        feature_set: &FeatureSet,
    ) -> Result<Self, SanitizeError> {
        tx.sanitize_signatures()?;
        let sanitized_versioned_message = SanitizedVersionedMessage::try_from(tx.message)?;
        let mut transaction_meta = process_compute_budget_instruction(
            sanitized_versioned_message.program_instructions_iter(),
            feature_set,
            None, //cluster type
        )?;

        transaction_meta.is_simple_vote_tx = is_simple_vote_tx.unwrap_or_else(|| {
            is_simple_vote_transaction(
                tx.signatures.len(),
                sanitized_versioned_message.instructions().len(),
                matches!(
                    sanitized_versioned_message.message,
                    VersionedMessage::Legacy(_)
                ),
                sanitized_versioned_message.program_instructions_iter(),
            )
        });

        Ok(Self {
            signatures: tx.signatures,
            message: sanitized_versioned_message,
            transaction_meta,
        })
    }

    pub fn get_message(&self) -> &SanitizedVersionedMessage {
        &self.message
    }

    pub fn get_transaction_meta(&self) -> &TransactionMeta {
        &self.transaction_meta
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_program::{
            hash::Hash,
            message::{v0, VersionedMessage},
            pubkey::Pubkey,
        },
    };

    #[test]
    fn test_try_new_with_invalid_signatures() {
        let tx = VersionedTransaction {
            signatures: vec![],
            message: VersionedMessage::V0(
                v0::Message::try_compile(&Pubkey::new_unique(), &[], &[], Hash::default()).unwrap(),
            ),
        };

        assert_eq!(
            SanitizedVersionedTransaction::try_new(tx),
            Err(SanitizeError::IndexOutOfBounds)
        );
    }

    #[test]
    fn test_try_new() {
        let mut message =
            v0::Message::try_compile(&Pubkey::new_unique(), &[], &[], Hash::default()).unwrap();
        message.header.num_readonly_signed_accounts += 1;

        let tx = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::V0(message),
        };

        assert_eq!(
            SanitizedVersionedTransaction::try_new(tx),
            Err(SanitizeError::InvalidValue)
        );
    }
}
