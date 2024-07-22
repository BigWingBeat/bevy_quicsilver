use bevy_ecs::{
    bundle::Bundle,
    component::{Component, ComponentHooks, StorageType},
    entity::Entity,
    query::{QueryEntityError, QuerySingleError},
    system::{Query, SystemParam},
};
use bytes::Bytes;
use quinn_proto::{Chunks, ClosedStream, FinishError, ReadableError, StreamId, VarInt, WriteError};

use crate::connection::ConnectionImpl;

/// A [`SystemParam`] for querying streams on entities.
#[derive(Debug, SystemParam)]
pub struct Streams<'w, 's> {
    connection: Query<'w, 's, &'static mut ConnectionImpl>,
    send_stream: Query<'w, 's, &'static mut SendStreamImpl>,
    recv_stream: Query<'w, 's, &'static mut RecvStreamImpl>,
}

impl Streams<'_, '_> {
    /// Returns the [`SendStream`] for the given entity
    ///
    /// [`SendStream`]: quinn_proto::SendStream
    pub fn send_stream(&mut self, entity: Entity) -> Result<SendStream<'_>, QueryEntityError> {
        let stream = self.send_stream.get_mut(entity)?.into_inner();
        let connection = self.connection.get_mut(stream.connection)?.into_inner();
        connection.should_poll = true;
        Ok(SendStream::new(connection, stream))
    }

    /// Returns a single [`SendStream`] when there is exactly one.
    ///
    /// If the number of [`SendStream`]s is not exactly one, a [`QuerySingleError`] is returned instead.
    pub fn send_stream_single(&mut self) -> Result<SendStream<'_>, QuerySingleError> {
        let stream = self.send_stream.get_single_mut()?.into_inner();
        let connection = self
            .connection
            .get_mut(stream.connection)
            .map_err(|_| QuerySingleError::NoEntities(""))?
            .into_inner();
        connection.should_poll = true;
        Ok(SendStream::new(connection, stream))
    }

    /// Returns the [`RecvStream`] for the given entity
    ///
    /// [`RecvStream`]: quinn_proto::RecvStream
    pub fn recv_stream(&mut self, entity: Entity) -> Result<RecvStream<'_>, QueryEntityError> {
        let stream = self.recv_stream.get_mut(entity)?.into_inner();
        let connection = self.connection.get_mut(stream.connection)?.into_inner();
        connection.should_poll = true;
        Ok(RecvStream::new(connection, stream))
    }

    /// Returns a single [`RecvStream`] when there is exactly one.
    ///
    /// If the number of [`RecvStream`]s is not exactly one, a [`QuerySingleError`] is returned instead.
    pub fn recv_stream_single(&mut self) -> Result<RecvStream<'_>, QuerySingleError> {
        let stream = self.recv_stream.get_single_mut()?.into_inner();
        let connection = self
            .connection
            .get_mut(stream.connection)
            .map_err(|_| QuerySingleError::NoEntities(""))?
            .into_inner();
        connection.should_poll = true;
        Ok(RecvStream::new(connection, stream))
    }
}

/// A stream that can only be used to send data.
///
/// If despawned or removed, streams that haven't been explicitly [`reset()`] will be implicitly [`finish()`]ed,
/// continuing to (re)transmit previously written data until it has been fully acknowledged or the
/// connection is closed.
pub struct SendStream<'a> {
    proto_stream: quinn_proto::SendStream<'a>,
    ecs_stream: &'a mut SendStreamImpl,
}

impl<'a> SendStream<'a> {
    fn new(connection: &'a mut ConnectionImpl, ecs_stream: &'a mut SendStreamImpl) -> Self {
        Self {
            proto_stream: connection.send_stream(ecs_stream.stream),
            ecs_stream,
        }
    }

    /// Send data on the stream
    pub fn write(&mut self, data: &[u8]) -> Result<(), WriteError> {
        match self.proto_stream.write(data) {
            Ok(written) => {
                let remaining = data.len() - written;
                if remaining > 0 {
                    self.ecs_stream
                        .pending_writes
                        .push(Bytes::copy_from_slice(&data[remaining..]));
                }
                Ok(())
            }
            Err(WriteError::Blocked) => {
                self.ecs_stream
                    .pending_writes
                    .push(Bytes::copy_from_slice(data));
                Ok(())
            }
            result => result.map(|_| ()),
        }
    }

    /// Send data on the stream
    ///
    /// Slightly more efficient than `write` due to not copying
    pub fn write_chunks(&mut self, data: &mut [Bytes]) -> Result<(), WriteError> {
        match self.proto_stream.write_chunks(data) {
            Ok(_) => {
                self.ecs_stream
                    .pending_writes
                    .extend(data.iter().filter(|&bytes| !bytes.is_empty()).cloned());
                Ok(())
            }
            Err(WriteError::Blocked) => {
                self.ecs_stream.pending_writes.extend_from_slice(data);
                Ok(())
            }
            result => result.map(|_| ()),
        }
    }

    /// Check if the stream was stopped by the peer, get the reason if it was
    ///
    /// Returns `Some` with the stop error code when the stream is stopped by the peer.
    /// Returns `None` if the stream is still alive and has not been stopped.
    /// Returns `Err` when the stream is [`finish()`](Self::finish)ed or [`reset()`] locally and all stream data has been
    /// received (but not necessarily processed) by the peer, after which it is no longer meaningful
    /// for the stream to be stopped.
    pub fn stopped(&self) -> Result<Option<VarInt>, ClosedStream> {
        self.proto_stream.stopped()
    }

    /// Notify the peer that no more data will ever be written to this stream
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

    /// Set the priority of the stream
    ///
    /// Every send stream has an initial priority of 0. Locally buffered data from streams with
    /// higher priority will be transmitted before data from streams with lower priority. Changing
    /// the priority of a stream with pending data may only take effect after that data has been
    /// transmitted. Using many different priority levels per connection may have a negative
    /// impact on performance.
    pub fn set_priority(&mut self, priority: i32) -> Result<(), ClosedStream> {
        self.proto_stream.set_priority(priority)
    }

    /// Get the priority of the stream
    pub fn priority(&self) -> Result<i32, ClosedStream> {
        self.proto_stream.priority()
    }
}

/// A bundle for adding a [`SendStream`] to an entity
#[derive(Debug, Bundle)]
pub struct SendStreamBundle {
    stream: SendStreamImpl,
}

impl SendStreamBundle {
    pub(crate) fn new(connection: Entity, stream: StreamId) -> Self {
        Self {
            stream: SendStreamImpl {
                connection,
                stream,
                pending_writes: Vec::new(),
            },
        }
    }
}

/// Underlying component type behind the [`SendStream`] and [`SendStreamBundle`] types
#[derive(Debug, Clone)]
pub(crate) struct SendStreamImpl {
    connection: Entity,
    pub(crate) stream: StreamId,
    pub(crate) pending_writes: Vec<Bytes>,
}

impl Component for SendStreamImpl {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks
            .on_insert(|mut world, entity, _component_id| {
                let stream = world.get::<Self>(entity).unwrap();
                let id = stream.stream;
                let Some(mut connection) = world.get_mut::<ConnectionImpl>(stream.connection)
                else {
                    return;
                };

                connection.streams.insert(id, entity);
            })
            .on_replace(|mut world, entity, _component_id| {
                let stream = world.get::<Self>(entity).unwrap();
                let id = stream.stream;

                let Some(mut connection) = world.get_mut::<ConnectionImpl>(stream.connection)
                else {
                    return;
                };

                connection.should_poll = true;

                let mut stream = connection.send_stream(id);

                // https://github.com/quinn-rs/quinn/blob/0.11.2/quinn/src/send_stream.rs#L287
                match stream.finish() {
                    Ok(()) => {}
                    Err(FinishError::Stopped(reason)) => {
                        let _ = stream.reset(reason);
                    }
                    // Already finished or reset, which is fine
                    Err(FinishError::ClosedStream) => {}
                }
            });
    }
}

/// A stream that can only be used to receive data.
///
/// If despawned or removed, [`stop(0)`] is implicitly called, unless the stream has already been stopped.
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
/// logging the [`id`] of each. Streams are always accepted in the same order as they are created,
/// i.e. ascending order by [`StreamId`]. For example, even if a sender first transmits on
/// bidirectional stream 1, the first stream returned by [`Connection::accept_bi`] on the receiver
/// will be bidirectional stream 0.
pub struct RecvStream<'a> {
    proto_stream: quinn_proto::RecvStream<'a>,
}

impl<'a> RecvStream<'a> {
    fn new(connection: &'a mut ConnectionImpl, ecs_stream: &'a mut RecvStreamImpl) -> Self {
        Self {
            proto_stream: connection.recv_stream(ecs_stream.stream),
        }
    }

    /// Read data from the stream
    ///
    /// `ordered` will make sure the returned chunk's offset will have an offset exactly equal to
    /// the previously returned offset plus the previously returned bytes' length.
    /// Unordered reads are less prone to head-of-line blocking within a stream,
    /// but require the application to manage reassembling the original data.
    ///
    /// The `max_length` parameter of [`next()`] on the returned [`Chunks`] limits the maximum size
    /// of the returned `Bytes` value; passing `usize::MAX` will yield the best performance.
    ///
    /// `Chunks::next()` returns `Ok(None)` if the stream was finished. Otherwise, returns a segment of data and its
    /// offset in the stream. If `ordered` is `false`, segments may be received in any order, and
    /// the `Chunk`'s `offset` field can be used to determine ordering in the caller.
    ///
    /// While most applications will prefer to consume stream data in order, unordered reads can
    /// improve performance when packet loss occurs and data cannot be retransmitted before the flow
    /// control window is filled. On any given stream, you can switch from ordered to unordered
    /// reads, but ordered reads on streams that have seen previous unordered reads will return
    /// `ReadError::IllegalOrderedRead`.
    ///
    /// [`next()`]: Chunks::next
    pub fn read(&mut self, ordered: bool) -> Result<Chunks<'_>, ReadableError> {
        self.proto_stream.read(ordered)
    }

    /// Stop accepting data
    ///
    /// Discards unread data and notifies the peer to stop transmitting. Once stopped, further
    /// attempts to operate on a stream will return `ClosedStream` errors.
    pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        self.proto_stream.stop(error_code)
    }

    /// Check whether the stream has been reset by the peer, returning the reset error code if so
    ///
    /// After returning `Ok(Some(_))` once, stream state will be discarded and all future calls will
    /// return `Err(ClosedStream)`.
    pub fn received_reset(&mut self) -> Result<Option<VarInt>, ClosedStream> {
        self.proto_stream.received_reset()
    }
}

/// A bundle for adding a [`RecvStream`] to an entity
#[derive(Debug, Bundle)]
pub struct RecvStreamBundle {
    stream: RecvStreamImpl,
}

impl RecvStreamBundle {
    pub(crate) fn new(connection: Entity, stream: StreamId) -> Self {
        Self {
            stream: RecvStreamImpl { connection, stream },
        }
    }
}

/// Underlying component type behind the [`RecvStream`] and [`RecvStreamBundle`] types
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecvStreamImpl {
    connection: Entity,
    stream: StreamId,
}

impl Component for RecvStreamImpl {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks
            .on_insert(|mut world, entity, _component_id| {
                let stream = world.get::<Self>(entity).unwrap();
                let id = stream.stream;
                let Some(mut connection) = world.get_mut::<ConnectionImpl>(stream.connection)
                else {
                    return;
                };

                connection.streams.insert(id, entity);
            })
            .on_replace(|mut world, entity, _component_id| {
                let stream = world.get::<Self>(entity).unwrap();
                let id = stream.stream;

                let Some(mut connection) = world.get_mut::<ConnectionImpl>(stream.connection)
                else {
                    return;
                };

                connection.should_poll = true;

                let mut stream = connection.recv_stream(id);
                let _ = stream.stop(0u32.into());
            });
    }
}
