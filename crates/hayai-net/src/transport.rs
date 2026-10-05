//! Framed message transport over blocking sockets: one reader thread per peer that decodes
//! frames and hands them to a handler, one writer thread draining a bounded queue.
//!
//! The outbound queue has a bound in frames and a bound in bytes
//! ([`MAX_QUEUED_BYTES`]). A write that makes no progress for [`WRITE_TIMEOUT`] closes
//! the connection, so a peer that does not read cannot hold the queue.

use std::io::{self, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Sender, TrySendError};

use crate::codec::{
    encode, read_message, LegacyMessage, Network, ReadError, MAX_HANDSHAKE_BODY_LEN,
};

/// Bytes of the frames that wait in the outbound queue of one connection, at most. A
/// frame that goes above the bound is refused, except when the queue is empty: the
/// largest frame (a compact-relay payload of 8 MB) is then still sent.
pub const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
/// Time without progress of a write after which the connection closes.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Sends framed messages to one peer.
pub trait Transport: Send + Sync {
    /// Queues a message. Fails when the peer's outbound queue is full (a peer that cannot
    /// keep up) or the connection is closed; the caller drops the peer in both cases.
    fn send(&self, message: &LegacyMessage) -> Result<(), TransportError>;
    /// Bytes of the frames that wait in the outbound queue.
    fn queued_bytes(&self) -> usize;
    fn close(&self);
    fn peer_addr(&self) -> SocketAddr;
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum TransportError {
    #[error("outbound queue full")]
    QueueFull,
    #[error("connection closed")]
    Closed,
}

/// What the reader thread hands to the handler.
#[derive(Debug)]
pub enum Incoming {
    Message(LegacyMessage),
    /// The connection ended (EOF or I/O error).
    Closed(String),
    /// A frame failed to decode. The connection is closed.
    Malformed(String),
}

/// The handler's verdict after each message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    /// Keep reading; the next frame's payload may be at most `max_body` bytes.
    Continue { max_body: usize },
    /// Stop reading and shut the socket down.
    Close,
}

pub type Handler = Box<dyn FnMut(Incoming) -> Control + Send>;

pub struct TcpTransport {
    stream: TcpStream,
    peer_addr: SocketAddr,
    network: Network,
    out: Sender<Vec<u8>>,
    /// Bytes of the frames in `out`.
    queued: Arc<AtomicUsize>,
    closed: AtomicBool,
}

impl TcpTransport {
    /// Wraps a connected stream and starts its writer thread. Frames are encoded with
    /// `network`'s magic; at most `queue_len` messages wait in the outbound queue.
    pub fn new(stream: TcpStream, network: Network, queue_len: usize) -> io::Result<Arc<Self>> {
        let peer_addr = stream.peer_addr()?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let mut writer_stream = stream.try_clone()?;
        let (out, out_rx) = bounded::<Vec<u8>>(queue_len);
        let queued = Arc::new(AtomicUsize::new(0));
        let written = queued.clone();
        let transport = Arc::new(Self {
            stream,
            peer_addr,
            network,
            out,
            queued,
            closed: AtomicBool::new(false),
        });
        // The writer holds no reference to the transport: its queue closes when the
        // transport is dropped, which is what ends the thread.
        thread::Builder::new()
            .name(format!("net-writer-{peer_addr}"))
            .spawn(move || {
                for frame in out_rx {
                    let result = writer_stream.write_all(&frame);
                    written.fetch_sub(frame.len(), Ordering::AcqRel);
                    if let Err(e) = result {
                        tracing::debug!(peer = %peer_addr, error = %e, "write failed");
                        // Unblocks the reader, which reports the closed connection.
                        let _ = writer_stream.shutdown(Shutdown::Both);
                        break;
                    }
                }
            })?;
        Ok(transport)
    }

    /// Starts the reader thread. The first frame's payload is bounded by
    /// [`MAX_HANDSHAKE_BODY_LEN`], later ones by what the handler returns.
    pub fn run_reader(self: &Arc<Self>, mut handler: Handler) -> io::Result<()> {
        let reader_stream = self.stream.try_clone()?;
        let network = self.network;
        let peer_addr = self.peer_addr;
        let transport = Arc::clone(self);
        thread::Builder::new()
            .name(format!("net-reader-{peer_addr}"))
            .spawn(move || {
                let mut reader = BufReader::with_capacity(64 * 1024, reader_stream);
                let mut max_body = MAX_HANDSHAKE_BODY_LEN;
                loop {
                    let message = match read_message(&mut reader, network, max_body) {
                        Ok(m) => m,
                        Err(e) => {
                            transport.close();
                            handler(match e {
                                ReadError::Io(e) => Incoming::Closed(e.to_string()),
                                ReadError::Decode(e) => Incoming::Malformed(e.to_string()),
                            });
                            break;
                        }
                    };
                    match handler(Incoming::Message(message)) {
                        Control::Continue { max_body: next } => max_body = next,
                        Control::Close => {
                            transport.close();
                            break;
                        }
                    }
                }
            })?;
        Ok(())
    }
}

impl Transport for TcpTransport {
    fn send(&self, message: &LegacyMessage) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        let frame = encode(self.network, message);
        let len = frame.len();
        let before = self.queued.fetch_add(len, Ordering::AcqRel);
        if before > 0 && before + len > MAX_QUEUED_BYTES {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return Err(TransportError::QueueFull);
        }
        match self.out.try_send(frame) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.queued.fetch_sub(len, Ordering::AcqRel);
                match error {
                    TrySendError::Full(_) => Err(TransportError::QueueFull),
                    TrySendError::Disconnected(_) => Err(TransportError::Closed),
                }
            }
        }
    }

    fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            // Unblocks the reader thread; errors (already closed by the peer) are moot.
            let _ = self.stream.shutdown(Shutdown::Both);
        }
    }

    fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bytes::Bytes;

    use super::*;

    /// A peer that does not read: the queue refuses a frame above the byte bound long
    /// before the frame bound, and the bytes in the queue stay at or below the bound.
    #[test]
    fn the_outbound_queue_has_a_byte_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let stream = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (_silent, _) = listener.accept().expect("accept");
        let transport = TcpTransport::new(stream, Network::Regtest, 1024).expect("transport");
        let block = LegacyMessage::Block(Bytes::from(vec![0u8; 2_000_000]));
        let mut sent = 0;
        let error = loop {
            match transport.send(&block) {
                Ok(()) => sent += 1,
                Err(error) => break error,
            }
            assert!(sent < 1024, "the frame bound is not the first bound");
        };
        assert_eq!(error, TransportError::QueueFull);
        assert!(transport.queued_bytes() <= MAX_QUEUED_BYTES);
        // The socket buffers of the kernel take some frames more than the queue.
        assert!((16..64).contains(&sent), "{sent} frames");
        transport.close();
    }
}
