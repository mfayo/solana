use {
    crate::{serve_repair::ServeRepair, tpu::MAX_QUIC_CONNECTIONS_PER_PEER, tvu::RepairQuicConfig},
    crossbeam_channel::{unbounded, Sender},
    solana_client::connection_cache::ConnectionCache,
    solana_ledger::blockstore::Blockstore,
    solana_quic_client::{QuicConfig, QuicConnectionManager, QuicPool},
    solana_sdk::signer::Signer,
    solana_streamer::{
        quic::{spawn_server, StreamStats, MAX_STAKED_CONNECTIONS, MAX_UNSTAKED_CONNECTIONS},
        socket::SocketAddrSpace,
        streamer::{self, ResponderOption},
    },
    std::{
        sync::{atomic::AtomicBool, Arc},
        thread::{self, JoinHandle},
    },
};

pub struct ServeRepairService {
    thread_hdls: Vec<JoinHandle<()>>,
}

impl ServeRepairService {
    pub fn new(
        serve_repair: ServeRepair,
        blockstore: Arc<Blockstore>,
        repair_quic_config: RepairQuicConfig,
        socket_addr_space: SocketAddrSpace,
        stats_reporter_sender: Sender<Box<dyn FnOnce() + Send>>,
        exit: Arc<AtomicBool>,
    ) -> Self {
        trace!(
            "ServeRepairService: id: {}, listening on: {:?}",
            &serve_repair.my_id(),
            repair_quic_config
                .serve_repair_address
                .local_addr()
                .unwrap()
        );

        let (response_sender_quic, response_receiver_quic) = unbounded();

        let host = repair_quic_config
            .serve_repair_address
            .as_ref()
            .local_addr()
            .unwrap()
            .ip();
        let stats = Arc::new(StreamStats::default());

        let (request_sender_quic, request_receiver_quic) = unbounded();
        // Repair server using quic
        let (serve_repair_endpoint, repair_quic_t) = spawn_server(
            repair_quic_config.serve_repair_address.try_clone().unwrap(),
            &repair_quic_config.identity_keypair,
            host.clone(),
            request_sender_quic,
            exit.clone(),
            MAX_QUIC_CONNECTIONS_PER_PEER,
            repair_quic_config.staked_nodes.clone(),
            MAX_STAKED_CONNECTIONS,
            MAX_UNSTAKED_CONNECTIONS,
            stats.clone(),
            repair_quic_config.wait_for_chunk_timeout_ms,
        )
        .unwrap();

        let connection_cache = ConnectionCache::new_with_client_options(
            1,
            Some(serve_repair_endpoint),
            Some((&repair_quic_config.identity_keypair, host)),
            Some((
                &repair_quic_config.staked_nodes,
                &repair_quic_config.identity_keypair.pubkey(),
            )),
        );

        let connection_cache = match connection_cache {
            ConnectionCache::Quic(connection_cache) => connection_cache,
            ConnectionCache::Udp(_) => panic!("Do not expect UDP connection cache in this case"),
        };
        let t_responder_quic = streamer::responder::<QuicPool, QuicConnectionManager, QuicConfig>(
            "RepairQuic",
            ResponderOption::ConnectionCache(connection_cache),
            response_receiver_quic,
            socket_addr_space,
            Some(stats_reporter_sender),
        );

        let t_listen_quic = serve_repair.listen(
            blockstore,
            request_receiver_quic,
            response_sender_quic,
            exit,
        );

        let thread_hdls = vec![repair_quic_t, t_responder_quic, t_listen_quic];
        Self { thread_hdls }
    }

    pub fn join(self) -> thread::Result<()> {
        for thread_hdl in self.thread_hdls {
            thread_hdl.join()?;
        }
        Ok(())
    }
}
