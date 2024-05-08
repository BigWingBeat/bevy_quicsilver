use bevy_ecs::{
    component::Component,
    entity::{Entity, EntityHash},
    event::{EventReader, EventWriter},
    system::{Query, Res},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::{Bytes, BytesMut};
use hashbrown::HashMap;
use quinn_proto::{
    crypto::Session, ConnectionHandle, ConnectionStats, Dir, Event, StreamEvent, StreamId,
};

use crate::endpoint::Endpoint;

use std::{
    iter,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Debug, bevy_ecs::event::Event)]
pub(crate) struct EndpointEvent {
    endpoint: Entity,
    connection: ConnectionHandle,
    event: quinn_proto::EndpointEvent,
}

#[derive(Debug, Component)]
pub struct Connection {
    endpoint: Entity,
    handle: ConnectionHandle,
    // TODO: Remove mutex when sync fix is released
    connection: Mutex<quinn_proto::Connection>,
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
            connection: Mutex::new(connection),
            timeout_timer: None,
            should_poll: false,
            transmit_buf: Vec::new(),
            pending_streams: Vec::new(),
            pending_writes: HashMap::default(),
        }
    }

    /// Returns connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.connection.lock().unwrap().stats()
    }

    /// Ping the remote endpoint
    ///
    /// Causes an ACK-eliciting packet to be transmitted.
    pub fn ping(&mut self) {
        self.connection.get_mut().unwrap().ping()
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
        let mut connection = self.connection.get_mut().unwrap();
        let mut streams = connection.streams();

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
            let mut stream = connection.send_stream(stream_id);
            stream.write_chunks(&mut bytes);
        }
    }

    pub(crate) fn handle_event(&mut self, event: quinn_proto::ConnectionEvent) {
        self.connection.get_mut().unwrap().handle_event(event);
        self.should_poll = true;
    }

    fn handle_timeout(&mut self, now: Instant, delta: Duration) {
        if let Some((ref mut timer, _)) = self.timeout_timer {
            if timer.tick(delta).just_finished() {
                self.connection.get_mut().unwrap().handle_timeout(now);
                self.should_poll = true;
            }
        }
    }

    fn poll_transmit(&mut self, now: Instant, max_datagrams: usize) {
        let mut connection = self.connection.get_mut().unwrap();
        while let Some(transmit) =
            connection.poll_transmit(now, max_datagrams, &mut self.transmit_buf)
        {
            todo!()
        }
    }

    fn poll_timeout(&mut self, now: Instant) {
        match self.connection.get_mut().unwrap().poll_timeout() {
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

    fn poll_endpoint_events(&mut self, mut event_fn: impl FnMut(EndpointEvent)) {
        let mut connection = self.connection.get_mut().unwrap();
        while let Some(event) = connection.poll_endpoint_events() {
            event_fn(EndpointEvent {
                endpoint: self.endpoint,
                connection: self.handle,
                event,
            });
        }
    }

    fn poll(&mut self) {
        let mut connection = self.connection.get_mut().unwrap();
        while let Some(poll) = connection.poll() {
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

fn connection_system(
    mut query: Query<&mut Connection>,
    endpoint: Query<&Endpoint>,
    time: Res<Time<Real>>,
    mut outgoing_events: EventWriter<EndpointEvent>,
) {
    let now = Instant::now();
    for mut connection in query.iter_mut() {
        connection.handle_timeout(now, time.delta());

        let max_gso_segments = endpoint
            .get(connection.endpoint)
            .expect("Endpoint entity was despawned")
            .max_gso_segments();

        if connection.should_poll {
            connection.should_poll = false;
            connection.poll_transmit(now, max_gso_segments);
            connection.poll_timeout(now);
            connection.poll_endpoint_events(|event| {
                outgoing_events.send(event);
            });
            connection.poll();
        }
    }
}
