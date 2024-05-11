use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    system::{Query, Res},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::Bytes;
use hashbrown::HashMap;
use quinn_proto::{
    ConnectionHandle, ConnectionStats, Dir, EndpointEvent, Event, StreamEvent, StreamId,
};

use crate::endpoint::Endpoint;

use std::{
    iter,
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
        }
    }

    /// Returns connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.connection.stats()
    }

    /// Ping the remote endpoint
    ///
    /// Causes an ACK-eliciting packet to be transmitted.
    pub fn ping(&mut self) {
        self.connection.ping()
    }

    // /// Get a session reference
    // pub fn crypto_session(&self) -> &dyn Session {
    //     self.connection.lock().unwrap().crypto_session()
    // }

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
                if self
                    .timeout_timer
                    .as_ref()
                    .map(|&(_, previous_timeout)| previous_timeout != timeout)
                    .unwrap_or(true)
                {
                    self.timeout_timer = Some((
                        Timer::new(
                            timeout.checked_duration_since(now).unwrap(),
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
        let connection_handle = connection.handle;

        connection.handle_timeout(now, time.delta());

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
