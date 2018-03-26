use accountant::Accountant;
use transaction::Transaction;
use signature::PublicKey;
use hash::Hash;
use entry::Entry;
use std::net::UdpSocket;
use bincode::{deserialize, serialize};
use result::Result;
use streamer;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::sync::mpsc::channel;
use std::thread::{spawn, JoinHandle};
use std::default::Default;
use std::io::Write;
use serde_json;

pub struct AccountantSkel<W: Write + Send + 'static> {
    pub acc: Accountant,
    pub last_id: Hash,
    pub ledger: Vec<Entry>,
    writer: W,
}

#[cfg_attr(feature = "cargo-clippy", allow(large_enum_variant))]
#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Transaction(Transaction),
    GetBalance { key: PublicKey },
    GetEntries { last_id: Hash },
    GetId { is_last: bool },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    Balance { key: PublicKey, val: Option<i64> },
    Entries { entries: Vec<Entry> },
    Id { id: Hash, is_last: bool },
}

impl<W: Write + Send + 'static> AccountantSkel<W> {
    pub fn new(acc: Accountant, w: W) -> Self {
        let last_id = acc.first_id;
        AccountantSkel {
            acc,
            last_id,
            ledger: vec![],
            writer: w,
        }
    }

    pub fn sync(&mut self) -> Hash {
        while let Ok(entry) = self.acc.historian.receiver.try_recv() {
            self.last_id = entry.id;
            write!(self.writer, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
            self.ledger.push(entry);
        }
        self.last_id
    }

    pub fn process_request(self: &mut Self, msg: Request) -> Option<Response> {
        match msg {
            Request::Transaction(tr) => {
                if let Err(err) = self.acc.process_transaction(tr) {
                    eprintln!("Transaction error: {:?}", err);
                }
                None
            }
            Request::GetBalance { key } => {
                let val = self.acc.get_balance(&key);
                Some(Response::Balance { key, val })
            }
            Request::GetEntries { last_id } => {
                self.sync();
                let entries = self.ledger
                    .iter()
                    .skip_while(|x| x.id != last_id) // log(n) way to find Entry with id == last_id.
                    .skip(1) // Skip the entry with last_id.
                    .take(256) // TODO: Take while the serialized entries fit into a 64k UDP packet.
                    .cloned()
                    .collect();
                Some(Response::Entries { entries })
            }
            Request::GetId { is_last } => Some(Response::Id {
                id: if is_last {
                    self.sync()
                } else {
                    self.acc.first_id
                },
                is_last,
            }),
        }
    }
    fn process(
        &mut self,
        r_reader: &streamer::Receiver,
        s_responder: &streamer::Responder,
        packet_recycler: &streamer::PacketRecycler,
        response_recycler: &streamer::ResponseRecycler,
    ) -> Result<()> {
        let timer = Duration::new(1, 0);
        let msgs = r_reader.recv_timeout(timer)?;
        let msgs_ = msgs.clone();
        let rsps = streamer::allocate(response_recycler);
        let rsps_ = rsps.clone();
        {
            let mut num = 0;
            let mut ursps = rsps.write().unwrap();
            for packet in &msgs.read().unwrap().packets {
                let sz = packet.meta.size;
                let req = deserialize(&packet.data[0..sz])?;
                if let Some(resp) = self.process_request(req) {
                    if ursps.responses.len() <= num {
                        ursps
                            .responses
                            .resize((num + 1) * 2, streamer::Response::default());
                    }
                    let rsp = &mut ursps.responses[num];
                    let v = serialize(&resp)?;
                    let len = v.len();
                    rsp.data[..len].copy_from_slice(&v);
                    rsp.meta.size = len;
                    rsp.meta.set_addr(&packet.meta.get_addr());
                    num += 1;
                }
            }
            ursps.responses.resize(num, streamer::Response::default());
        }
        s_responder.send(rsps_)?;
        streamer::recycle(packet_recycler, msgs_);
        Ok(())
    }

    /// UDP Server that forwards messages to Accountant methods.
    pub fn serve(
        obj: Arc<Mutex<AccountantSkel<W>>>,
        addr: &str,
        exit: Arc<Mutex<bool>>,
    ) -> Result<Vec<JoinHandle<()>>> {
        let read = UdpSocket::bind(addr)?;
        // make sure we are on the same interface
        let mut local = read.local_addr()?;
        local.set_port(0);
        let write = UdpSocket::bind(local)?;

        let packet_recycler = Arc::new(Mutex::new(Vec::new()));
        let response_recycler = Arc::new(Mutex::new(Vec::new()));
        let (s_reader, r_reader) = channel();
        let t_receiver = streamer::receiver(read, exit.clone(), packet_recycler.clone(), s_reader)?;

        let (s_responder, r_responder) = channel();
        let t_responder =
            streamer::responder(write, exit.clone(), response_recycler.clone(), r_responder);

        let t_server = spawn(move || {
            if let Ok(me) = Arc::try_unwrap(obj) {
                loop {
                    let _e = me.lock().unwrap().process(
                        &r_reader,
                        &s_responder,
                        &packet_recycler,
                        &response_recycler,
                    );
                    debug!("exit {:?}", *exit.lock().unwrap());
                    if *exit.lock().unwrap() {
                        info!("serve exiting");
                        break;
                    }
                }
            }
        });
        Ok(vec![t_receiver, t_responder, t_server])
    }
}
