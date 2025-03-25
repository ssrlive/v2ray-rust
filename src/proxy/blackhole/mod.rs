use crate::proxy::{BoxProxyStream, BoxProxyUdpStream};
use crate::proxy::{ChainableStreamBuilder, ProtocolType};
use async_trait::async_trait;

#[derive(Clone)]
pub struct BlackHoleStreamBuilder;

#[async_trait]
impl ChainableStreamBuilder for BlackHoleStreamBuilder {
    async fn build_tcp(&self, _io: BoxProxyStream) -> std::io::Result<BoxProxyStream> {
        unimplemented!()
    }

    async fn build_udp(&self, _io: BoxProxyUdpStream, _build_tcp_inside: bool) -> std::io::Result<BoxProxyUdpStream> {
        unimplemented!()
    }

    fn into_box(self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self)
    }

    fn clone_box(&self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self.clone())
    }

    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Blackhole
    }
}
