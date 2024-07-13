use crate::common::{new_error, LW_BUFFER_SIZE};
use crate::proxy::{
    BoxProxyStream, BoxProxyUdpStream, ChainableStreamBuilder, ProtocolType, UdpRead, UdpWrite,
};
use async_trait::async_trait;
#[cfg(feature = "enable_useless")]
use bytes::Buf;
use bytes::{BufMut, Bytes, BytesMut};

use futures_util::ready;
use h2::{RecvStream, SendStream};
use http::{Request, Uri, Version};
#[cfg(feature = "enable_useless")]
use prost::encoding::{decode_varint, encode_varint};

use std::future::Future;
use std::io;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone)]
pub struct GrpcStreamBuilder {
    pub host: String,
    pub path: http::uri::PathAndQuery,
}

impl GrpcStreamBuilder {
    pub fn new(host: String, path: http::uri::PathAndQuery) -> Self {
        Self { host, path }
    }
    fn req(&self) -> io::Result<Request<()>> {
        let uri: Uri = {
            Uri::builder()
                .scheme("https")
                .authority(self.host.as_str())
                .path_and_query(self.path.as_str())
                .build()
                .map_err(new_error)?
        };
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .version(Version::HTTP_2)
            .header("content-type", "application/grpc")
            .header("user-agent", "grpc-go/1.46.0");
        Ok(request.body(()).unwrap())
    }
}

macro_rules! grpc_build_tcp_impl {
    ($s:tt,$io:tt) => {
        let (mut client, h2) = h2::client::handshake($io).await.map_err(new_error)?;
        let req = $s.req()?;
        let (resp, send_stream) = client.send_request(req, false).map_err(new_error)?;
        tokio::spawn(async move {
            if let Err(e) = h2.await {
                log::error!("http2 got err:{:?}", e);
            }
        });
        return Ok(Box::new(GrpcStream::new(resp, send_stream)))
    };
}

#[async_trait]
impl ChainableStreamBuilder for GrpcStreamBuilder {
    async fn build_tcp(&self, io: BoxProxyStream) -> io::Result<BoxProxyStream> {
        grpc_build_tcp_impl!(self, io);
    }

    async fn build_udp(
        &self,
        io: BoxProxyUdpStream,
        build_tcp_inside: bool,
    ) -> io::Result<BoxProxyUdpStream> {
        if build_tcp_inside {
            grpc_build_tcp_impl!(self, io);
        } else {
            Ok(io)
        }
    }

    fn into_box(self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self)
    }

    fn clone_box(&self) -> Box<dyn ChainableStreamBuilder> {
        Box::new(self.clone())
    }

    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Grpc
    }
}

pub struct GrpcStream {
    resp_fut: h2::client::ResponseFuture,
    recv: Option<RecvStream>,
    send: SendStream<Bytes>,
    buffer: BytesMut,
    payload_len: u64,
}

impl GrpcStream {
    pub fn new(resp_fut: h2::client::ResponseFuture, send: SendStream<Bytes>) -> Self {
        Self {
            resp_fut,
            recv: None,
            send,
            buffer: BytesMut::with_capacity(LW_BUFFER_SIZE * 4),
            payload_len: 0,
        }
    }

    fn reserve_send_capacity(&mut self, data: &[u8]) {
        let mut buf = [0u8; 10];
        #[allow(unused_mut)]
        let mut buf = &mut buf[..];
        #[cfg(feature = "enable_useless")]
        encode_varint(data.len() as u64, &mut buf);
        self.send.reserve_capacity(6 + 10 - buf.len() + data.len());
    }

    fn encode_buf(&self, data: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(16 + data.len());
        let grpc_header = [0u8; 5];
        buf.put_slice(&grpc_header[..]);
        buf.put_u8(0x0a);
        #[cfg(feature = "enable_useless")]
        encode_varint(data.len() as u64, &mut buf);
        let payload_len = ((buf.len() - 5 + data.len()) as u32).to_be_bytes();
        buf[1..5].copy_from_slice(&payload_len[..4]);
        buf.put_slice(data);
        buf.freeze()
    }
}

impl AsyncRead for GrpcStream {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        dst: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.recv.is_none() {
            self.recv = Some(
                ready!(Pin::new(&mut self.resp_fut).poll(cx))
                    .map_err(new_error)?
                    .into_body(),
            );
            log::debug!("receive grpc recv stream");
        }
        if !self.buffer.is_empty() {
            let to_read = std::cmp::min(dst.remaining(), self.buffer.len());
            let data = self.buffer.split_to(to_read);
            self.payload_len -= to_read as u64;
            dst.put_slice(&data[..to_read]);
            return Poll::Ready(Ok(()));
        };
        Poll::Ready(
            match ready!(Pin::new(&mut self.recv).as_pin_mut().unwrap().poll_data(cx)) {
                #[allow(unused_mut)]
                Some(Ok(mut data)) => {
                    let before_parse_data_len = data.len();
                    #[cfg(feature = "enable_useless")]
                    while self.payload_len > 0 || data.len() > 6 {
                        if self.payload_len == 0 {
                            data.advance(6);
                            self.payload_len = decode_varint(&mut data).map_err(new_error)?;
                        }
                        let to_read = std::cmp::min(dst.remaining(), data.len());
                        let to_read = std::cmp::min(self.payload_len as usize, to_read);
                        if to_read == 0 {
                            self.buffer.extend_from_slice(&data[..]);
                            data.clear();
                            break;
                        }
                        dst.put_slice(&data[..to_read]);
                        self.payload_len -= to_read as u64;
                        data.advance(to_read);
                    }
                    // increase recv window
                    self.recv
                        .as_mut()
                        .unwrap()
                        .flow_control()
                        .release_capacity(before_parse_data_len - data.len())
                        .map_or_else(
                            |e| Err(Error::new(ErrorKind::ConnectionReset, e)),
                            |_| Ok(()),
                        )
                }
                // no more data frames
                // maybe trailer
                // or cancelled
                _ => Ok(()),
            },
        )
    }
}

impl AsyncWrite for GrpcStream {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.reserve_send_capacity(buf);
        Poll::Ready(match ready!(self.send.poll_capacity(cx)) {
            Some(Ok(to_write)) => {
                let encoded_buf = self.encode_buf(buf);
                self.send.send_data(encoded_buf, false).map_or_else(
                    |e| Err(Error::new(ErrorKind::BrokenPipe, e)),
                    |_| Ok(to_write),
                )
            }
            // is_send_streaming returns false
            // which indicates the state is
            // neither open nor half_close_remote
            _ => Err(Error::new(ErrorKind::BrokenPipe, "broken pipe")),
        })
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    #[inline]
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.send.reserve_capacity(0);
        Poll::Ready(ready!(self.send.poll_capacity(cx)).map_or(
            Err(Error::new(ErrorKind::BrokenPipe, "broken pipe")),
            |_| {
                self.send
                    .send_data(Bytes::new(), true)
                    .map_or_else(|e| Err(Error::new(ErrorKind::BrokenPipe, e)), |_| Ok(()))
            },
        ))
    }
}

impl UdpRead for GrpcStream {}
impl UdpWrite for GrpcStream {}
