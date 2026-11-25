//! `request_response::Codec` over our rkyv-archived [`Frame`].
//!
//! Wire format on the libp2p stream is **length-prefixed rkyv bytes**:
//! `u32_be length || rkyv(Frame)`. Length is bounded by [`MAX_FRAME_BYTES`].

use futures::prelude::*;
use libp2p::request_response;
use rkyv::rancor;
use std::io;
use thespan_rendezvous_proto::{Frame, MAX_FRAME_BYTES};

#[derive(Debug, Clone)]
pub struct FrameCodec;

async fn read_frame<R>(io: &mut R) -> io::Result<Frame>
where
    R: AsyncRead + Unpin + Send,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME_BYTES",
        ));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    rkyv::from_bytes::<Frame, rancor::Error>(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv decode: {e}")))
}

async fn write_frame<W>(io: &mut W, frame: &Frame) -> io::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    let bytes = rkyv::to_bytes::<rancor::Error>(frame)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv encode: {e}")))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME_BYTES",
        ));
    }
    let len = (bytes.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}

#[async_trait::async_trait]
impl request_response::Codec for FrameCodec {
    type Protocol = libp2p::StreamProtocol;
    type Request = Frame;
    type Response = Frame;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(io).await
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(io).await
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(io, &resp).await
    }
}
