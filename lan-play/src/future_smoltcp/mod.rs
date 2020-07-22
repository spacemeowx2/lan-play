mod peekable_receiver;
mod raw_udp;
mod reactor;
mod socket;
mod socketset;

use crate::interface::{RawsockInterface, IntercepterBuilder};
use peekable_receiver::PeekableReceiver;
pub use raw_udp::OwnedUdp;
use reactor::NetReactor;
use smoltcp::{
    iface::{
        EthernetInterface as SmoltcpEthernetInterface, EthernetInterfaceBuilder, NeighborCache,
        Routes,
    },
    phy::{DeviceCapabilities, RxToken, TxToken},
    time::Instant,
    wire::{EthernetAddress, IpCidr, Ipv4Address},
};
pub use socket::{SocketHandle, TcpListener, TcpSocket, UdpSocket};
use socketset::SocketSet;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use async_channel::{Sender, Receiver, TryRecvError};

type Packet = Vec<u8>;
pub type Ethernet = SmoltcpEthernetInterface<'static, 'static, 'static, FutureDevice>;

pub struct Net {
    reactor: NetReactor,
}

impl Net {
    pub fn new(
        ethernet_addr: EthernetAddress,
        ip_addrs: Vec<IpCidr>,
        gateway_ip: Ipv4Address,
        interf: RawsockInterface,
        intercepter: IntercepterBuilder,
    ) -> Net {
        let (_running, tx, rx) = interf.start(
            intercepter
        );
        let device = FutureDevice::new(tx, rx);
        let neighbor_cache = NeighborCache::new(BTreeMap::new());
        let mut routes = Routes::new(BTreeMap::new());
        routes.add_default_ipv4_route(gateway_ip).unwrap();

        let ethernet = EthernetInterfaceBuilder::new(device)
            .ethernet_addr(ethernet_addr)
            .ip_addrs(ip_addrs)
            .neighbor_cache(neighbor_cache)
            .any_ip(true)
            .routes(routes)
            .finalize();

        let socket_set = Arc::new(Mutex::new(SocketSet::new()));
        let reactor = NetReactor::new(socket_set);
        let r = reactor.clone();
        crate::rt::spawn(async move {
            r.run(ethernet).await
        });

        Net {
            reactor,
        }
    }
    pub async fn tcp_listener(&self) -> TcpListener {
        TcpListener::new(self.reactor.clone()).await
    }
    pub async fn udp_socket(&self) -> UdpSocket {
        UdpSocket::new(self.reactor.clone()).await
    }
}

pub struct FutureDevice {
    caps: DeviceCapabilities,
    receiver: PeekableReceiver<Packet>,
    sender: Sender<Packet>,
}

impl FutureDevice {
    fn new(tx: Sender<Packet>, rx: Receiver<Packet>) -> FutureDevice {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1536;
        caps.max_burst_size = Some(1);
        FutureDevice {
            caps,
            receiver: PeekableReceiver::new(rx),
            sender: tx,
        }
    }
}

pub struct FutureRxToken(Packet);

impl RxToken for FutureRxToken {
    fn consume<R, F>(mut self, _timestamp: Instant, f: F) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        let p = &mut self.0;
        let result = f(p);
        result
    }
}

pub struct FutureTxToken(Sender<Packet>);

impl TxToken for FutureTxToken {
    fn consume<R, F>(self, _timestamp: Instant, len: usize, f: F) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        if result.is_ok() {
            let s = self.0;
            if let Err(_e) = s.try_send(buffer) {
                log::warn!("send error");
            }
        }
        result
    }
}

impl<'d> smoltcp::phy::Device<'d> for FutureDevice {
    type RxToken = FutureRxToken;
    type TxToken = FutureTxToken;

    fn receive(&'d mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        match self.receiver.try_recv() {
            Ok(packet) => Some((FutureRxToken(packet), FutureTxToken(self.sender.clone()))),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Closed) => todo!("handle receiver closed"),
        }
    }
    fn transmit(&'d mut self) -> Option<Self::TxToken> {
        Some(FutureTxToken(self.sender.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.caps.clone()
    }
}
