use std::io;
use std::mem::MaybeUninit;

use crate::common::LW_BUFFER_SIZE;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};

use bytes::{BufMut, BytesMut};
use futures_util::ready;
use log::info;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::common::net::copy_with_capacity::copy_with_capacity_and_atomic_counter;
use crate::debug_log;
pub use copy_with_capacity::copy_with_capacity_and_counter;

pub mod copy_with_capacity;

pub fn poll_read_buf<T>(
    io: &mut T,
    cx: &mut Context<'_>,
    buf: &mut BytesMut,
) -> Poll<io::Result<usize>>
where
    T: AsyncRead + Unpin,
{
    if !buf.has_remaining_mut() {
        return Poll::Ready(Ok(0));
    }
    let n = {
        let dst = buf.chunk_mut();
        let dst = unsafe { &mut *(dst as *mut _ as *mut [MaybeUninit<u8>]) };
        let mut buf = ReadBuf::uninit(dst);
        let ptr = buf.filled().as_ptr();
        ready!(Pin::new(io).poll_read(cx, &mut buf)?);

        // Ensure the pointer does not change from under us
        assert_eq!(ptr, buf.filled().as_ptr());
        buf.filled().len()
    };

    // Safety: This is guaranteed to be the number of initialized (and read)
    // bytes due to the invariants provided by `ReadBuf::filled`.
    unsafe {
        buf.advance_mut(n);
    }
    Poll::Ready(Ok(n))
}

#[allow(dead_code)]
pub trait PollUtil {
    type T;
    fn drop_poll_result(self) -> Poll<io::Result<()>>;
    fn is_pending_or_error(&self) -> bool;
    fn is_error(&self) -> bool;
    fn get_poll_res(&self) -> Self::T;
}

impl<T: Default + Copy> PollUtil for Poll<io::Result<T>> {
    type T = T;
    fn drop_poll_result(self) -> Poll<io::Result<()>> {
        match self {
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_pending_or_error(&self) -> bool {
        match self {
            Poll::Ready(Err(_)) => true,
            Poll::Ready(Ok(_)) => false,
            Poll::Pending => true,
        }
    }

    fn is_error(&self) -> bool {
        match self {
            Poll::Ready(Err(_)) => true,
            Poll::Ready(Ok(_)) => false,
            Poll::Pending => false,
        }
    }

    fn get_poll_res(&self) -> Self::T {
        match self {
            Poll::Ready(Err(_)) => T::default(),
            Poll::Ready(Ok(t)) => *t,
            Poll::Pending => T::default(),
        }
    }
}
pub async fn relay<T1, T2>(
    inbound_stream: T1,
    outbound_stream: T2,
    relay_buffer_size: usize,
) -> io::Result<()>
where
    T1: AsyncRead + AsyncWrite + Unpin,
    T2: AsyncRead + AsyncWrite + Unpin,
{
    let (mut outbound_r, mut outbound_w) = tokio::io::split(outbound_stream);
    let (mut inbound_r, mut inbound_w) = tokio::io::split(inbound_stream);
    let mut down = 0u64;
    let mut up = 0u64;
    tokio::select! {
            _ = copy_with_capacity_and_counter(&mut outbound_r,&mut inbound_w,&mut down,LW_BUFFER_SIZE*relay_buffer_size)=>{
            }
            _ = copy_with_capacity_and_counter(&mut inbound_r, &mut outbound_w,&mut up,LW_BUFFER_SIZE*relay_buffer_size)=>{
            }
    }
    info!("downloaded bytes:{}, uploaded bytes:{}", down, up);
    Ok(())
}
pub async fn relay_with_atomic_counter<T1, T2>(
    inbound_stream: T1,
    outbound_stream: T2,
    inbound_up: &AtomicU64,
    inbound_down: &AtomicU64,
    outbound_up: &AtomicU64,
    outbound_down: &AtomicU64,
    relay_buffer_size: usize,
) -> io::Result<()>
where
    T1: AsyncRead + AsyncWrite + Unpin,
    T2: AsyncRead + AsyncWrite + Unpin,
{
    let (mut outbound_r, mut outbound_w) = tokio::io::split(outbound_stream);
    let (mut inbound_r, mut inbound_w) = tokio::io::split(inbound_stream);
    tokio::select! {
            _ = copy_with_capacity_and_atomic_counter(&mut outbound_r,
            &mut inbound_w,
            outbound_down,
            inbound_down,
            LW_BUFFER_SIZE*relay_buffer_size)=>{
            }
            _ = copy_with_capacity_and_atomic_counter(&mut inbound_r,
            &mut outbound_w,
            inbound_up,
            outbound_up,
            LW_BUFFER_SIZE*relay_buffer_size)=>{
            }
    }
    debug_log!(
        "api atomic counter downloaded bytes:{}, uploaded bytes:{}",
        inbound_down.load(std::sync::atomic::Ordering::Relaxed),
        inbound_up.load(std::sync::atomic::Ordering::Relaxed)
    );
    Ok(())
}
