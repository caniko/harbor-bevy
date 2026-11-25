//! Mirror of the server's `FrameCodec`. See server crate for protocol notes.
//! Duplicated rather than shared to keep the dependency graph clean —
//! this file is ~80 lines and pure.

use futures::prelude::*;
use libp2p::{StreamProtocol, request_response};
use rkyv::rancor;
use std::io;
use thespan_rendezvous_proto::{Frame, MAX_FRAME_BYTES};

#[derive(Debug, Clone)]
pub struct FrameCodec;

async fn read_frame<R: AsyncRead + Unpin + Send>(io: &mut R) -> io::Result<Frame> {
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame > MAX_FRAME_BYTES",
        ));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    rkyv::from_bytes::<Frame, rancor::Error>(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv decode: {e}")))
}

async fn write_frame<W: AsyncWrite + Unpin + Send>(io: &mut W, frame: &Frame) -> io::Result<()> {
    let bytes = rkyv::to_bytes::<rancor::Error>(frame)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv encode: {e}")))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame > MAX_FRAME_BYTES",
        ));
    }
    io.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}

#[async_trait::async_trait]
impl request_response::Codec for FrameCodec {
    type Protocol = StreamProtocol;
    type Request = Frame;
    type Response = Frame;

    async fn read_request<T: AsyncRead + Unpin + Send>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Frame> {
        read_frame(io).await
    }
    async fn read_response<T: AsyncRead + Unpin + Send>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Frame> {
        read_frame(io).await
    }
    async fn write_request<T: AsyncWrite + Unpin + Send>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Frame,
    ) -> io::Result<()> {
        write_frame(io, &req).await
    }
    async fn write_response<T: AsyncWrite + Unpin + Send>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        resp: Frame,
    ) -> io::Result<()> {
        write_frame(io, &resp).await
    }
}
