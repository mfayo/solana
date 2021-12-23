use {
    crate::clock::Slot,
    bincode::Result,
    serde::Serialize,
    std::{
        fmt, io,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    },
};

/// Maximum over-the-wire size of a Transaction
///   1280 is IPv6 minimum MTU
///   40 bytes is the size of the IPv6 header
///   8 bytes is the size of the fragment header
pub const PACKET_DATA_SIZE: usize = 1280 - 40 - 8;

pub const EXTENDED_PACKET_DATA_SIZE: usize = PACKET_DATA_SIZE * 2;

pub const MAX_TRANSACTION_SIZE: usize = EXTENDED_PACKET_DATA_SIZE;
#[cfg(test)]
static_assertions::const_assert_eq!(EXTENDED_PACKET_DATA_SIZE > PACKET_DATA_SIZE);

// Send + Sync needed to allow Rayon par_iter()
/// Generic interface to support variable sized packtes
pub trait PacketInterface: Clone + Default + Sized + Send + Sync + fmt::Debug {
    fn get_data(&self) -> &[u8];

    fn get_data_mut(&mut self) -> &mut [u8];

    fn get_meta(&self) -> &Meta;

    fn get_meta_mut(&mut self) -> &mut Meta;

    fn from_data<T: Serialize>(dest: Option<&SocketAddr>, data: T) -> Result<Self> {
        let mut packet = Self::default();
        Self::populate_packet(&mut packet, dest, &data)?;
        Ok(packet)
    }

    fn populate_packet<T: Serialize>(
        packet: &mut Self,
        dest: Option<&SocketAddr>,
        data: &T,
    ) -> Result<()> {
        let mut wr = io::Cursor::new(&mut *packet.get_data_mut());
        bincode::serialize_into(&mut wr, data)?;
        let len = wr.position() as usize;
        packet.get_meta_mut().size = len;
        if let Some(dest) = dest {
            packet.get_meta_mut().set_addr(dest);
        }
        Ok(())
    }

    // Hack to allow the introduction of special logic
    // in necessary places and work around Rust's lack of generic specialization
    // or similar "compile-time conditionals"
    // TODO: is there a better way to do this (perhaps a macro of some sort?)?
    fn is_extended() -> bool;
}

#[derive(Clone, Default, Debug, PartialEq)]
#[repr(C)]
pub struct Meta {
    pub size: usize,
    pub forwarded: bool,
    pub repair: bool,
    pub discard: bool,
    pub addr: [u16; 8],
    pub port: u16,
    pub v6: bool,
    pub seed: [u8; 32],
    pub slot: Slot,
    pub is_tracer_tx: bool,
    pub is_simple_vote_tx: bool,
}

#[derive(Clone)]
#[repr(C)]
pub struct Packet {
    pub data: [u8; PACKET_DATA_SIZE],
    pub meta: Meta,
}

// TODO: can we de-duplicate some of this Packet and ExtendedPacket code?
#[derive(Clone)]
#[repr(C)]
pub struct ExtendedPacket {
    pub data: [u8; EXTENDED_PACKET_DATA_SIZE],
    pub meta: Meta,
}

impl PacketInterface for ExtendedPacket {
    fn get_data(&self) -> &[u8] {
        &self.data
    }

    fn get_data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn get_meta(&self) -> &Meta {
        &self.meta
    }

    fn get_meta_mut(&mut self) -> &mut Meta {
        &mut self.meta
    }

    fn is_extended() -> bool {
        true
    }
}

impl PacketInterface for Packet {
    fn get_data(&self) -> &[u8] {
        &self.data
    }

    fn get_data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn get_meta(&self) -> &Meta {
        &self.meta
    }

    fn get_meta_mut(&mut self) -> &mut Meta {
        &mut self.meta
    }

    fn is_extended() -> bool {
        false
    }
}

impl fmt::Debug for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Packet {{ size: {:?}, addr: {:?} }}",
            self.meta.size,
            self.meta.addr()
        )
    }
}

#[allow(clippy::uninit_assumed_init)]
impl Default for Packet {
    fn default() -> Packet {
        Packet {
            data: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
            meta: Meta::default(),
        }
    }
}

impl PartialEq for Packet {
    fn eq(&self, other: &Packet) -> bool {
        let self_data: &[u8] = self.data.as_ref();
        let other_data: &[u8] = other.data.as_ref();
        self.meta == other.meta && self_data[..self.meta.size] == other_data[..self.meta.size]
    }
}

impl fmt::Debug for ExtendedPacket {
    // It may be useful to know the type of Packet in the debug
    // print
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ExtendedPacket {{ size: {:?}, addr: {:?} }}",
            self.meta.size,
            self.meta.addr()
        )
    }
}

#[allow(clippy::uninit_assumed_init)]
impl Default for ExtendedPacket {
    fn default() -> ExtendedPacket {
        ExtendedPacket {
            data: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
            meta: Meta::default(),
        }
    }
}

impl PartialEq for ExtendedPacket {
    fn eq(&self, other: &ExtendedPacket) -> bool {
        let self_data: &[u8] = self.data.as_ref();
        let other_data: &[u8] = other.data.as_ref();
        self.meta == other.meta && self_data[..self.meta.size] == other_data[..self.meta.size]
    }
}

impl Meta {
    pub fn addr(&self) -> SocketAddr {
        if !self.v6 {
            let addr = [
                self.addr[0] as u8,
                self.addr[1] as u8,
                self.addr[2] as u8,
                self.addr[3] as u8,
            ];
            let ipv4: Ipv4Addr = From::<[u8; 4]>::from(addr);
            SocketAddr::new(IpAddr::V4(ipv4), self.port)
        } else {
            let ipv6: Ipv6Addr = From::<[u16; 8]>::from(self.addr);
            SocketAddr::new(IpAddr::V6(ipv6), self.port)
        }
    }

    pub fn set_addr(&mut self, a: &SocketAddr) {
        match *a {
            SocketAddr::V4(v4) => {
                let ip = v4.ip().octets();
                self.addr[0] = u16::from(ip[0]);
                self.addr[1] = u16::from(ip[1]);
                self.addr[2] = u16::from(ip[2]);
                self.addr[3] = u16::from(ip[3]);
                self.addr[4] = 0;
                self.addr[5] = 0;
                self.addr[6] = 0;
                self.addr[7] = 0;
                self.v6 = false;
            }
            SocketAddr::V6(v6) => {
                self.addr = v6.ip().segments();
                self.v6 = true;
            }
        }
        self.port = a.port();
    }
}
