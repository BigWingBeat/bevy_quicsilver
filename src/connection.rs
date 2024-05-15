use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    system::{Query, Res},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::Bytes;
use hashbrown::HashMap;
use quinn_proto::{
    congestion::Controller, ConnectionHandle, ConnectionStats, Dir, EndpointEvent, Event,
    SendDatagramError, StreamEvent, StreamId, VarInt,
};

use crate::{endpoint::Endpoint, Error};

use std::{
    any::Any,
    iter,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// A QUIC connection
#[derive(Debug, Component)]
pub struct Connection {
    pub(crate) endpoint: Entity,
    pub(crate) handle: ConnectionHandle,
    connection: quinn_proto::Connection,
    timeout_timer: Option<(Timer, Instant)>,
    should_poll: bool,
    transmit_buf: Vec<u8>,
    pending_streams: Vec<Bytes>,
    pending_writes: HashMap<StreamId, Vec<Bytes>, EntityHash>,
    pending_datagrams: Vec<Bytes>,
}

impl Connection {
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
            should_poll: false,
            transmit_buf: Vec::new(),
            pending_streams: Vec::new(),
            pending_writes: HashMap::default(),
            pending_datagrams: Vec::new(),
        }
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
        self.connection
            .datagrams()
            .send(data, true)
            .map_err(Into::into)
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
        self.connection
            .datagrams()
            .send(data, false)
            .or_else(|e| match e {
                SendDatagramError::Blocked(data) => {
                    self.pending_datagrams.push(data);
                    Ok(())
                }
                _ => Err(e.into()),
            })
    }

    /// Receive an unreliable, unordered application datagram
    pub fn read_datagram(&mut self) -> Option<Bytes> {
        self.connection.datagrams().recv()
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
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection.datagrams().max_size()
    }

    /// Bytes available in the outgoing datagram buffer
    ///
    /// When greater than zero, sending a datagram of at most this size is guaranteed not to cause older datagrams to be dropped.
    pub fn datagram_send_buffer_space(&self) -> usize {
        self.connection.datagrams().send_buffer_space()
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
        self.connection.crypto_session().handshake_data()
    }

    /// Cryptographic identity of the peer
    ///
    /// The dynamic type returned is determined by the configured [`Session`].
    /// For the default `rustls` session, it can be [`downcast`](Box::downcast) to a
    /// <code>Vec<[rustls::pki_types::CertificateDer]></code>
    pub fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.connection.crypto_session().peer_identity()
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
            .crypto_session()
            .export_keying_material(output, label, context)
    }

    /// Modify the number of remotely initiated unidirectional streams that may be concurrently open
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_uni_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_streams(Dir::Uni, count)
    }

    /// Modify the number of remotely initiated bidirectional streams that may be concurrently open
    ///
    /// No streams may be opened by the peer unless fewer than `count` are already open.
    /// Large `count`s increase both minimum and worst-case memory consumption.
    pub fn set_max_concurrent_bi_streams(&mut self, count: VarInt) {
        self.connection.set_max_concurrent_streams(Dir::Bi, count)
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

    fn poll_transmit(&mut self, now: Instant, max_datagrams: usize) {
        while let Some(transmit) =
            self.connection
                .poll_transmit(now, max_datagrams, &mut self.transmit_buf)
        {
            todo!()
        }
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

    fn poll(&mut self) {
        while let Some(poll) = self.connection.poll() {
            match poll {
                Event::HandshakeDataReady => {}
                Event::Connected => todo!(),
                Event::ConnectionLost { reason } => todo!(),
                Event::Stream(StreamEvent::Opened { dir }) => todo!(),
                Event::Stream(StreamEvent::Readable { id }) => todo!(),
                Event::Stream(StreamEvent::Writable { id }) => todo!(),
                Event::Stream(StreamEvent::Finished { id }) => {}
                Event::Stream(StreamEvent::Stopped { id, error_code }) => todo!(),
                Event::Stream(StreamEvent::Available { dir }) => todo!(),
                Event::DatagramReceived => todo!(),
                Event::DatagramsUnblocked => todo!(),
            }
        }
    }
}

/// Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/connection.rs#L231
pub(crate) fn poll_connections(
    mut query: Query<&mut Connection>,
    mut endpoint: Query<Endpoint>,
    time: Res<Time<Real>>,
) {
    let now = Instant::now();
    for mut connection in query.iter_mut() {
        let mut endpoint = endpoint
            .get_mut(connection.endpoint)
            .expect("Endpoint entity was despawned");

        connection.handle_timeout(now, time.delta());

        let connection_handle = connection.handle;
        let max_gso_segments = endpoint.max_gso_segments();

        while connection.should_poll {
            connection.should_poll = false;

            connection.poll_transmit(now, max_gso_segments);
            connection.poll_timeout(now);
            let events = connection
                .poll_endpoint_events()
                .filter_map(|event| endpoint.handle_event(connection_handle, event))
                .collect::<Vec<_>>();
            connection.poll();

            // Process events after finishing polling instead of immediately
            for event in events {
                connection.handle_event(event);
            }
        }
    }
}
