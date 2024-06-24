use bevy_ecs::{
    bundle::Bundle,
    component::Component,
    entity::{Entity, EntityHashMap},
    query::{Added, QueryEntityError},
    removal_detection::RemovedComponents,
    system::{Query, ResMut, Resource, SystemParam},
};
use quinn_proto::{FinishError, StreamId};

use crate::connection::ConnectionImpl;

/// A [`SystemParam`] for querying [`SendStream`]s from entities.
///
/// A `SendStream` is a stream that can only be used to send data.
///
/// If despawned or removed, streams that haven't been explicitly [`reset()`] will be implicitly [`finish()`]ed,
/// continuing to (re)transmit previously written data until it has been fully acknowledged or the
/// connection is closed.
///
/// [`SendStream`]: quinn_proto::SendStream
/// [`reset()`]: quinn_proto::SendStream::reset
/// [`finish()`]: quinn_proto::SendStream::finish
#[derive(Debug, SystemParam)]
pub struct SendStream<'w, 's> {
    connection: Query<'w, 's, &'static mut ConnectionImpl>,
    stream: Query<'w, 's, &'static mut SendStreamImpl>,
}

impl SendStream<'_, '_> {
    /// Returns the [`SendStream`] for the given entity
    ///
    /// [`SendStream`]: quinn_proto::SendStream
    pub fn get(&mut self, entity: Entity) -> Result<quinn_proto::SendStream<'_>, QueryEntityError> {
        let stream = self.stream.get(entity)?;
        let connection = self.connection.get_mut(stream.connection)?;
        Ok(connection.into_inner().send_stream(stream.stream))
    }
}

/// A bundle for adding a [`SendStream`] to an entity
///
/// [`SendStream`]: quinn_proto::SendStream
#[derive(Debug, Bundle)]
pub struct SendStreamBundle {
    stream: SendStreamImpl,
}

impl SendStreamBundle {
    pub(crate) fn new(connection: Entity, stream: StreamId) -> Self {
        Self {
            stream: SendStreamImpl { connection, stream },
        }
    }
}

/// Underlying component type behind the [`SendStreamBundle`] bundle and [`SendStream`] systemparam types
#[derive(Debug, Component)]
pub(crate) struct SendStreamImpl {
    connection: Entity,
    stream: StreamId,
}

/// A [`SystemParam`] for querying [`RecvStream`]s from entities.
///
/// A `RecvStream` is a stream that can only be used to receive data.
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
///
/// [`RecvStream`]: quinn_proto::RecvStream
/// [`ReadError`]: crate::ReadError
/// [`stop(0)`]: RecvStream::stop
/// [`id`]: RecvStream::id
/// [`Connection::accept_bi`]: crate::Connection::accept_bi
#[derive(Debug, SystemParam)]
pub struct RecvStream<'w, 's> {
    connection: Query<'w, 's, &'static mut ConnectionImpl>,
    stream: Query<'w, 's, &'static mut RecvStreamImpl>,
}

impl RecvStream<'_, '_> {
    /// Returns the [`RecvStream`] for the given entity
    ///
    /// [`RecvStream`]: quinn_proto::RecvStream
    pub fn get(&mut self, entity: Entity) -> Result<quinn_proto::RecvStream<'_>, QueryEntityError> {
        let stream = self.stream.get(entity)?;
        let connection = self.connection.get_mut(stream.connection)?;
        Ok(connection.into_inner().recv_stream(stream.stream))
    }
}

/// A bundle for adding a [`RecvStream`] to an entity
///
/// [`RecvStream`]: quinn_proto::RecvStream
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

/// Underlying component type behind the [`RecvStreamBundle`] bundle and [`RecvStream`] systemparam types
#[derive(Debug, Component)]
pub(crate) struct RecvStreamImpl {
    connection: Entity,
    stream: StreamId,
}

#[derive(Debug, Resource, Default)]
pub(crate) struct StreamEntities(EntityHashMap<(Entity, StreamId)>);

pub(crate) fn handle_added_streams(
    mut mapping: ResMut<StreamEntities>,
    send: Query<(Entity, &SendStreamImpl), Added<SendStreamImpl>>,
    recv: Query<(Entity, &RecvStreamImpl), Added<RecvStreamImpl>>,
) {
    for (entity, send) in send.iter() {
        mapping.0.insert(entity, (send.connection, send.stream));
    }

    for (entity, recv) in recv.iter() {
        mapping.0.insert(entity, (recv.connection, recv.stream));
    }
}

pub(crate) fn handle_removed_streams(
    mut mapping: ResMut<StreamEntities>,
    mut send: RemovedComponents<SendStreamImpl>,
    mut recv: RemovedComponents<RecvStreamImpl>,
    mut connections: Query<&mut ConnectionImpl>,
) {
    for entity in send.read() {
        let Some((connection, id)) = mapping.0.remove(&entity) else {
            continue;
        };

        let Ok(mut connection) = connections.get_mut(connection) else {
            continue;
        };

        let mut stream = connection.send_stream(id);

        // https://github.com/quinn-rs/quinn/blob/0.11.2/quinn/src/send_stream.rs#L287
        match stream.finish() {
            Ok(()) => {}
            Err(FinishError::Stopped(reason)) => {
                let _ = stream.reset(reason);
            }
            // Already finished or reset, which is fine.
            Err(FinishError::ClosedStream) => {}
        }
    }

    for entity in recv.read() {
        let Some((connection, id)) = mapping.0.remove(&entity) else {
            continue;
        };

        let Ok(mut connection) = connections.get_mut(connection) else {
            continue;
        };

        let mut stream = connection.recv_stream(id);

        let _ = stream.stop(0u32.into());
    }
}
