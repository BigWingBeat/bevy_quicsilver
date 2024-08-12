use bevy_ecs::{
    bundle::Bundle,
    component::{Component, ComponentHooks, StorageType},
    entity::Entity,
    event::EventWriter,
    query::{Has, QueryData, QueryEntityError},
    system::{Commands, Query, Res},
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
    endpoint::{Endpoint, EndpointImpl},
    streams::{RecvStream, SendStream},
    KeepAlive,
};

use std::{
    any::Any,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// An observer trigger that is fired whenever a [`Connecting`] entity encounters an error
#[derive(Debug, Error, bevy_ecs::event::Event)]
pub enum ConnectingError {
    /// The connection was lost
    #[error(transparent)]
    Lost(quinn_proto::ConnectionError),
    /// The connection has been aborted due to an I/O error
    #[error(transparent)]
    IoError(std::io::Error),
}

/// An observer trigger that is fired whenever a [`Connection`] entity encounters an error
#[derive(Debug, Error, bevy_ecs::event::Event)]
pub enum ConnectionError {
    /// The connection was lost
    #[error(transparent)]
    Lost(quinn_proto::ConnectionError),
    /// The connection has been aborted due to an I/O error
    #[error(transparent)]
    IoError(std::io::Error),
}

/// An observer trigger that is fired when a connection is successfully established,
/// and has changed from a [`Connecting`] entity to a [`Connection`] entity
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionEstablished;

/// An observer trigger that is fired when a connection has been fully closed, and is just about to be despawned
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionDrained;

/// A bundle for adding a new connection to an entity
#[derive(Debug, Bundle)]
pub struct ConnectingBundle {
    marker: StillConnecting,
    connection: ConnectionImpl,
}

impl ConnectingBundle {
    pub(crate) fn new(connection: ConnectionImpl) -> Self {
        Self {
            marker: StillConnecting,
            connection,
        }
    }
}

/// Event raised when a [`Connecting`] entity's handshake data becomes available.
/// After this event is raised, [`Connecting::handshake_data()`] will begin returning [`Some`]
#[derive(Debug, bevy_ecs::event::Event)]
pub struct HandshakeDataReady(pub Entity);

/// Marker component type for connection entities that have not yet been fully established
/// (i.e. exposed to the user through [`Connecting`])
#[derive(Debug)]
struct StillConnecting;

impl Component for StillConnecting {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks.on_insert(|mut world, entity, _component_id| {
            // In case an established connection is replaced with a new connection,
            // to prevent the new connection from being queried as though it were fully established
            world.commands().entity(entity).remove::<FullyConnected>();
        });
    }
}

/// An in-progress connection attempt, that has not yet been fully established
#[derive(Debug, QueryData)]
pub struct Connecting {
    marker: &'static StillConnecting,
    connection: &'static ConnectionImpl,
}

impl ConnectingItem<'_> {
    /// Parameters negotiated during the handshake
    ///
    /// Returns `None` until a [`HandshakeDataReady`] event is raised.
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// [`crypto::rustls::HandshakeData`](crate::crypto::rustls::HandshakeData).
    pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.connection.handshake_data()
    }

    /// The peer's UDP address
    ///
    /// If [`ServerConfig::migration()`] is `true`, clients may change addresses at will, e.g. when
    /// switching to a cellular internet connection.
    pub fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// The local IP address which was used when the peer established the connection.
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients, or when the platform does not expose this
    /// information. See [`quinn_udp::RecvMeta::dst_ip`] for a list of supported platforms
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.connection.local_ip()
    }

    /// Returns connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.connection.stats()
    }
}

/// Marker component type for connection entities that are fully established
/// (i.e. exposed to the user through [`Connection`])
#[derive(Debug)]
pub(crate) struct FullyConnected;

impl Component for FullyConnected {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks.on_insert(|mut world, entity, _component_id| {
            world.trigger_targets(ConnectionEstablished, entity);
        });
    }
}

/// A fully established QUIC connection
#[derive(Debug, QueryData)]
#[query_data(mutable)]
pub struct Connection {
    entity: Entity,
    marker: &'static FullyConnected,
    connection: &'static mut ConnectionImpl,
}

impl ConnectionItem<'_> {
    /// Whether the connection is closed.
    ///
    /// Closed connections cannot transport any further data. A connection becomes closed when
    /// either peer application intentionally closes it, or when either transport layer detects an
    /// error such as a time-out or certificate validation failure.
    ///
    /// When the connection becomes closed, an [`EntityError`] event is emitted, and after a brief timeout,
    /// the entity is despawned. If the entity has a [`KeepAlive`] component, only the connection component is removed instead
    pub fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }

    /// Close the connection immediately.
    ///
    /// Pending operations will fail immediately with [`ConnectionError::LocallyClosed`]. Delivery
    /// of data on unfinished streams is not guaranteed, so the application must call this only
    /// when all important communications have been completed, e.g. by calling [`finish`] on
    /// outstanding [`SendStream`]s and waiting for the resulting futures to complete.
    ///
    /// `error_code` and `reason` are not interpreted, and are provided directly to the peer.
    ///
    /// `reason` will be truncated to fit in a single packet with overhead; to improve odds that it
    /// is preserved in full, it should be kept under 1KiB.
    ///
    /// [`ConnectionError::LocallyClosed`]: crate::ConnectionError::LocallyClosed
    /// [`finish`]: crate::SendStream::finish
    /// [`SendStream`]: crate::SendStream
    pub fn close(&mut self, error_code: VarInt, reason: Bytes) {
        self.connection.close(Instant::now(), error_code, reason)
    }

    /// Initiate a new outgoing unidirectional stream.
    /// To reuse the stream later, the [`SendStream::id`] should be stored and passed to [`Self::send_stream`].
    ///
    /// Streams are cheap and instantaneous to open unless blocked by flow control. As a
    /// consequence, the peer won't be notified that a stream has been opened until the stream is
    /// actually used.
    ///
    /// Returns `None` if outgoing unidirectional streams are currently exhausted.
    pub fn open_uni(&mut self) -> Option<SendStream<'_>> {
        self.connection.open_uni()
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
    /// [`open_bi()`]: crate::Connection::open_bi
    /// [`SendStream`]: crate::SendStream
    /// [`RecvStream`]: crate::RecvStream
    pub fn open_bi(&mut self) -> Option<StreamId> {
        self.connection.open_bi()
    }

    /// Accept the next incoming unidirectional stream.
    /// To reuse the stream later, the [`SendStream::id`] should be stored and passed to [`Self::recv_stream`].
    ///
    /// Returns `None` if there are no new incoming unidirectional streams for this connection.
    /// Has no impact on the data flow-control or stream concurrency limits.
    pub fn accept_uni(&mut self) -> Option<RecvStream<'_>> {
        self.connection.accept_uni()
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
    /// [`open_bi()`]: crate::Connection::open_bi
    /// [`SendStream`]: crate::SendStream
    /// [`RecvStream`]: crate::RecvStream
    pub fn accept_bi(&mut self) -> Option<StreamId> {
        self.connection.accept_bi()
    }

    /// Get the send stream associated with the given stream ID.
    ///
    /// # Panics
    /// Panics if the given stream ID is not associated with a send stream.
    pub fn send_stream(&mut self, id: StreamId) -> Result<SendStream<'_>, ClosedStream> {
        self.connection.send_stream(id)
    }

    /// Get the receive stream associated with the given stream ID.
    ///
    /// # Panics
    /// Panics if the given stream ID is not associated with a receive stream.
    pub fn recv_stream(&mut self, id: StreamId) -> Result<RecvStream<'_>, ClosedStream> {
        self.connection.recv_stream(id)
    }

    /// Transmit `data` as an unreliable, unordered application datagram
    ///
    /// Application datagrams are a low-level primitive. They may be lost or delivered out of order,
    /// and `data` must both fit inside a single QUIC packet and be smaller than the maximum
    /// dictated by the peer.
    ///
    /// Previously queued datagrams which are still unsent may be discarded to make space for this datagram,
    /// in order of oldest to newest.
    pub fn send_datagram(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.connection.send_datagram(data)
    }

    /// Transmit `data` as an unreliable, unordered application datagram
    ///
    /// Unlike [`send_datagram()`], this method will wait for buffer space during congestion
    /// conditions, which effectively prioritizes old datagrams over new datagrams.
    ///
    /// See [`send_datagram()`] for details.
    ///
    /// [`send_datagram()`]: Connection::send_datagram
    pub fn send_datagram_wait(&mut self, data: Bytes) -> Result<(), SendDatagramError> {
        self.connection.send_datagram_wait(data)
    }

    /// Receive an unreliable, unordered application datagram
    pub fn read_datagram(&mut self) -> Option<Bytes> {
        self.connection.read_datagram()
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
        self.connection.max_datagram_size()
    }

    /// Bytes available in the outgoing datagram buffer
    ///
    /// When greater than zero, sending a datagram of at most this size is guaranteed not to cause older datagrams to be dropped.
    pub fn datagram_send_buffer_space(&mut self) -> usize {
        self.connection.datagram_send_buffer_space()
    }

    /// The peer's UDP address
    ///
    /// If [`ServerConfig::migration()`] is `true`, clients may change addresses at will, e.g. when
    /// switching to a cellular internet connection.
    pub fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// The local IP address which was used when the peer established the connection
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients.
    ///
    /// Retrieving the local IP address is currently supported on the following
    /// platforms:
    /// - Linux
    /// - ???, see https://github.com/quinn-rs/quinn/issues/1864
    ///
    /// On all non-supported platforms the local IP address will not be available,
    /// and the method will return `None`.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.connection.local_ip()
    }

    /// Current best estimate of this connection's latency (round-trip time)
    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    /// Returns connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.connection.stats()
    }

    /// Current state of this connection's congestion control algorithm, for debugging purposes
    pub fn congestion_state(&self) -> &dyn Controller {
        self.connection.congestion_state()
    }

    /// Parameters negotiated during the handshake
    ///
    /// Guranteed to return `Some` on fully established connections.
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// [`crypto::rustls::HandshakeData`](crate::crypto::rustls::HandshakeData).
    pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.connection.handshake_data()
    }

    /// Cryptographic identity of the peer
    ///
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// <code>Vec<[rustls::pki_types::CertificateDer]></code>
    pub fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.connection.peer_identity()
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
        self.connection
            .export_keying_material(output, label, context)
    }

    /// Modify the number of remotely initiated unidirectional streams that may be concurrently open
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_uni_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_uni_streams(count)
    }

    /// Modify the number of remotely initiated bidirectional streams that may be concurrently open
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_bi_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_bi_streams(count)
    }
}

impl ConnectionReadOnlyItem<'_> {
    /// Whether the connection is closed.
    ///
    /// Closed connections cannot transport any further data. A connection becomes closed when
    /// either peer application intentionally closes it, or when either transport layer detects an
    /// error such as a time-out or certificate validation failure.
    ///
    /// When the connection becomes closed, an [`EntityError`] event is emitted, and after a brief timeout,
    /// the entity is despawned. If the entity has a [`KeepAlive`] component, only the connection component is removed instead
    pub fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }

    /// The peer's UDP address
    ///
    /// If [`ServerConfig::migration()`] is `true`, clients may change addresses at will, e.g. when
    /// switching to a cellular internet connection.
    pub fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// The local IP address which was used when the peer established the connection
    ///
    /// This can be different from the address the endpoint is bound to, in case
    /// the endpoint is bound to a wildcard address like `0.0.0.0` or `::`.
    ///
    /// This will return `None` for clients.
    ///
    /// Retrieving the local IP address is currently supported on the following
    /// platforms:
    /// - Linux
    /// - ???, see https://github.com/quinn-rs/quinn/issues/1864
    ///
    /// On all non-supported platforms the local IP address will not be available,
    /// and the method will return `None`.
    pub fn local_ip(&self) -> Option<IpAddr> {
        self.connection.local_ip()
    }

    /// Current best estimate of this connection's latency (round-trip time)
    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    /// Returns connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.connection.stats()
    }

    /// Current state of this connection's congestion control algorithm, for debugging purposes
    pub fn congestion_state(&self) -> &dyn Controller {
        self.connection.congestion_state()
    }

    /// Parameters negotiated during the handshake
    ///
    /// Guranteed to return `Some` on fully established connections.
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// [`crypto::rustls::HandshakeData`](crate::crypto::rustls::HandshakeData).
    pub fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.connection.handshake_data()
    }

    /// Cryptographic identity of the peer
    ///
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// <code>Vec<[rustls::pki_types::CertificateDer]></code>
    pub fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.connection.peer_identity()
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
        self.connection
            .export_keying_material(output, label, context)
    }
}

/// Underlying component type behind the [`Connecting`] and [`Connection`] querydata types
#[derive(Debug)]
pub(crate) struct ConnectionImpl {
    pub(crate) endpoint: Entity,
    pub(crate) handle: ConnectionHandle,
    connection: quinn_proto::Connection,
    timeout_timer: Option<(Timer, Instant)>,
    pub(crate) should_poll: bool,
    io_error: bool,
    blocked_transmit: Option<Transmit>,
    transmit_buf: Vec<u8>,
    pending_datagrams: Vec<Bytes>,
    pending_streams: HashMap<StreamId, Vec<Bytes>>,
}

impl Component for ConnectionImpl {
    const STORAGE_TYPE: StorageType = StorageType::Table;

    fn register_component_hooks(hooks: &mut ComponentHooks) {
        hooks
            .on_insert(|mut world, entity, _component_id| {
                let connection = world.get::<Self>(entity).unwrap();
                let handle = connection.handle;
                let Some(mut endpoint) = world.get_mut::<EndpointImpl>(connection.endpoint) else {
                    return;
                };
                endpoint.connections.insert(handle, entity);
            })
            .on_remove(|mut world, entity, _component_id| {
                world.trigger_targets(ConnectionDrained, entity);
            });
    }
}

impl ConnectionImpl {
    pub(crate) fn new(
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
            SendStream {
                id,
                write_buffer,
                proto_stream: self.connection.send_stream(id),
            }
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
            .map(|id| RecvStream {
                id,
                proto_stream: self.connection.recv_stream(id),
            })
    }

    fn accept_bi(&mut self) -> Option<StreamId> {
        self.should_poll = true;
        self.connection.streams().accept(Dir::Bi).inspect(|&id| {
            self.pending_streams.insert(id, Vec::new());
        })
    }

    pub(crate) fn send_stream(&mut self, id: StreamId) -> Result<SendStream<'_>, ClosedStream> {
        self.should_poll = true;
        self.pending_streams
            .get_mut(&id)
            .ok_or(ClosedStream::new())
            .map(|write_buffer| SendStream {
                id,
                write_buffer,
                proto_stream: self.connection.send_stream(id),
            })
    }

    pub(crate) fn recv_stream(&mut self, id: StreamId) -> Result<RecvStream<'_>, ClosedStream> {
        self.should_poll = true;
        Ok(RecvStream {
            id,
            proto_stream: self.connection.recv_stream(id),
        })
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
    }

    pub(crate) fn handle_event(&mut self, event: quinn_proto::ConnectionEvent) {
        self.connection.handle_event(event);
        self.should_poll = true;
    }

    fn handle_timeout(&mut self, now: Instant, delta: Duration) {
        if let Some((ref mut timer, _)) = self.timeout_timer {
            if timer.tick(delta).just_finished() {
                self.connection.handle_timeout(now);
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
                    .map(|&(_, previous_timeout)| previous_timeout != timeout)
                    .unwrap_or(true)
                {
                    self.timeout_timer = Some((
                        Timer::new(
                            timeout
                                .checked_duration_since(now)
                                .expect("Monotonicity violated"),
                            TimerMode::Once,
                        ),
                        timeout,
                    ))
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
}

/// Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/connection.rs#L231
pub(crate) fn poll_connections(
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &mut ConnectionImpl,
        Has<FullyConnected>,
        Has<KeepAlive>,
    )>,
    mut endpoint: Query<Endpoint>,
    mut handshake_events: EventWriter<HandshakeDataReady>,
    time: Res<Time<Real>>,
) {
    let now = Instant::now();
    for (entity, mut connection, established, keepalive) in query.iter_mut() {
        let mut endpoint = match endpoint.get_mut(connection.endpoint) {
            Ok(endpoint) => endpoint,
            Err(QueryEntityError::QueryDoesNotMatch(_))
            | Err(QueryEntityError::NoSuchEntity(_)) => {
                // If the endpoint does not exist anymore, neither should we
                if keepalive {
                    commands
                        .entity(entity)
                        .remove::<(ConnectionImpl, StillConnecting, FullyConnected)>();
                } else {
                    commands.entity(entity).despawn();
                }
                continue;
            }
            Err(QueryEntityError::AliasedMutability(_)) => unreachable!(),
        };

        // If drained or the endpoint was replaced with a new one
        if connection.is_drained()
            || !endpoint
                .endpoint
                .connections
                .contains_key(&connection.handle)
        {
            if keepalive {
                commands
                    .entity(entity)
                    .remove::<(ConnectionImpl, StillConnecting, FullyConnected)>();
            } else {
                commands.entity(entity).despawn();
            }
        }

        connection.handle_timeout(now, time.delta());

        let connection_handle = connection.handle;
        let max_datagrams = endpoint.max_gso_segments();

        let mut transmit_blocked = false;

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
                        handshake_events.send(HandshakeDataReady(entity));
                    }
                    Event::Connected => {
                        commands
                            .entity(entity)
                            .remove::<StillConnecting>()
                            .insert(FullyConnected);
                    }
                    Event::ConnectionLost { reason } => {
                        if established {
                            commands.trigger_targets(ConnectionError::Lost(reason), entity);
                        } else {
                            commands.trigger_targets(ConnectingError::Lost(reason), entity);
                        }
                    }
                    Event::Stream(StreamEvent::Opened { .. }) => {}
                    Event::Stream(StreamEvent::Readable { .. }) => {}
                    Event::Stream(StreamEvent::Writable { id }) => streams_to_flush.push(id),
                    Event::Stream(StreamEvent::Finished { .. }) => {}
                    Event::Stream(StreamEvent::Stopped { .. }) => {}
                    Event::Stream(StreamEvent::Available { .. }) => {}
                    Event::DatagramReceived => {}
                    Event::DatagramsUnblocked => connection.flush_pending_datagrams(),
                }
            }

            // Process events after finishing polling instead of immediately
            for event in events {
                connection.handle_event(event);
            }

            for id in streams_to_flush {
                let ConnectionImpl {
                    connection,
                    should_poll,
                    pending_streams,
                    ..
                } = &mut *connection;

                if let Some(pending_writes) = pending_streams.get_mut(&id) {
                    let mut proto_stream = connection.send_stream(id);
                    let _ = proto_stream.write_chunks(pending_writes);
                    pending_writes.retain(|bytes| !bytes.is_empty());
                    *should_poll = true;
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
