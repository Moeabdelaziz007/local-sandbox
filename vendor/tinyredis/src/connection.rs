use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;
use crate::parser::Frame;

pub struct Connection {
    stream: TcpStream,
    read_buf: BytesMut,
    write_buf: BytesMut,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            read_buf: BytesMut::with_capacity(4086),
            write_buf: BytesMut::with_capacity(4096),
        }
    }

    /// Read a from the socket until a complete frame is buffered.
    /// Returns `Ok(None)` on clean EOF (client closed the connection).
    pub async fn read_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if let Some((frame, n)) = Frame::parse(&self.read_buf)? {
                self.read_buf.advance(n);
                return Ok(Some(frame));
            }
            let n = self.stream.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(None); // clean EOF
            }
        }
    }

    /// Serialize `frame` into the write buffer and flush. Do NOT flush to the socket.
    /// Call `flush` to send the data to the socket.
    pub fn write_frame(&mut self, frame: &Frame) {
        self.write_buf.put(frame.serialize());
    }

    /// Write the write buffer to the socket in one syscall.
    pub async fn flush(&mut self) -> Result<()> {
        self.stream.write_all(&self.write_buf).await?;
        self.write_buf.clear();
        Ok(())
    }
}
