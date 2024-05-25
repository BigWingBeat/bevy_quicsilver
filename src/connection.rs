use bevy_ecs::{
    bundle::Bundle,
    component::Component,
    entity::Entity,
    event::EventWriter,
    query::{Added, Has, QueryData, QueryEntityError},
    system::{Commands, Query, Res},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::Bytes;
use hashbrown::HashMap;
use quinn_proto::{
    congestion::Controller, ConnectionHandle, ConnectionStats, Dir, EndpointEvent, Event,
    SendDatagramError, StreamEvent, StreamId, Transmit, VarInt,
};

use crate::{endpoint::Endpoint, EntityError, Error, KeepAlive};

use std::{
    any::Any,
    iter,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

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

/// Event raised whenever a [`Connecting`] entity finishes establishing the connection, turning into a [`Connection`] entity
#[derive(Debug, bevy_ecs::event::Event)]
pub struct ConnectionEstablished(pub Entity);

/// Event raised when a [`Connecting`] entity's handshake data becomes available.
/// After this event is raised, [`Connecting::handshake_data()`] will begin returning [`Some`]
#[derive(Debug, bevy_ecs::event::Event)]
pub struct HandshakeDataReady(pub Entity);

/// Marker component type for connection entities that have not yet been fully established
/// (i.e. exposed to the user through [`Connecting`])
#[derive(Debug, Component)]
struct StillConnecting;

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
#[derive(Debug, Component)]
pub(crate) struct FullyConnected;

/// A QUIC connection
#[derive(Debug, QueryData)]
#[query_data(mutable)]
pub struct Connection {
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

    /// Transmit `data` as an unreliable, unordered application datagram
    ///
    /// Application datagrams are a low-level primitive. They may be lost or delivered out of order,
    /// and `data` must both fit inside a single QUIC packet and be smaller than the maximum
    /// dictated by the peer.
    ///
    /// Previously queued datagrams which are still unsent may be discarded to make space for this datagram,
    /// in order of oldest to newest.
    pub fn send_datagram(&mut self, data: Bytes) -> Result<(), Error> {
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
    pub fn send_datagram_wait(&mut self, data: Bytes) -> Result<(), Error> {
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
    ) -> Result<(), quinn_proto::crypto::ExportKeyingMaterialError> {
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
    ) -> Result<(), quinn_proto::crypto::ExportKeyingMaterialError> {
        self.connection
            .export_keying_material(output, label, context)
    }
}

/// Underlying component type behind the [`Connecting`] and [`Connection`] querydata types
#[derive(Debug, Component)]
pub(crate) struct ConnectionImpl {
    pub(crate) endpoint: Entity,
    pub(crate) handle: ConnectionHandle,
    connection: quinn_proto::Connection,
    timeout_timer: Option<(Timer, Instant)>,
    should_poll: bool,
    io_error: bool,
    blocked_transmit: Option<Transmit>,
    transmit_buf: Vec<u8>,
    pending_streams: Vec<Bytes>,
    pending_writes: HashMap<StreamId, Vec<Bytes>>,
    pending_datagrams: Vec<Bytes>,
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
            pending_streams: Vec::new(),
            pending_writes: HashMap::default(),
            pending_datagrams: Vec::new(),
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

    fn send_datagram(&mut self, data: Bytes) -> Result<(), Error> {
        self.should_poll = true;
        self.connection
            .datagrams()
            .send(data, true)
            .map_err(Into::into)
    }

    fn send_datagram_wait(&mut self, data: Bytes) -> Result<(), Error> {
        self.should_poll = true;
        self.connection
            .datagrams()
            .send(data, false)
            .or_else(|error| match error {
                SendDatagramError::Blocked(data) => {
                    self.pending_datagrams.push(data);
                    Ok(())
                }
                _ => Err(error.into()),
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
    ) -> Result<(), quinn_proto::crypto::ExportKeyingMaterialError> {
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

    /// Serialize the given packet and send it over the network
    // pub fn send<T: OutboundPacket>(&mut self, packet: T) -> Result<(), Error> {
    //     let bytes = serialize(packet)?;
    //     match T::CHANNEL {
    //         Channel::Ordered => {
    //             self.pending_writes
    //                 .entry(&self.ordered_stream)
    //                 .or_default()
    //                 .push(bytes);

    //             let result = self
    //                 .connection
    //                 .send_stream(self.ordered_stream)
    //                 .write_chunks(bytes);

    //             if result.is_err_and(|e| !matches!(e, WriteError::Blocked)) {
    //                 return result;
    //             }

    //             if !bytes.is_empty() {
    //                 self.pending_stream_writeable.push(bytes);
    //             }
    //         }
    //         Channel::Unordered => match self.connection.streams().open(Dir::Uni) {
    //             Some(stream_id) => {
    //                 self.pending_writes
    //                     .entry(&stream_id)
    //                     .or_default()
    //                     .push(bytes);

    //                 let stream = self.connection.send_stream(stream_id);
    //                 stream.write_chunks(bytes)?;

    //                 if bytes.is_empty() {
    //                     stream.finish()?;
    //                 } else {
    //                     self.pending_unordered_packets
    //                         .push((Some(stream_id), bytes));
    //                 }
    //             }
    //             None => self.pending_writes.entry(None).or_default().push(bytes),
    //         },
    //         Channel::Unreliable => self.connection.datagrams().send(bytes)?,
    //     }
    //     Ok(())
    // }

    fn flush_pending_datagrams(&mut self) {
        self.pending_datagrams.retain(|datagram| {
            matches!(
                self.connection.datagrams().send(datagram.clone(), false),
                Err(SendDatagramError::Blocked(_))
            )
        });
    }

    fn flush_pending_writes(&mut self) {
        let mut streams = self.connection.streams();

        for (stream_id, bytes) in iter::zip(
            iter::from_fn(|| streams.open(Dir::Uni)),
            iter::from_fn(|| self.pending_streams.pop()),
        ) {
            self.pending_writes
                .entry(stream_id)
                .or_default()
                .push(bytes);
        }

        for (&stream_id, mut bytes) in self.pending_writes.iter_mut() {
            let mut stream = self.connection.send_stream(stream_id);
            stream.write_chunks(&mut bytes);
        }
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

/// Events are sent in system using [`Added`] instead of where the component is actually inserted,
/// because new events are visible immediately, but inserting components is deferred until `Commands` are applied
pub(crate) fn send_connection_established_events(
    connections: Query<Entity, Added<FullyConnected>>,
    mut events: EventWriter<ConnectionEstablished>,
) {
    for entity in connections.iter() {
        events.send(ConnectionEstablished(entity));
    }
}

/// Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/connection.rs#L231
pub(crate) fn poll_connections(
    mut commands: Commands,
    mut query: Query<(Entity, &mut ConnectionImpl, Has<KeepAlive>)>,
    mut endpoint: Query<Endpoint>,
    mut handshake_events: EventWriter<HandshakeDataReady>,
    mut error_events: EventWriter<EntityError>,
    time: Res<Time<Real>>,
) {
    let now = Instant::now();
    for (entity, mut connection, keepalive) in query.iter_mut() {
        if connection.is_drained() {
            if keepalive {
                commands
                    .entity(entity)
                    .remove::<(ConnectionImpl, StillConnecting, FullyConnected)>();
            } else {
                commands.entity(entity).despawn();
            }
        }

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
                        error_events.send(EntityError::new(entity, error));
                        connection.close(now, 0u32.into(), "I/O error".into());
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
                        error_events.send(EntityError::new(entity, reason));
                    }
                    Event::Stream(StreamEvent::Opened { dir }) => todo!(),
                    Event::Stream(StreamEvent::Readable { id }) => todo!(),
                    Event::Stream(StreamEvent::Writable { id }) => todo!(),
                    Event::Stream(StreamEvent::Finished { id }) => {}
                    Event::Stream(StreamEvent::Stopped { id, error_code }) => todo!(),
                    Event::Stream(StreamEvent::Available { dir }) => todo!(),
                    Event::DatagramReceived => {}
                    Event::DatagramsUnblocked => connection.flush_pending_datagrams(),
                }
            }

            // Process events after finishing polling instead of immediately
            for event in events {
                connection.handle_event(event);
            }
        }

        // If we need to wait a bit for the socket to become unblocked,
        // queue the connection to be polled again the next time this system runs
        if transmit_blocked {
            connection.should_poll = true;
        }
    }
}
