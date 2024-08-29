use bytes::Bytes;
use quinn_proto::{
    Chunk, ClosedStream, FinishError, ReadError, ReadableError, StreamId, VarInt, WriteError,
    Written,
};
use thiserror::Error;

/// A stream that can be used to send data.
pub struct SendStream<'a> {
    pub(crate) id: StreamId,
    pub(crate) write_buffer: &'a mut Vec<Bytes>,
    pub(crate) proto_stream: quinn_proto::SendStream<'a>,
}

impl<'a> SendStream<'a> {
    /// Get the ID of the stream.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Write data to the stream.
    ///
    /// Returns the number of bytes written on success. Congestion and flow control may cause this to
    /// be shorter than `data.len()`, indicating that only a prefix of `data` was written.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.proto_stream.write(data)
    }

    /// Convenience method to write an entire buffer to the stream.
    ///
    /// Data that could not be written immediately will be internally buffered instead,
    /// and automatically flushed when the stream becomes writeable again.
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), WriteError> {
        match self.proto_stream.write(data) {
            Ok(written) => {
                let remaining = data.len() - written;
                if remaining > 0 {
                    self.write_buffer
                        .push(Bytes::copy_from_slice(&data[remaining..]));
                }
                Ok(())
            }
            Err(WriteError::Blocked) => {
                self.write_buffer.push(Bytes::copy_from_slice(data));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Write chunks of data to the stream. Slighty more efficient than `write` due to not copying.
    ///
    /// Returns the number of bytes and chunks written on success. Congestion and flow control may cause this to
    /// be shorter than `data.len()`, indicating that only a prefix of `data` was written.
    /// Note that this method might also write a partial chunk. In this case
    /// [`Written::chunks`] will not count this chunk as fully written,
    /// and the chunk will be advanced and contain only non-written data after the call.
    pub fn write_chunks(&mut self, data: &mut [Bytes]) -> Result<Written, WriteError> {
        self.proto_stream.write_chunks(data)
    }

    /// Convenience method to write a single chunk in its entirety to the stream.
    ///
    /// Data that could not be written immediately will be internally buffered instead,
    /// and automatically flushed when the stream becomes writeable again.
    pub fn write_chunk(&mut self, data: Bytes) -> Result<(), WriteError> {
        self.write_all_chunks(&mut [data])
    }

    /// Convenience method to write an entire list of chunks to the stream.
    ///
    /// Data that could not be written immediately will be internally buffered instead,
    /// and automatically flushed when the stream becomes writeable again.
    pub fn write_all_chunks(&mut self, data: &mut [Bytes]) -> Result<(), WriteError> {
        match self.proto_stream.write_chunks(data) {
            Ok(written) => {
                self.write_buffer.extend_from_slice(&data[written.chunks..]);
                Ok(())
            }
            Err(WriteError::Blocked) => {
                self.write_buffer.extend_from_slice(data);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Check if the stream was stopped by the peer, get the reason if it was.
    ///
    /// Returns `Ok(Some(_))` with the stop error code when the stream is stopped by the peer.
    /// Returns `Ok(None)` if the stream is still alive and has not been stopped.
    /// Returns `Err` when the stream is [`finish()`](Self::finish)ed or [`reset()`](Self::reset) locally and all stream data has been
    /// received (but not necessarily processed) by the peer, after which it is no longer meaningful
    /// for the stream to be stopped.
    pub fn stopped(&self) -> Result<Option<VarInt>, ClosedStream> {
        self.proto_stream.stopped()
    }

    /// Notify the peer that no more data will ever be written to this stream.
    ///
    /// It is an error to write to a [`SendStream`] after `finish()`ing it. [`reset()`](Self::reset)
    /// may still be called after `finish` to abandon transmission of any stream data that might
    /// still be buffered.
    ///
    /// To wait for the peer to receive all buffered stream data, see [`stopped()`](Self::stopped).
    ///
    /// May fail if [`finish()`](Self::finish) or [`reset()`](Self::reset) was previously
    /// called. This error is harmless and serves only to indicate that the caller may have
    /// incorrect assumptions about the stream's state.
    pub fn finish(&mut self) -> Result<(), FinishError> {
        self.proto_stream.finish()
    }

    /// Close the stream immediately.
    ///
    /// No new data can be written after calling this method. Locally buffered data is dropped, and
    /// previously transmitted data will no longer be retransmitted if lost. If an attempt has
    /// already been made to finish the stream, the peer may still receive all written data.
    ///
    /// May fail if [`finish()`](Self::finish) or [`reset()`](Self::reset) was previously
    /// called. This error is harmless and serves only to indicate that the caller may have
    /// incorrect assumptions about the stream's state.
    pub fn reset(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        self.proto_stream.reset(error_code)
    }

    /// Set the priority of the stream.
    ///
    /// Every send stream has an initial priority of 0. Locally buffered data from streams with
    /// higher priority will be transmitted before data from streams with lower priority. Changing
    /// the priority of a stream with pending data may only take effect after that data has been
    /// transmitted. Using many different priority levels per connection may have a negative
    /// impact on performance.
    pub fn set_priority(&mut self, priority: i32) -> Result<(), ClosedStream> {
        self.proto_stream.set_priority(priority)
    }

    /// Get the priority of the stream.
    pub fn priority(&self) -> Result<i32, ClosedStream> {
        self.proto_stream.priority()
    }
}

/// A stream that can be used to receive data.
///
/// # Common issues
///
/// ## Data never received on a locally-opened stream
///
/// Peers are not notified of streams until they or a later-numbered stream are used to send
/// data. If a bidirectional stream is locally opened but never used to send, then the peer may
/// never see it. Application protocols should always arrange for the endpoint which will first
/// transmit on a stream to be the endpoint responsible for opening it.
///
/// ## Data never received on a remotely-opened stream
///
/// Verify that the stream you are receiving is the same one that the server is sending on, e.g. by
/// logging the [`id`](Self::id) of each. Streams are always accepted in the same order as they are created,
/// i.e. ascending order by [`StreamId`]. For example, even if a sender first transmits on
/// bidirectional stream 1, the first stream returned by [`Connection::accept_bi`](crate::query::ConnectionItem::accept_bi) on the receiver
/// will be bidirectional stream 0.
pub struct RecvStream<'a> {
    pub(crate) id: StreamId,
    pub(crate) proto_stream: quinn_proto::RecvStream<'a>,
}

/// Errors that can occur when reading data from a [`RecvStream`].
#[derive(Debug, Error)]
pub enum RecvError {
    /// No more data is currently available on this stream.
    #[error("Stream blocked")]
    Blocked,
    /// The peer abandoned transmitting data on this stream, by calling [`SendStream::reset()`].
    /// Includes an application-defined error code.
    #[error("Stream reset by peer. Error code: {0}")]
    Reset(VarInt),
    /// The stream does not exist, or has already been stopped, finished or reset.
    #[error("Stream does not exist or has been closed")]
    ClosedStream,
    /// Attempted an ordered read following an unordered read.
    ///
    /// Performing an unordered read allows discontinuities to arise
    /// in the receive buffer of a stream which cannot be recovered,
    /// making further ordered reads impossible.
    #[error("Attempted an ordered read following an unordered read")]
    IllegalOrderedRead,
}

impl From<ReadError> for RecvError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::Blocked => Self::Blocked,
            ReadError::Reset(e) => Self::Reset(e),
        }
    }
}

impl From<ReadableError> for RecvError {
    fn from(e: ReadableError) -> Self {
        match e {
            ReadableError::ClosedStream => Self::ClosedStream,
            ReadableError::IllegalOrderedRead => Self::IllegalOrderedRead,
        }
    }
}

/// Hack to automatically call finalize on quinn chunks before dropping them, because otherwise they panic.
/// finalize consumes self but drop gives you &mut self, so `Option::take` is needed to make this work.
/// This makes writing functions that use chunks way better as you don't need to put calls to finalize everywhere.
struct Chunks<'a>(Option<quinn_proto::Chunks<'a>>);

impl Chunks<'_> {
    fn next(&mut self, max_length: usize) -> Result<Option<Chunk>, ReadError> {
        self.0.as_mut().unwrap().next(max_length)
    }
}

impl<'a> From<quinn_proto::Chunks<'a>> for Chunks<'a> {
    fn from(chunks: quinn_proto::Chunks<'a>) -> Self {
        Self(Some(chunks))
    }
}

impl Drop for Chunks<'_> {
    fn drop(&mut self) {
        // All methods for accessing streams set `should_poll = true`, so if a transmit is needed the next poll will handle it
        let _ = self.0.take().unwrap().finalize();
    }
}

impl<'a> RecvStream<'a> {
    /// Get the ID of the stream.
    pub fn id(&self) -> StreamId {
        self.id
    }

    /// Read data contiguously from the stream.
    ///
    /// Returns the number of bytes read into `buf` on success, or `Ok(None)` if the stream was [`finish()`]ed.
    ///
    /// [`finish()`]: SendStream::finish()
    pub fn read(&mut self, mut buf: &mut [u8]) -> Result<Option<usize>, RecvError> {
        let mut chunks: Chunks = self.proto_stream.read(true)?.into();

        let mut read = 0;
        while !buf.is_empty() {
            match chunks.next(buf.len()) {
                // There is more data to read
                Ok(Some(chunk)) => {
                    // This can't panic because the chunk length is capped to the buf length
                    let len = chunk.bytes.len();
                    buf[..len].copy_from_slice(&chunk.bytes);
                    buf = &mut buf[len..];
                    read += len;
                }
                _ if read > 0 => break, // Successfully read some data, the error will propagate on the next read call
                Ok(None) => return Ok(None), // Stream finished
                Err(e) => return Err(e.into()), // No data or stream reset
            }
        }

        Ok(Some(read))
    }

    /// Read the next chunk of data from the stream. Slightly more efficient than `read` due to not copying.
    ///
    /// Returns `Ok(None)` if the stream was [`finish()`]ed. Otherwise, returns a chunk of data and its offset in the stream.
    /// If `ordered` is `true`, the chunk's offset will be immediately after the last data read from the stream.
    /// If `ordered` is `false`, chunks may be received in any order, and the chunk's offset can be used to determine
    /// ordering in the caller. Unordered reads are less prone to head-of-line blocking within a stream,
    /// but require the application to manage reassembling the original data.
    ///
    /// While most applications will prefer to consume stream data in order, unordered reads can
    /// improve performance when packet loss occurs and data cannot be retransmitted before the flow
    /// control window is filled. On any given stream, you can switch from ordered to unordered
    /// reads, but ordered reads on streams that have seen previous unordered reads will return
    /// `ReadError::IllegalOrderedRead`.
    ///
    /// The `max_length` parameter limits the maximum size of the returned `Bytes` value;
    /// passing `usize::MAX` will yield the best performance.
    /// Chunk boundaries do not correspond to peer writes, and hence cannot be used as framing.
    ///
    /// [`finish()`]: SendStream::finish()
    pub fn read_chunk(
        &mut self,
        max_length: usize,
        ordered: bool,
    ) -> Result<Option<Chunk>, RecvError> {
        let mut chunks: Chunks = self.proto_stream.read(ordered)?.into();
        chunks.next(max_length).map_err(Into::into)
    }

    /// Read ordered chunks of data from the stream.
    ///
    /// Fills `bufs` with chunks of data read contiguously from the stream,
    /// returning how many chunks were read on success, or returns `Ok(None)` if the stream was [`finish()`]ed.
    ///
    /// Chunk boundaries do not correspond to peer writes, and hence cannot be used as framing.
    ///
    /// [`finish()`]: SendStream::finish()
    pub fn read_chunks(&mut self, bufs: &mut [Bytes]) -> Result<Option<usize>, RecvError> {
        let mut chunks: Chunks = self.proto_stream.read(true)?.into();

        let mut read = 0;
        while read < bufs.len() {
            match chunks.next(usize::MAX) {
                // There is more data to read
                Ok(Some(chunk)) => {
                    bufs[read] = chunk.bytes;
                    read += 1;
                }
                _ if read > 0 => break, // Successfully read some data, the error will propagate on the next read call
                Ok(None) => return Ok(None), // Stream finished
                Err(e) => return Err(e.into()), // No data or stream reset
            }
        }

        Ok(Some(read))
    }

    /// Stop accepting data.
    ///
    /// Discards unread data and notifies the peer to stop transmitting. Once stopped, further
    /// attempts to operate on a stream will return `ClosedStream` errors.
    pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        self.proto_stream.stop(error_code)
    }

    /// Check whether the stream has been reset by the peer, returning the reset error code if so.
    ///
    /// After returning `Ok(Some(_))` once, stream state will be discarded and all future calls will
    /// return `Err(ClosedStream)`.
    pub fn received_reset(&mut self) -> Result<Option<VarInt>, ClosedStream> {
        self.proto_stream.received_reset()
    }
}
