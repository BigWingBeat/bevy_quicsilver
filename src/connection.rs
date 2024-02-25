use bevy_ecs::{
    component::Component,
    entity::EntityHash,
    event::{EventReader, EventWriter},
    system::{Query, Res},
};
use bevy_time::{Real, Time, Timer, TimerMode};
use bytes::Bytes;
use hashbrown::HashMap;
use quinn_proto::{ConnectionHandle, Dir, Event, StreamEvent, StreamId};

use crate::endpoint::{ConnectionEvent, Endpoint};

use std::{
    iter,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct EndpointEvent {
    connection: ConnectionHandle,
    event: quinn_proto::EndpointEvent,
}

#[derive(Debug, Component)]
pub struct Connection {
    handle: ConnectionHandle,
    connection: quinn_proto::Connection,
    ordered_stream: StreamId,
    timeout_timer: Option<(Timer, Instant)>,
    should_poll: bool,
    pending_streams: Vec<Bytes>,
    pending_writes: HashMap<StreamId, Vec<Bytes>, EntityHash>,
}

impl Connection {
    pub fn new(handle: ConnectionHandle, connection: Connection, ordered_stream: StreamId) -> Self {
        Self {
            handle,
            connection,
            ordered_stream,
            timeout_timer: None,
            should_poll: false,
            pending_streams: Vec::new(),
            pending_writes: HashMap::new(),
        }
    }

    /// Serialize the given packet and send it over the network
    pub fn send<T: OutboundPacket>(&mut self, packet: T) -> Result<(), Error> {
        let bytes = serialize(packet)?;
        match T::CHANNEL {
            Channel::Ordered => {
                self.pending_writes
                    .entry(&self.ordered_stream)
                    .or_default()
                    .push(bytes);

                let result = self
                    .connection
                    .send_stream(self.ordered_stream)
                    .write_chunks(bytes);

                if result.is_err_and(|e| !matches!(e, WriteError::Blocked)) {
                    return result;
                }

                if !bytes.is_empty() {
                    self.pending_stream_writeable.push(bytes);
                }
            }
            Channel::Unordered => match self.connection.streams().open(Dir::Uni) {
                Some(stream_id) => {
                    self.pending_writes
                        .entry(&stream_id)
                        .or_default()
                        .push(bytes);

                    let stream = self.connection.send_stream(stream_id);
                    stream.write_chunks(bytes)?;

                    if bytes.is_empty() {
                        stream.finish()?;
                    } else {
                        self.pending_unordered_packets
                            .push((Some(stream_id), bytes));
                    }
                }
                None => self.pending_writes.entry(None).or_default().push(bytes),
            },
            Channel::Unreliable => self.connection.datagrams().send(bytes)?,
        }
        Ok(())
    }

    fn flush_pending_writes(&mut self) {
        let streams = self.connection.streams();

        for (stream_id, bytes) in iter::zip(
            iter::from_fn(|| streams.open(Dir::Uni)),
            iter::from_fn(|| self.pending_streams.pop()),
        ) {
            self.pending_writes
                .entry(&stream_id)
                .or_default()
                .push(bytes);
        }

        for (stream_id, bytes) in self.pending_writes.iter_mut() {
            let stream = self.connection.send_stream(stream_id);
            stream.write_chunks(&mut bytes);
        }
    }

    fn handle_event(&mut self, event: quinn_proto::EndpointEvent) {
        self.connection.handle_event(event);
        self.should_poll = true;
    }

    fn handle_timeout(&mut self, now: Instant, delta: Duration) {
        if let Some((mut timer, _)) = self.timeout_timer {
            if timer.tick(delta).just_finished() {
                self.connection.handle_timeout(now);
                self.should_poll = true;
            }
        }
    }

    fn poll_transmit(&mut self, now: Instant, max_datagrams: usize) {
        while let Some(transmit) = self.connection.poll_transmit(now, max_datagrams) {
            todo!()
        }
    }

    fn poll_timeout(&mut self, now: Instant) {
        match self.connection.poll_timeout() {
            Some(timeout) => {
                if self
                    .timeout_timer
                    .map(|(_, previous_timeout)| previous_timeout != timeout)
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

    fn poll_endpoint_events(&mut self, mut event_fn: impl Fn(EndpointEvent)) {
        while let Some(event) = self.connection.poll_endpoint_events() {
            event_fn(EndpointEvent {
                connection: self.handle,
                event,
            });
        }
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
            }
        }
    }
}

fn connection_system(
    mut query: Query<&mut Connection>,
    mut endpoint: Res<Endpoint>,
    time: Res<Time<Real>>,
    mut incoming_events: EventReader<ConnectionEvent>,
    mut outgoing_events: EventWriter<EndpointEvent>,
) {
    let now = Instant::now();
    let events = incoming_events.read().collect();
    for mut connection in query.iter_mut() {
        for event in events {
            if event.connection == connection.handle {
                connection.handle_event(event.event);
            }
        }

        connection.handle_timeout(now, time.delta());

        if connection.should_poll {
            connection.should_poll = false;
            connection.poll_transmit(now, endpoint.udp_state.max_gso_segments());
            connection.poll_timeout(now);
            connection.poll_endpoint_events(|event| outgoing_events.send(event));
            connection.poll();
        }
    }
}
