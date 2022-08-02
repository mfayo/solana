mod bank_metrics;
pub mod banks_with_commitments;
mod cluster_metrics;
mod utils;
pub mod identity_info;

use banks_with_commitments::BanksWithCommitments;
use solana_gossip::cluster_info::ClusterInfo;
use solana_sdk::pubkey::Pubkey;
use std::{collections::HashSet, sync::Arc};
use identity_info::IdentityInfoMap;

#[derive(Clone, Copy)]
pub struct Lamports(pub u64);

pub fn render_prometheus(
    banks_with_commitments: BanksWithCommitments,
    cluster_info: &Arc<ClusterInfo>,
    vote_accounts: &Arc<HashSet<Pubkey>>,
    identity_config: &Arc<IdentityInfoMap>,
) -> Vec<u8> {
    // There are 3 levels of commitment for a bank:
    // - finalized: most recent block *confirmed* by supermajority of the
    // cluster.
    // - confirmed: most recent block that has been *voted* on by supermajority
    // of the cluster.
    // - processed: most recent block.
    let mut out: Vec<u8> = Vec::new();
    bank_metrics::write_bank_metrics(&banks_with_commitments, &mut out).expect("IO error");
    cluster_metrics::write_cluster_metrics(
        &banks_with_commitments,
        &cluster_info,
        vote_accounts,
        identity_config,
        &mut out,
    )
    .expect("IO error");
    out
}
