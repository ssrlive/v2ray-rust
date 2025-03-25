use crate::proxy::{BoxProxyStream, BoxProxyUdpStream};
use crate::proxy::{ChainableStreamBuilder, ProtocolType};
use async_trait::async_trait;

#[derive(Clone)]
pub struct DirectStreamBuilder;

#[async_trait]
impl ChainableStreamBuilder for DirectStreamBuilder {
    async fn build_tcp(&self, io: BoxProxyStream) -> std::io::Result<BoxProxyStream> {
        Ok(io)
    }

    async fn build_udp(&self, io: BoxProxyUdpStream, _build_tcp_inside: bool) -> std::io::Result<BoxProxyUdpStream> {
        Ok(io)
    }

    fn into_box(self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self)
    }

    fn clone_box(&self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self.clone())
    }

    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Direct
    }
}
