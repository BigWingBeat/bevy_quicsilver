use bevy_ecs::{
    component::{Component, ComponentHooks, StorageType},
    entity::Entity,
    query::{AnyOf, Has, QueryData, QueryEntityError},
    system::{Commands, Query, Res},
    world::{DeferredWorld, EntityWorldMut},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::Bytes;
use hashbrown::HashMap;
use quinn_proto::{
    congestion::Controller, crypto::ExportKeyingMaterialError, ClosedStream, ConnectionHandle,
    ConnectionStats, Dir, EndpointEvent, Event, SendDatagramError, StreamEvent, StreamId, Transmit,
    VarInt,
};
use thiserror::Error;

use crate::{
    endpoint::Endpoint,
    streams::{RecvStream, SendStream},
    KeepAlive, KeepAliveEntityCommandsExt,
};

use std::{
    any::Any,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// An observer trigger that is fired whenever a [`Connecting`] component on an entity encounters an error.
#[derive(Debug, Error, bevy_ecs::event::Event)]
pub enum ConnectingError {
    /// The connection was lost
    #[error(transparent)]
    Lost(quinn_proto::ConnectionError),
    /// The connection has been aborted due to an I/O error
    #[error(transparent)]
    IoError(std::io::Error),
}

/// An observer trigger that is fired whenever a [`Connection`] component on an entity encounters an error.
#[derive(Debug, Error, bevy_ecs::event::Event)]
pub enum ConnectionError {
    /// The connection was lost
    #[error(transparent)]
    Lost(quinn_proto::ConnectionError),
    /// The connection has been aborted due to an I/O error
    #[error(transparent)]
    IoError(std::io::Error),
}

/// An observer trigger that is fired when a new incoming connection is accepted,
/// and the [`Incoming`](crate::Incoming) component on the entity has been replaced with a [`Connecting`] component.
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionAccepted;

/// An observer trigger that is fired when a connection is successfully established,
/// and the [`Connecting`] component on the entity has been replaced with a [`Connection`] component.
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionEstablished;

/// An observer trigger that is fired when a connection has been fully closed, and is just about to be despawned.
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionDrained;

/// An observer trigger that is fired when a connection's handshake data becomes available.
/// After this trigger is fired, [`Connecting::handshake_data()`](ConnectingItem::handshake_data) will begin returning [`Some`].
#[derive(Debug, bevy_ecs::event::Event)]
pub struct HandshakeDataReady;

/// An in-progress connection attempt, that has not yet been fully established.
///
/// When a [`ConnectionEstablished`] trigger is fired, this component is replaced with [`Connection`] on the target entity.
///
/// There can only ever be 1 connection on a given entity at a time. If this component is inserted onto an entity that already
/// has a connection, that connection will be immediately destroyed, without being closed or drained, and replaced with this one.
///
/// # Usage
/// ```
/// # use bevy_ecs::system::{Query, assert_is_system};
/// # use bevy_quicsilver::Connecting;
/// fn my_system(query: Query<&Connecting>) {
///     for connecting in query.iter() {
///         println!("Connecting to: {}", connecting.remote_address());
///     }
/// }
/// # assert_is_system(my_system);
/// ```
// TODO: Use archetype invariants to specify that this and `Connection` are mutually exclusive
// See https://github.com/bevyengine/bevy/issues/1481
#[derive(Debug)]
pub struct Connecting(ConnectionImpl);

impl Component for Connecting {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks.on_insert(|mut world, entity, _component_id| {
            // To prevent there being more than 1 connection on this entity at a time
            world.commands().entity(entity).remove::<Connection>();

            // Bookkeeping
            ConnectionImpl::on_insert(|world| &world.get::<Self>(entity).unwrap().0, world, entity);
        });
    }
}

impl Connecting {
    pub(crate) fn new(
        endpoint: Entity,
        handle: ConnectionHandle,
        connection: quinn_proto::Connection,
    ) -> Self {
        Self(ConnectionImpl::new(endpoint, handle, connection))
    }

    /// Parameters negotiated during the handshake.
    ///
    /// Returns `None` until the [`HandshakeDataReady`] observer trigger is fired for this entity.
    /// The dynamic type returned is determined by the configured [`Session`](crate::crypto::Session).
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// [`crypto::rustls::HandshakeData`](quinn_proto::crypto::rustls::HandshakeData).
    pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.0.handshake_data()
    }

    /// The peer's UDP address.
    ///
    /// If [`ServerConfig::migration()`](crate::ServerConfig::migration) is `true`, clients may change addresses at will, e.g. when
    /// switching to a cellular internet connection.
    pub fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }

    /// The local IP address which was used when the peer established the connection.
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients, or when the platform does not expose this
    /// information. See [`quinn_udp::RecvMeta::dst_ip`] for a list of supported platforms.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.0.local_ip()
    }

    /// Returns connection statistics.
    pub fn stats(&self) -> ConnectionStats {
        self.0.stats()
    }
}

/// A fully established QUIC connection.
///
/// There can only ever be 1 connection on a given entity at a time. If this component is inserted onto an entity that already
/// has a connection, that connection will be immediately destroyed, without being closed or drained, and replaced with this one.
///
/// # Usage
/// ```
/// # use bevy_ecs::system::{Query, assert_is_system};
/// # use bevy_quicsilver::Connection;
/// fn my_system(query: Query<&Connection>) {
///     for connection in query.iter() {
///         println!("Connected to: {}", connection.remote_address());
///     }
/// }
/// # assert_is_system(my_system);
/// ```
#[derive(Debug)]
// TODO: Use archetype invariants to specify that this and `Connecting` are mutually exclusive
// See https://github.com/bevyengine/bevy/issues/1481
pub struct Connection(ConnectionImpl);

impl Component for Connection {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks.on_insert(|mut world, entity, _component_id| {
            // To prevent there being more than 1 connection on this entity at a time
            world.commands().entity(entity).remove::<Connecting>();

            // Bookkeeping
            ConnectionImpl::on_insert(|world| &world.get::<Self>(entity).unwrap().0, world, entity);
        });
    }
}

impl Connection {
    /// Whether the connection is closed.
    ///
    /// Closed connections cannot transport any further data. A connection becomes closed when
    /// either peer application intentionally closes it, or when either transport layer detects an
    /// error such as a time-out or certificate validation failure.
    ///
    /// When the connection becomes closed, a [`ConnectionError`] event is fired, and after a brief timeout,
    /// the connection becomes drained, and the entity is despawned.
    /// If the entity has a [`KeepAlive`] component, only the connection component is removed instead.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Close the connection immediately.
    ///
    /// Pending operations will fail immediately with [`ConnectionError::LocallyClosed`]. Delivery
    /// of data on unfinished streams is not guaranteed, so the application must call this only
    /// when all important communications have been completed, e.g. by calling [`finish`] on
    /// outstanding [`SendStream`]s and waiting for the streams to become fully closed.
    ///
    /// `error_code` and `reason` are not interpreted, and are provided directly to the peer.
    ///
    /// `reason` will be truncated to fit in a single packet with overhead; to improve odds that it
    /// is preserved in full, it should be kept under 1KiB.
    ///
    /// [`ConnectionError::LocallyClosed`]: quinn_proto::ConnectionError::LocallyClosed
    /// [`finish`]: crate::SendStream::finish
    /// [`SendStream`]: crate::SendStream
    pub fn close(&mut self, error_code: VarInt, reason: Bytes) {
        self.0.close(Instant::now(), error_code, reason);
    }

    /// Initiate a new outgoing unidirectional stream.
    /// To reuse the stream later, the [`SendStream::id`] should be stored and passed to [`Self::send_stream`].
    ///
    /// Streams are cheap and instantaneous to open unless blocked by flow control. As a
    /// consequence, the peer won't be notified that a stream has been opened until the stream is
    /// actually used.
    ///
    /// Returns `None` if outgoing unidirectional streams are currently exhausted.
    #[must_use = "The stream must be used for the peer to be notified that it has been opened"]
    pub fn open_uni(&mut self) -> Option<SendStream<'_>> {
        self.0.open_uni()
    }

    /// Initiate a new outgoing bidirectional stream.
    /// The returned stream ID can be passed to both [`Self::send_stream`] and [`Self::recv_stream`].
    ///
    /// Streams are cheap and instantaneous to open unless blocked by flow control. As a
    /// consequence, the peer won't be notified that a stream has been opened until the stream is
    /// actually used to send data. Calling [`open_bi()`] then waiting to receive data from the [`RecvStream`] without writing
    /// anything to the [`SendStream`] first will never succeed.
    ///
    /// Returns `None` if outgoing bidirectional streams are currently exhausted.
    ///
    /// [`open_bi()`]: Self::open_bi
    /// [`SendStream`]: crate::SendStream
    /// [`RecvStream`]: crate::RecvStream
    #[must_use = "The stream must be used for the peer to be notified that it has been opened"]
    pub fn open_bi(&mut self) -> Option<StreamId> {
        self.0.open_bi()
    }

    /// Accept the next incoming unidirectional stream.
    /// To reuse the stream later, the [`SendStream::id`] should be stored and passed to [`Self::recv_stream`].
    ///
    /// Returns `None` if there are no new incoming unidirectional streams for this connection.
    /// Has no impact on the data flow-control or stream concurrency limits.
    pub fn accept_uni(&mut self) -> Option<RecvStream<'_>> {
        self.0.accept_uni()
    }

    /// Accept the next incoming bidirectional stream.
    /// The returned stream ID can be passed to both [`Self::send_stream`] and [`Self::recv_stream`].
    ///
    /// **Important Note**: The `Connection` that calls [`open_bi()`] must write to its [`SendStream`]
    /// before the other `Connection` is able to `accept_bi()`.
    /// Calling [`open_bi()`] then waiting to receive data from the [`RecvStream`] without writing
    /// anything to the [`SendStream`] first will never succeed.
    ///
    /// Returns `None` if there are no new incoming bidirectional streams for this connection.
    /// Has no impact on the data flow-control or stream concurrency limits.
    ///
    /// [`open_bi()`]: Self::open_bi
    /// [`SendStream`]: crate::SendStream
    /// [`RecvStream`]: crate::RecvStream
    pub fn accept_bi(&mut self) -> Option<StreamId> {
        self.0.accept_bi()
    }

    /// Get the send stream associated with the given stream ID.
    /// Returns an error if the stream does not exist, or has already been stopped, finished or reset.
    pub fn send_stream(&mut self, id: StreamId) -> Result<SendStream<'_>, ClosedStream> {
        self.0.send_stream(id)
    }

    /// Get the receive stream associated with the given stream ID.
    /// Returns an error if the stream does not exist, or has already been stopped, finished or reset.
    pub fn recv_stream(&mut self, id: StreamId) -> Result<RecvStream<'_>, ClosedStream> {
        self.0.recv_stream(id)
    }

    /// Transmit `data` as an unreliable, unordered application datagram.
    ///
    /// Application datagrams are a low-level primitive. They may be lost or delivered out of order,
    /// and `data` must both fit inside a single QUIC packet and be smaller than the maximum size
    /// dictated by the peer.
    ///
    /// Previously queued datagrams which are still unsent may be discarded to make space for this datagram,
    /// in order of oldest to newest.
    pub fn send_datagram(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.0.send_datagram(data)
    }

    /// Transmit `data` as an unreliable, unordered application datagram.
    ///
    /// Unlike [`send_datagram()`], this method will wait for buffer space during congestion
    /// conditions, which effectively prioritizes old datagrams over new datagrams.
    ///
    /// See [`send_datagram()`] for details.
    ///
    /// [`send_datagram()`]: Self::send_datagram
    pub fn send_datagram_wait(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.0.send_datagram_wait(data)
    }

    /// Receive an unreliable, unordered application datagram.
    /// Returns `None` if there are no received datagrams waiting to be read.
    pub fn read_datagram(&mut self) -> Option<Bytes> {
        self.0.read_datagram()
    }

    /// Compute the maximum size of datagrams that can be sent.
    ///
    /// Returns `None` if datagrams are unsupported by the peer or disabled locally.
    ///
    /// This may change over the lifetime of a connection according to variation in the path MTU
    /// estimate. The peer can also enforce an arbitrarily small fixed limit, but if the peer's
    /// limit is large this is guaranteed to be a little over a kilobyte at minimum.
    ///
    /// Not necessarily the maximum size of received datagrams.
    pub fn max_datagram_size(&mut self) -> Option<usize> {
        self.0.max_datagram_size()
    }

    /// Bytes available in the outgoing datagram buffer.
    ///
    /// When greater than zero, sending a datagram of at most this size is guaranteed not to cause older datagrams to be dropped.
    pub fn datagram_send_buffer_space(&mut self) -> usize {
        self.0.datagram_send_buffer_space()
    }

    /// The peer's UDP address.
    ///
    /// If [`ServerConfig::migration()`](crate::ServerConfig::migration) is `true`, clients may change addresses at will, e.g. when
    /// switching to a cellular internet connection.
    pub fn remote_address(&self) -> SocketAddr {
        self.0.remote_address()
    }

    /// The local IP address which was used when the peer established the connection.
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients, or when the platform does not expose this
    /// information. See [`quinn_udp::RecvMeta::dst_ip`] for a list of supported platforms.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.0.local_ip()
    }

    /// Current best estimate of this connection's latency (round-trip time).
    pub fn rtt(&self) -> Duration {
        self.0.rtt()
    }

    /// Returns connection statistics.
    pub fn stats(&self) -> ConnectionStats {
        self.0.stats()
    }

    /// Current state of this connection's congestion control algorithm, for debugging purposes.
    pub fn congestion_state(&self) -> &dyn Controller {
        self.0.congestion_state()
    }

    /// Parameters negotiated during the handshake.
    ///
    /// Guranteed to return `Some` on fully established connections.
    /// The dynamic type returned is determined by the configured [`Session`](crate::crypto::Session).
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// [`crypto::rustls::HandshakeData`](crate::crypto::rustls::HandshakeData).
    pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.0.handshake_data()
    }

    /// Cryptographic identity of the peer.
    ///
    /// The dynamic type returned is determined by the configured [`Session`](crate::crypto::Session).
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// <code>Vec<[rustls::pki_types::CertificateDer]></code>.
    pub fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.0.peer_identity()
    }

    /// Derive keying material from this connection's TLS session secrets.
    ///
    /// When both peers call this method with the same `label` and `context`
    /// arguments and `output` buffers of equal length, they will get the
    /// same sequence of bytes in `output`. These bytes are cryptographically
    /// strong and pseudorandom, and are suitable for use as keying material.
    ///
    /// See [RFC5705](https://tools.ietf.org/html/rfc5705) for more information.
    pub fn export_keying_material(
        &self,
        output: &mut [u8],
        label: &[u8],
        context: &[u8],
    ) -> Result<(), ExportKeyingMaterialError> {
        self.0.export_keying_material(output, label, context)
    }

    /// Modify the number of remotely initiated unidirectional streams that may be concurrently open.
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_uni_streams(&mut self, count: VarInt) {
        self.0.set_max_concurrent_uni_streams(count);
    }

    /// Modify the number of remotely initiated bidirectional streams that may be concurrently open.
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_bi_streams(&mut self, count: VarInt) {
        self.0.set_max_concurrent_bi_streams(count);
    }
}

/// Internal helper `QueryData` to handle `Connecting` & `Connection` being distinct component types
#[derive(QueryData)]
#[query_data(mutable)]
pub(crate) struct ConnectionQuery {
    query: AnyOf<(&'static mut Connecting, &'static mut Connection)>,
}

impl<'w> ConnectionQueryItem<'w> {
    /// Get the connection and whether or not it is fully established
    pub(crate) fn get(self) -> (bool, &'w mut ConnectionImpl) {
        match self.query {
            (None, Some(c)) => (true, &mut c.into_inner().0),
            (Some(c), None) => (false, &mut c.into_inner().0),
            _ => unreachable!("Connecting and Connection are mutually exclusive"),
        }
    }
}

impl<'w> ConnectionQueryReadOnlyItem<'w> {
    /// Get the connection and whether or not it is fully established
    #[expect(dead_code)]
    pub(crate) fn get(self) -> (bool, &'w ConnectionImpl) {
        match self.query {
            (None, Some(c)) => (true, &c.0),
            (Some(c), None) => (false, &c.0),
            _ => unreachable!("Connecting and Connection are mutually exclusive"),
        }
    }
}

/// Underlying impl type behind the [`Connecting`] and [`Connection`] component types.
#[derive(Debug)]
pub(crate) struct ConnectionImpl {
    pub(crate) endpoint: Entity,
    pub(crate) handle: ConnectionHandle,
    connection: quinn_proto::Connection,
    timeout_timer: Option<(Timer, Instant)>,
    should_poll: bool,
    io_error: bool,
    blocked_transmit: Option<Transmit>,
    transmit_buf: Vec<u8>,
    pending_datagrams: Vec<Bytes>,
    pending_streams: HashMap<StreamId, Vec<Bytes>>,
}

impl ConnectionImpl {
    fn new(
        endpoint: Entity,
        handle: ConnectionHandle,
        connection: quinn_proto::Connection,
    ) -> Self {
        Self {
            endpoint,
            handle,
            connection,
            timeout_timer: None,
            should_poll: true,
            io_error: false,
            blocked_transmit: None,
            transmit_buf: Vec::new(),
            pending_datagrams: Vec::new(),
            pending_streams: HashMap::new(),
        }
    }

    fn is_drained(&self) -> bool {
        self.connection.is_drained()
    }

    fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }

    fn close(&mut self, now: Instant, error_code: VarInt, reason: Bytes) {
        self.connection.close(now, error_code, reason);
        self.should_poll = true;
    }

    fn open_uni(&mut self) -> Option<SendStream<'_>> {
        self.should_poll = true;
        self.connection.streams().open(Dir::Uni).map(|id| {
            let write_buffer = self.pending_streams.entry(id).or_default();
            SendStream::new(id, write_buffer, self.connection.send_stream(id))
        })
    }

    fn open_bi(&mut self) -> Option<StreamId> {
        self.should_poll = true;
        self.connection.streams().open(Dir::Bi).inspect(|&id| {
            self.pending_streams.insert(id, Vec::new());
        })
    }

    fn accept_uni(&mut self) -> Option<RecvStream<'_>> {
        self.should_poll = true;
        self.connection
            .streams()
            .accept(Dir::Uni)
            .map(|id| RecvStream::new(id, self.connection.recv_stream(id)))
    }

    fn accept_bi(&mut self) -> Option<StreamId> {
        self.should_poll = true;
        self.connection.streams().accept(Dir::Bi).inspect(|&id| {
            self.pending_streams.insert(id, Vec::new());
        })
    }

    fn send_stream(&mut self, id: StreamId) -> Result<SendStream<'_>, ClosedStream> {
        self.should_poll = true;
        self.pending_streams
            .get_mut(&id)
            .filter(|_| id.dir() == Dir::Bi || id.initiator() == self.connection.side())
            .map(|write_buffer| SendStream::new(id, write_buffer, self.connection.send_stream(id)))
            .ok_or_else(ClosedStream::new)
    }

    fn recv_stream(&mut self, id: StreamId) -> Result<RecvStream<'_>, ClosedStream> {
        self.should_poll = true;
        (id.dir() == Dir::Bi || id.initiator() != self.connection.side())
            .then(|| RecvStream::new(id, self.connection.recv_stream(id)))
            .ok_or_else(ClosedStream::new)
    }

    fn send_datagram(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.should_poll = true;
        self.connection.datagrams().send(data, true)
    }

    fn send_datagram_wait(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.should_poll = true;
        self.connection
            .datagrams()
            .send(data, false)
            .or_else(|error| match error {
                SendDatagramError::Blocked(data) => {
                    self.pending_datagrams.push(data);
                    Ok(())
                }
                e => Err(e),
            })
    }

    fn read_datagram(&mut self) -> Option<Bytes> {
        self.should_poll = true;
        self.connection.datagrams().recv()
    }

    fn max_datagram_size(&mut self) -> Option<usize> {
        self.connection.datagrams().max_size()
    }

    fn datagram_send_buffer_space(&mut self) -> usize {
        self.connection.datagrams().send_buffer_space()
    }

    fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    fn local_ip(&self) -> Option<IpAddr> {
        self.connection.local_ip()
    }

    fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    fn stats(&self) -> ConnectionStats {
        self.connection.stats()
    }

    fn congestion_state(&self) -> &dyn Controller {
        self.connection.congestion_state()
    }

    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.connection.crypto_session().handshake_data()
    }

    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.connection.crypto_session().peer_identity()
    }

    fn export_keying_material(
        &self,
        output: &mut [u8],
        label: &[u8],
        context: &[u8],
    ) -> Result<(), ExportKeyingMaterialError> {
        self.connection
            .crypto_session()
            .export_keying_material(output, label, context)
    }

    fn set_max_concurrent_uni_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_streams(Dir::Uni, count);
        self.should_poll = true;
    }

    fn set_max_concurrent_bi_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_streams(Dir::Bi, count);
        self.should_poll = true;
    }

    fn flush_pending_datagrams(&mut self) {
        self.pending_datagrams.retain(|datagram| {
            matches!(
                self.connection.datagrams().send(datagram.clone(), false),
                Err(SendDatagramError::Blocked(_))
            )
        });
        self.should_poll = true;
    }

    pub(crate) fn handle_event(&mut self, event: quinn_proto::ConnectionEvent) {
        self.connection.handle_event(event);
        self.should_poll = true;
    }

    fn handle_timeout(&mut self, now: Instant, delta: Duration) {
        if let Some((ref mut timer, _)) = self.timeout_timer {
            if timer.tick(delta).just_finished() {
                self.connection.handle_timeout(now);
                self.timeout_timer = None;
                self.should_poll = true;
            }
        }
    }

    fn poll_transmit(&mut self, now: Instant, max_datagrams: usize) -> Option<(Transmit, &[u8])> {
        // Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/connection.rs#L952
        if let Some(transmit) = self.blocked_transmit.take() {
            let data = &self.transmit_buf[..transmit.size];
            return Some((transmit, data));
        }

        self.transmit_buf.clear();
        self.transmit_buf
            .reserve(self.connection.current_mtu() as _);

        self.connection
            .poll_transmit(now, max_datagrams, &mut self.transmit_buf)
            .map(|transmit| {
                let data = &self.transmit_buf[..transmit.size];
                (transmit, data)
            })
    }

    fn poll_timeout(&mut self, now: Instant) {
        match self.connection.poll_timeout() {
            Some(timeout) => {
                // If the timeout hasn't changed since the last call, avoid unnecessarily recreating the timer
                if self
                    .timeout_timer
                    .as_ref()
                    .map_or(true, |&(_, previous_timeout)| previous_timeout != timeout)
                {
                    self.timeout_timer = Some((
                        Timer::new(
                            timeout
                                .checked_duration_since(now)
                                .expect("Monotonicity violated"),
                            TimerMode::Once,
                        ),
                        timeout,
                    ));
                }
            }
            None => self.timeout_timer = None,
        }
    }

    fn poll_endpoint_events(&mut self) -> impl Iterator<Item = EndpointEvent> + '_ {
        std::iter::from_fn(|| self.connection.poll_endpoint_events())
    }

    fn poll(&mut self) -> Option<Event> {
        self.connection.poll()
    }

    fn on_insert(
        this: impl for<'a> FnOnce(&'a DeferredWorld<'a>) -> &'a Self,
        mut world: DeferredWorld,
        entity: Entity,
    ) {
        // Bookkeeping so the endpoint knows the entity we're on
        let this = this(&world);
        let handle = this.handle;

        let Some(mut endpoint) = world.get_mut::<Endpoint>(this.endpoint) else {
            return;
        };
        endpoint.connection_inserted(handle, entity);
    }
}

/// Based on <https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/connection.rs#L231>
pub(crate) fn poll_connections(
    mut commands: Commands,
    mut query: Query<(Entity, ConnectionQuery, Has<KeepAlive>)>,
    mut endpoint: Query<&mut Endpoint>,
    time: Res<Time<Real>>,
) {
    let now = Instant::now();
    for (entity, connection, keepalive) in &mut query {
        let (established, connection) = connection.get();

        let Some(mut endpoint) = ({
            if connection.is_drained() {
                commands.trigger_targets(ConnectionDrained, entity);
                None
            } else {
                match endpoint.get_mut(connection.endpoint) {
                    Ok(endpoint) => {
                        // Return None if the endpoint was replaced with a new one
                        endpoint
                            .knows_connection(connection.handle)
                            .then_some(endpoint)
                    }
                    Err(
                        QueryEntityError::QueryDoesNotMatch(..) | QueryEntityError::NoSuchEntity(_),
                    ) => {
                        // If the endpoint does not exist anymore, neither should we
                        None
                    }
                    Err(QueryEntityError::AliasedMutability(_)) => unreachable!(),
                }
            }
        }) else {
            commands
                .entity(entity)
                .remove_or_despawn::<(Connecting, Connection)>(keepalive);
            continue;
        };

        connection.handle_timeout(now, time.delta());

        let connection_handle = connection.handle;
        let max_datagrams = endpoint.max_gso_segments();

        let mut transmit_blocked = false;

        // Poll in a loop to eagerly do as much work as we can,
        // instead of polling once per system run, which would mean polling only once per app update

        // TODO: Always poll each update to avoid having to remember to set should_poll in every method
        // let mut should_poll = true;
        // while should_poll {
        // should_poll = false;
        while connection.should_poll {
            connection.should_poll = false;

            // The polling methods must be called in order:
            // (See https://docs.rs/quinn-proto/latest/quinn_proto/struct.Connection.html)

            // #1: poll_transmit
            let mut io_error = connection.io_error;
            while let Some((transmit, data)) = connection.poll_transmit(now, max_datagrams) {
                if io_error {
                    // In case of an I/O error we want to continue polling the connection so it can close and drain gracefully,
                    // but also stop using the socket, so we continue running as normal, except eating any transmits
                    continue;
                }

                match endpoint.send(&transmit, data) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        connection.blocked_transmit = Some(transmit);
                        transmit_blocked = true;
                        break;
                    }
                    Err(error) => {
                        // I/O error
                        if established {
                            commands.trigger_targets(ConnectionError::IoError(error), entity);
                        } else {
                            commands.trigger_targets(ConnectingError::IoError(error), entity);
                        }
                        connection.close(now, 0u32.into(), "I/O Error".into());
                        connection.io_error = true;
                        io_error = true;
                    }
                }
            }

            // #2: poll_timeout
            connection.poll_timeout(now);

            // #3: poll_endpoint_events
            let events = connection
                .poll_endpoint_events()
                .filter_map(|event| endpoint.handle_event(connection_handle, event))
                .collect::<Vec<_>>();

            // #4: poll
            let mut streams_to_flush = Vec::new();
            while let Some(event) = connection.poll() {
                match event {
                    Event::HandshakeDataReady => {
                        commands.trigger_targets(HandshakeDataReady, entity);
                    }
                    Event::Connected => {
                        commands.entity(entity).queue(|mut entity: EntityWorldMut| {
                            if let Some(Connecting(connection)) = entity.take() {
                                entity.insert(Connection(connection));
                            }
                        });
                    }
                    Event::ConnectionLost { reason } => {
                        if established {
                            commands.trigger_targets(ConnectionError::Lost(reason), entity);
                        } else {
                            commands.trigger_targets(ConnectingError::Lost(reason), entity);
                        }
                    }
                    Event::Stream(StreamEvent::Writable { id }) => streams_to_flush.push(id),
                    Event::DatagramsUnblocked => connection.flush_pending_datagrams(),
                    _ => {}
                }
            }

            // Process events after finishing polling instead of immediately
            for event in events {
                connection.handle_event(event);
            }

            for id in streams_to_flush {
                if let Some(pending_writes) = connection.pending_streams.get_mut(&id) {
                    let mut proto_stream = connection.connection.send_stream(id);
                    let _ = proto_stream.write_chunks(pending_writes);
                    pending_writes.retain(|bytes| !bytes.is_empty());
                    connection.should_poll = true;
                }
            }
        }

        // If we need to wait a bit for the socket to become unblocked,
        // queue the connection to be polled again the next time this system runs
        if transmit_blocked {
            connection.should_poll = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::{
        observer::Trigger,
        system::{Query, ResMut},
    };
    use bytes::Bytes;
    use quinn_proto::crypto::rustls::HandshakeData;

    use crate::{tests::*, IncomingResponse, KeepAlive};

    use super::{
        Connecting, ConnectingError, Connection, ConnectionAccepted, ConnectionDrained,
        ConnectionError, ConnectionEstablished, HandshakeDataReady,
    };

    #[test]
    fn keepalive() {
        let mut app = app_no_errors();

        let connections = connection(&mut app);

        app.world_mut()
            .entity_mut(connections.server)
            .insert(KeepAlive);

        // When the endpoint despawns all its associated connections should too
        app.world_mut().despawn(connections.endpoint);

        app.update();

        // Server with keepalive should still exist, but with component removed
        assert!(!app
            .world()
            .entity(connections.server)
            .contains::<Connection>());

        // Client without keepalive should be despawned
        assert!(app
            .world_mut()
            .get_entity(connections.client)
            .is_err_and(|entity| entity == connections.client));
    }

    #[test]
    fn connection_error() {
        let mut app = app_one_error::<ConnectionError>();
        app.init_resource::<HasObserverTriggered>();

        let connections = connection(&mut app);

        let mut server = app
            .world_mut()
            .query::<&mut Connection>()
            .get_mut(app.world_mut(), connections.server)
            .unwrap();

        server.close(0u8.into(), Bytes::new());

        app.world_mut()
            .entity_mut(connections.client)
            .observe(test_observer::<ConnectionError, &Connection>);

        app.update();
        app.update();
    }

    #[test]
    fn connecting_error() {
        let mut app = app_one_error::<ConnectingError>();
        app.init_resource::<HasObserverTriggered>();

        let connections = incoming(&mut app);

        app.world_mut()
            .send_event(IncomingResponse::refuse(connections.server));

        app.world_mut()
            .entity_mut(connections.client)
            .observe(test_observer::<ConnectingError, &Connecting>);

        app.update();
        app.update();
    }

    #[test]
    fn connection_accepted() {
        let mut app = app_no_errors();
        app.init_resource::<HasObserverTriggered>();

        let server = incoming(&mut app).server;

        app.world_mut().send_event(IncomingResponse::accept(server));

        app.world_mut()
            .entity_mut(server)
            .observe(test_observer::<ConnectionAccepted, &Connecting>);

        app.update();
    }

    #[test]
    fn connection_established() {
        let mut app = app_no_errors();
        app.init_resource::<HasObserverTriggered>();

        let server = incoming(&mut app).server;

        app.world_mut().send_event(IncomingResponse::accept(server));

        app.update();
        app.update();

        app.world_mut()
            .entity_mut(server)
            .observe(test_observer::<ConnectionEstablished, &Connection>);

        app.update();
    }

    #[test]
    fn connection_drained() {
        // Exclude ConnectionError because it fires when connection closes
        let mut app = app_one_error::<ConnectionError>();
        app.init_resource::<HasObserverTriggered>();

        let server = connection(&mut app).server;

        let mut connection = app
            .world_mut()
            .query::<&mut Connection>()
            .get_mut(app.world_mut(), server)
            .unwrap();

        connection.close(0u8.into(), Bytes::new());

        app.world_mut()
            .entity_mut(server)
            .observe(test_observer::<ConnectionDrained, &Connection>);

        // Wait for the drain timeout to elapse
        while !app.world().resource::<HasObserverTriggered>().0 {
            app.update();
        }

        // Connections should despawn after draining
        assert!(app
            .world_mut()
            .get_entity(server)
            .is_err_and(|entity| entity == server));
    }

    #[test]
    fn handshake_data_ready() {
        let mut app = app_no_errors();
        app.init_resource::<HasObserverTriggered>();

        let server = incoming(&mut app).server;

        app.world_mut().send_event(IncomingResponse::accept(server));

        app.world_mut().entity_mut(server).observe(
            |trigger: Trigger<HandshakeDataReady>,
             connecting: Query<&Connecting>,
             mut res: ResMut<HasObserverTriggered>| {
                let connecting = connecting.get(trigger.entity()).unwrap();
                let data = connecting.handshake_data().unwrap();
                let data = data.downcast::<HandshakeData>().unwrap();
                assert_eq!(data.server_name, Some("localhost".into()));
                res.0 = true;
            },
        );

        app.update();
    }
}
