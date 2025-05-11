mod connector;
mod tokiort;
use bytes::Bytes;
use http::{StatusCode, header};
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::net::TcpStream;

use crate::common::net::{relay, relay_with_atomic_counter};
use crate::common::new_error;
use crate::config::{COUNTER_MAP, Router};
use crate::debug_log;
use crate::proxy::{Address, BoxProxyStream, ChainStreamBuilder};
// use hyper::{Body, Client, Method, Request, Response, server::conn::Http, service::service_fn, upgrade::Upgraded};
use hyper::{Method, Request, Response, body::Incoming, service::service_fn, upgrade::Upgraded};

use tokiort::{TokioIo, TokioTimer};
type ClientBuilder = hyper::client::conn::http1::Builder;
type ServerBuilder = hyper::server::conn::http1::Builder;

use self::connector::Connector;

// To proxy tls scheme, the client must use CONNECT method. So here we are always using HTTP1.1.
// impl hyper::client::connect::Connection for BoxProxyStream {
//     fn connected(&self) -> hyper::client::connect::Connected {
//         hyper::client::connect::Connected::new()
//     }
// }

#[derive(Clone)]
pub struct HttpInbound {
    inner_map: Arc<HashMap<String, ChainStreamBuilder>>,
    router: Arc<Router>,
    enable_api_server: bool,
    in_counter_up: Option<&'static AtomicU64>,
    in_counter_down: Option<&'static AtomicU64>,
    relay_buffer_size: usize,
}
impl HttpInbound {
    pub fn new(
        inner_map: Arc<HashMap<String, ChainStreamBuilder>>,
        router: Arc<Router>,
        enable_api_server: bool,
        in_counter_up: Option<&'static AtomicU64>,
        in_counter_down: Option<&'static AtomicU64>,
        relay_buffer_size: usize,
    ) -> Self {
        Self {
            router,
            enable_api_server,
            in_counter_up,
            in_counter_down,
            relay_buffer_size,
            inner_map,
        }
    }
    pub async fn serve_http_conn(&self, io: TcpStream) -> std::io::Result<()> {
        let inner_map = self.inner_map.clone();
        let router = self.router.clone();
        let enable_api_server = self.enable_api_server;
        let in_counter_up = self.in_counter_up;
        let in_counter_down = self.in_counter_down;
        let relay_buffer_size = self.relay_buffer_size;

        let io = TokioIo::new(io);
        let res = ServerBuilder::new()
            .timer(TokioTimer::new())
            .preserve_header_case(true)
            .title_case_headers(true)
            .serve_connection(
                io,
                service_fn(|req| {
                    let inner_map = inner_map.clone();
                    let router = router.clone();
                    async move {
                        if Method::CONNECT == req.method() {
                            proxy_connect(
                                req,
                                inner_map,
                                router,
                                enable_api_server,
                                in_counter_up,
                                in_counter_down,
                                relay_buffer_size,
                            )
                            .await
                        } else {
                            let connector = Connector::new(inner_map.clone(), router.clone());
                            let io = TokioIo::new(connector);
                            let (mut sender, conn) = ClientBuilder::new()
                                .preserve_header_case(true)
                                .title_case_headers(true)
                                .handshake(io)
                                .await?;

                            proxy(req, client).await
                        }
                    }
                }),
            )
            .with_upgrades()
            .await;
        if let Err(err) = res {
            println!("Failed to serve connection: {:?}", err);
        }
        Ok(())
    }
}

async fn proxy_connect(
    req: Request<Incoming>,
    inner_map: Arc<HashMap<String, ChainStreamBuilder>>,
    router: Arc<Router>,

    enable_api_server: bool,
    in_counter_up: Option<&'static AtomicU64>,
    in_counter_down: Option<&'static AtomicU64>,
    relay_buffer_size: usize,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    if let Some(addr) = host_addr(req.uri()) {
        tokio::task::spawn(async move {
            let inner_map = inner_map;
            let router = router;
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    if let Err(e) = tunnel(
                        upgraded,
                        addr,
                        inner_map,
                        router,
                        enable_api_server,
                        in_counter_up,
                        in_counter_down,
                        relay_buffer_size,
                    )
                    .await
                    {
                        log::error!("http tunnel error: {}", e);
                    };
                }
                Err(e) => log::error!("upgrade error: {}", e),
            }
        });

        Ok(Response::new(Empty::<Bytes>::new().map_err(|never| match never {}).boxed()))
    } else {
        log::error!("CONNECT host is not socket addr: {:?}", req.uri());
        let info = "CONNECT must be to a socket address";
        let mut resp = Response::new(Full::new(info.into()).map_err(|never| match never {}).boxed());
        *resp.status_mut() = http::StatusCode::BAD_REQUEST;

        Ok(resp)
    }
}
async fn proxy(mut req: Request<Incoming>, client: Client<Connector>) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    remove_proxy_headers(&mut req);
    debug_log!("http proxy server req: {:?}", req);
    let response: Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> = client.request(req).await;
    if response.is_err() {
        Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Empty::<Bytes>::new().map_err(|never| match never {}).boxed())
            .unwrap())
    } else {
        response
    }
}

fn host_addr(uri: &http::Uri) -> Option<Address> {
    uri.authority()
        .and_then(|auth| Address::from_str(auth.as_str()).map(Some).unwrap_or(None))
}

// Create a TCP connection to host:port, build a tunnel between the connection and
// the upgraded connection
#[allow(clippy::too_many_arguments)]
async fn tunnel(
    upgraded: Upgraded,
    addr: Address,
    inner_map: Arc<HashMap<String, ChainStreamBuilder>>,
    router: Arc<Router>,
    enable_api_server: bool,
    in_counter_up: Option<&'static AtomicU64>,
    in_counter_down: Option<&'static AtomicU64>,
    relay_buffer_size: usize,
) -> std::io::Result<()> {
    // Connect to remote server
    let ob = router.match_addr(&addr);
    let stream_builder = inner_map.get(ob).unwrap();
    log::info!("routing {} to outbound:{}", addr, ob);
    if stream_builder.is_blackhole() {
        return Ok(());
    }
    let server = stream_builder.build_tcp(addr).await?;
    if enable_api_server {
        let out_down = format!("outbound>>>{}>>>traffic>>>downlink", ob);
        let out_up = format!("outbound>>>{}>>>traffic>>>uplink", ob);
        let out_down = COUNTER_MAP.get().unwrap().get(out_down.as_str()).unwrap();
        let out_up = COUNTER_MAP.get().unwrap().get(out_up.as_str()).unwrap();
        relay_with_atomic_counter(
            upgraded,
            server,
            in_counter_up.unwrap(),
            in_counter_down.unwrap(),
            out_up,
            out_down,
            relay_buffer_size,
        )
        .await?;
    } else {
        relay(upgraded, server, relay_buffer_size).await?;
    }
    Ok(())
}

pub fn remove_proxy_headers(req: &mut Request<Incoming>) {
    // Remove headers that shouldn't be forwarded to upstream
    req.headers_mut().remove(header::ACCEPT_ENCODING);
    req.headers_mut().remove(header::CONNECTION);
    req.headers_mut().remove("proxy-connection");
    req.headers_mut().remove(header::PROXY_AUTHENTICATE);
    req.headers_mut().remove(header::PROXY_AUTHORIZATION);
}
