//! The `shred_fetch_stage` pulls shreds from UDP sockets and sends it to a channel.

use {
    crate::{
        cluster_nodes::check_feature_activation, packet_hasher::PacketHasher,
        repair_service::RepairTransportConfig, serve_repair::ServeRepair,
        tpu::MAX_QUIC_CONNECTIONS_PER_PEER, tvu::RepairQuicConfig,
    },
    crossbeam_channel::{unbounded, Sender},
    lru::LruCache,
    solana_client::connection_cache::ConnectionCache,
    solana_gossip::cluster_info::ClusterInfo,
    solana_ledger::shred::{should_discard_shred, ShredFetchStats},
    solana_perf::packet::{Packet, PacketBatch, PacketBatchRecycler, PacketFlags},
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_sdk::{
        clock::{Slot, DEFAULT_MS_PER_SLOT},
        feature_set,
        signer::Signer,
    },
    solana_streamer::{
        quic::{spawn_server, StreamStats, MAX_STAKED_CONNECTIONS, MAX_UNSTAKED_CONNECTIONS},
        streamer::{self, PacketBatchReceiver, StreamerReceiveStats},
    },
    std::{
        net::UdpSocket,
        sync::{atomic::AtomicBool, Arc, RwLock},
        thread::{self, Builder, JoinHandle},
        time::{Duration, Instant},
    },
};

const DEFAULT_LRU_SIZE: usize = 10_000;

pub(crate) struct ShredFetchStage {
    thread_hdls: Vec<JoinHandle<()>>,
    // /// The Quic ConnectonCache using the same Quic Endpoint of the Quic based
    // /// streamer receiving shreds. The connection cache can be used for sending
    // /// repair requests.
    // connection_cache: Option<Arc<ConnectionCache>>,
    quic_repair_addr: Option<Arc<UdpSocket>>,
}

impl ShredFetchStage {
    // updates packets received on a channel and sends them on another channel
    fn modify_packets(
        recvr: PacketBatchReceiver,
        sendr: Sender<PacketBatch>,
        bank_forks: &RwLock<BankForks>,
        shred_version: u16,
        name: &'static str,
        flags: PacketFlags,
        repair_context: Option<(RepairTransportConfig, &ClusterInfo)>,
    ) {
        const STATS_SUBMIT_CADENCE: Duration = Duration::from_secs(1);
        let mut shreds_received = LruCache::new(DEFAULT_LRU_SIZE);
        let mut last_updated = Instant::now();
        let mut keypair = repair_context
            .as_ref()
            .map(|(_, cluster_info)| cluster_info.keypair().clone());

        // In the case of bank_forks=None, setup to accept any slot range
        let mut root_bank = bank_forks.read().unwrap().root_bank();
        let mut last_root = 0;
        let mut last_slot = std::u64::MAX;
        let mut slots_per_epoch = 0;

        let mut stats = ShredFetchStats::default();
        let mut packet_hasher = PacketHasher::default();

        for mut packet_batch in recvr {
            if last_updated.elapsed().as_millis() as u64 > DEFAULT_MS_PER_SLOT {
                last_updated = Instant::now();
                packet_hasher.reset();
                shreds_received.clear();
                {
                    let bank_forks_r = bank_forks.read().unwrap();
                    last_root = bank_forks_r.root();
                    let working_bank = bank_forks_r.working_bank();
                    last_slot = working_bank.slot();
                    root_bank = bank_forks_r.root_bank();
                    slots_per_epoch = root_bank.get_slots_in_epoch(root_bank.epoch());
                }
                keypair = repair_context
                    .as_ref()
                    .map(|(_, cluster_info)| cluster_info.keypair().clone());
            }
            stats.shred_count += packet_batch.len();

            if let Some((udp_socket, cluster_info)) = &repair_context {
                info!("Got some repair response at {:?} use quic {}", cluster_info.id(), matches!(udp_socket, RepairTransportConfig::Quic(_)));
                debug_assert_eq!(flags, PacketFlags::REPAIR);
                debug_assert!(keypair.is_some());
                if let Some(ref keypair) = keypair {
                    ServeRepair::handle_repair_response_pings(
                        udp_socket,
                        keypair,
                        &mut packet_batch,
                        &mut stats,
                    );
                }
            }

            // Limit shreds to 2 epochs away.
            let max_slot = last_slot + 2 * slots_per_epoch;
            let should_drop_merkle_shreds =
                |shred_slot| should_drop_merkle_shreds(shred_slot, &root_bank);
            for packet in packet_batch.iter_mut() {
                if should_discard_packet(
                    packet,
                    last_root,
                    max_slot,
                    shred_version,
                    &packet_hasher,
                    &mut shreds_received,
                    should_drop_merkle_shreds,
                    &mut stats,
                ) {
                    packet.meta_mut().set_discard(true);
                } else {
                    packet.meta_mut().flags.insert(flags);
                }
            }
            stats.maybe_submit(name, STATS_SUBMIT_CADENCE);
            if sendr.send(packet_batch).is_err() {
                break;
            }
        }
    }

    fn packet_modifier(
        sockets: Vec<Arc<UdpSocket>>,
        exit: &Arc<AtomicBool>,
        sender: Sender<PacketBatch>,
        recycler: PacketBatchRecycler,
        bank_forks: Arc<RwLock<BankForks>>,
        shred_version: u16,
        name: &'static str,
        flags: PacketFlags,
        repair_context: Option<(Arc<UdpSocket>, Arc<ClusterInfo>)>,
    ) -> (Vec<JoinHandle<()>>, JoinHandle<()>) {
        let (packet_sender, packet_receiver) = unbounded();
        let streamers = sockets
            .into_iter()
            .map(|s| {
                streamer::receiver(
                    s,
                    exit.clone(),
                    packet_sender.clone(),
                    recycler.clone(),
                    Arc::new(StreamerReceiveStats::new("packet_modifier")),
                    1,
                    true,
                    None,
                )
            })
            .collect();
        let modifier_hdl = Builder::new()
            .name("solTvuFetchPMod".to_string())
            .spawn(move || {
                let repair_context = repair_context.as_ref().map(|(socket, cluster_info)| {
                    (
                        RepairTransportConfig::Udp(socket.as_ref()),
                        cluster_info.as_ref(),
                    )
                });
                Self::modify_packets(
                    packet_receiver,
                    sender,
                    &bank_forks,
                    shred_version,
                    name,
                    flags,
                    repair_context,
                )
            })
            .unwrap();
        (streamers, modifier_hdl)
    }

    /// This creates a Quic based streamer and a packet modifier. The streamer forwards PacketBatch to
    /// the packet modifier which in turn sends out the PacketBatch via 'sender'.
    /// In addition, it also creates a connection cache using the same Quic endpoint of the
    /// streamer.
    fn packet_modifier_quic(
        exit: &Arc<AtomicBool>,
        sender: Sender<PacketBatch>,
        bank_forks: Arc<RwLock<BankForks>>,
        shred_version: u16,
        name: &'static str,
        flags: PacketFlags,
        cluster_info: Arc<ClusterInfo>,
        repair_quic_config: &RepairQuicConfig,
    ) -> (JoinHandle<()>, JoinHandle<()>, Arc<ConnectionCache>) {
        let (packet_sender, packet_receiver) = unbounded();

        let stats = Arc::new(StreamStats::default());
        let host = repair_quic_config.repair_address.local_addr().unwrap().ip();
        let (endpoint, repair_quic_t) = spawn_server(
            "PacketModQ".into(),
            repair_quic_config.repair_address.try_clone().unwrap(),
            &repair_quic_config.identity_keypair,
            host,
            packet_sender,
            exit.clone(),
            MAX_QUIC_CONNECTIONS_PER_PEER,
            repair_quic_config.staked_nodes.clone(),
            MAX_STAKED_CONNECTIONS,
            MAX_UNSTAKED_CONNECTIONS,
            stats,
            repair_quic_config.wait_for_chunk_timeout_ms,
            repair_quic_config.repair_packet_coalesce_timeout_ms,
        )
        .unwrap();

        let cert_info = Some((
            &*repair_quic_config.identity_keypair,
            endpoint.local_addr().unwrap().ip(),
        ));
        let staked_nodes = &repair_quic_config.staked_nodes;
        let connection_cache = Arc::new(ConnectionCache::new_with_client_options(
            1,
            Some(endpoint),
            cert_info,
            Some((staked_nodes, &repair_quic_config.identity_keypair.pubkey())),
        ));

        let connection_cache_clone = connection_cache.clone();
        let modifier_hdl = Builder::new()
            .name("solTvuFetchPModQ".to_string())
            .spawn(move || {
                let repair_context = Some((
                    RepairTransportConfig::Quic(connection_cache_clone),
                    cluster_info.as_ref(),
                ));
                info!("Calling modify_packets for quic repair results at {:?}.", cluster_info.id());
                Self::modify_packets(
                    packet_receiver,
                    sender,
                    &bank_forks,
                    shred_version,
                    name,
                    flags,
                    repair_context,
                )
            })
            .unwrap();

        (repair_quic_t, modifier_hdl, connection_cache)
    }

    pub(crate) fn new(
        sockets: Vec<Arc<UdpSocket>>,
        forward_sockets: Vec<Arc<UdpSocket>>,
        repair_socket: Arc<UdpSocket>,
        repair_quic_config: Option<&RepairQuicConfig>,
        sender: Sender<PacketBatch>,
        shred_version: u16,
        bank_forks: Arc<RwLock<BankForks>>,
        cluster_info: Arc<ClusterInfo>,
        exit: &Arc<AtomicBool>,
    ) -> (Option<Arc<ConnectionCache>>, Self) {
        let recycler = PacketBatchRecycler::warmed(100, 1024);

        let (mut tvu_threads, tvu_filter) = Self::packet_modifier(
            sockets,
            exit,
            sender.clone(),
            recycler.clone(),
            bank_forks.clone(),
            shred_version,
            "shred_fetch",
            PacketFlags::empty(),
            None, // repair_context
        );

        let (tvu_forwards_threads, fwd_thread_hdl) = Self::packet_modifier(
            forward_sockets,
            exit,
            sender.clone(),
            recycler.clone(),
            bank_forks.clone(),
            shred_version,
            "shred_fetch_tvu_forwards",
            PacketFlags::FORWARDED,
            None, // repair_context
        );

        let (repair_receiver, repair_handler) = Self::packet_modifier(
            vec![repair_socket.clone()],
            exit,
            sender.clone(),
            recycler,
            bank_forks.clone(),
            shred_version,
            "shred_fetch_repair",
            PacketFlags::REPAIR,
            Some((repair_socket, cluster_info.clone())),
        );

        let (connection_cache, quic_repair_addr, repair_quic_t, quic_repair_modifier_t) =
            if let Some(repair_quic_config) = repair_quic_config {
                let local_addr = repair_quic_config.repair_address.clone();
                let (repair_quic_t, quic_repair_modifier_t, connection_cache) =
                    Self::packet_modifier_quic(
                        exit,
                        sender,
                        bank_forks,
                        shred_version,
                        "shred_fetch_repair_quic",
                        PacketFlags::REPAIR,
                        cluster_info,
                        repair_quic_config,
                    );
                (
                    Some(connection_cache),
                    Some(local_addr),
                    Some(repair_quic_t),
                    Some(quic_repair_modifier_t),
                )
            } else {
                (None, None, None, None)
            };

        tvu_threads.extend(tvu_forwards_threads.into_iter());
        tvu_threads.extend(repair_receiver.into_iter());
        tvu_threads.push(tvu_filter);
        tvu_threads.push(fwd_thread_hdl);
        tvu_threads.push(repair_handler);

        if let Some(repair_quic_t) = repair_quic_t {
            tvu_threads.push(repair_quic_t);
        }

        if let Some(quic_repair_modifier_t) = quic_repair_modifier_t {
            tvu_threads.push(quic_repair_modifier_t);
        }

        (
            connection_cache,
            Self {
                thread_hdls: tvu_threads,
                quic_repair_addr
            },
        )
    }

    pub(crate) fn join(self) -> thread::Result<()> {
        error!("Shutting down PacketModQ quic_repair_add {:?}", self.quic_repair_addr);
        for thread_hdl in self.thread_hdls {
            thread_hdl.join()?;
        }
        Ok(())
    }

    // /// Obtain the quic based ConnectionCache which used the same
    // /// Endpoint receiving the repair responses to send repair requests.
    // pub(crate) fn get_connection_cache(&self) -> Option<Arc<ConnectionCache>> {
    //     self.connection_cache.clone()
    // }
}

// Returns true if the packet should be marked as discard.
#[must_use]
fn should_discard_packet(
    packet: &Packet,
    root: Slot,
    max_slot: Slot, // Max slot to ingest shreds for.
    shred_version: u16,
    packet_hasher: &PacketHasher,
    shreds_received: &mut LruCache<u64, ()>,
    should_drop_merkle_shreds: impl Fn(Slot) -> bool,
    stats: &mut ShredFetchStats,
) -> bool {
    if should_discard_shred(
        packet,
        root,
        max_slot,
        shred_version,
        should_drop_merkle_shreds,
        stats,
    ) {
        return true;
    }
    let hash = packet_hasher.hash_packet(packet);
    match shreds_received.put(hash, ()) {
        None => false,
        Some(()) => {
            stats.duplicate_shred += 1;
            true
        }
    }
}

#[must_use]
fn should_drop_merkle_shreds(shred_slot: Slot, root_bank: &Bank) -> bool {
    check_feature_activation(
        &feature_set::drop_merkle_shreds::id(),
        shred_slot,
        root_bank,
    ) && !check_feature_activation(
        &feature_set::keep_merkle_shreds::id(),
        shred_slot,
        root_bank,
    )
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_ledger::{
            blockstore::MAX_DATA_SHREDS_PER_SLOT,
            shred::{ReedSolomonCache, Shred, ShredFlags},
        },
    };

    #[test]
    fn test_data_code_same_index() {
        solana_logger::setup();
        let mut shreds_received = LruCache::new(DEFAULT_LRU_SIZE);
        let mut packet = Packet::default();
        let mut stats = ShredFetchStats::default();

        let slot = 2;
        let shred_version = 45189;
        let shred = Shred::new_from_data(
            slot,
            3,   // shred index
            1,   // parent offset
            &[], // data
            ShredFlags::LAST_SHRED_IN_SLOT,
            0, // reference_tick
            shred_version,
            3, // fec_set_index
        );
        shred.copy_to_packet(&mut packet);

        let hasher = PacketHasher::default();

        let last_root = 0;
        let last_slot = 100;
        let slots_per_epoch = 10;
        let max_slot = last_slot + 2 * slots_per_epoch;
        assert!(!should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
        let coding = solana_ledger::shred::Shredder::generate_coding_shreds(
            &[shred],
            3, // next_code_index
            &ReedSolomonCache::default(),
        );
        coding[0].copy_to_packet(&mut packet);
        assert!(!should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
    }

    #[test]
    fn test_shred_filter() {
        solana_logger::setup();
        let mut shreds_received = LruCache::new(DEFAULT_LRU_SIZE);
        let mut packet = Packet::default();
        let mut stats = ShredFetchStats::default();
        let last_root = 0;
        let last_slot = 100;
        let slots_per_epoch = 10;
        let shred_version = 59445;
        let max_slot = last_slot + 2 * slots_per_epoch;

        let hasher = PacketHasher::default();

        // packet size is 0, so cannot get index
        assert!(should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
        assert_eq!(stats.index_overrun, 1);
        let shred = Shred::new_from_data(
            2,   // slot
            3,   // index
            1,   // parent_offset
            &[], // data
            ShredFlags::LAST_SHRED_IN_SLOT,
            0, // reference_tick
            shred_version,
            0, // fec_set_index
        );
        shred.copy_to_packet(&mut packet);

        // rejected slot is 2, root is 3
        assert!(should_discard_packet(
            &packet,
            3,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
        assert_eq!(stats.slot_out_of_range, 1);

        assert!(should_discard_packet(
            &packet,
            last_root,
            max_slot,
            345, // shred_version
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
        assert_eq!(stats.shred_version_mismatch, 1);

        // Accepted for 1,3
        assert!(!should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));

        // shreds_received should filter duplicate
        assert!(should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
        assert_eq!(stats.duplicate_shred, 1);

        let shred = Shred::new_from_data(
            1_000_000,
            3,
            0,
            &[],
            ShredFlags::LAST_SHRED_IN_SLOT,
            0,
            0,
            0,
        );
        shred.copy_to_packet(&mut packet);

        // Slot 1 million is too high
        assert!(should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));

        let index = MAX_DATA_SHREDS_PER_SLOT as u32;
        let shred = Shred::new_from_data(5, index, 0, &[], ShredFlags::LAST_SHRED_IN_SLOT, 0, 0, 0);
        shred.copy_to_packet(&mut packet);
        assert!(should_discard_packet(
            &packet,
            last_root,
            max_slot,
            shred_version,
            &hasher,
            &mut shreds_received,
            |_| false, // should_drop_merkle_shreds
            &mut stats,
        ));
    }
}
