use std::{
    future::Future,
    io::{IoSliceMut, Result},
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll},
    time::Instant,
};

use async_io::Async;
use bytes::BytesMut;
use crossbeam_channel::Sender;
use quinn_udp::{RecvMeta, Transmit, UdpSocketState};

#[derive(Debug)]
pub(crate) struct UdpSocket {
    socket: Async<std::net::UdpSocket>,
    state: UdpSocketState,
}

impl UdpSocket {
    pub(crate) fn new(socket: std::net::UdpSocket) -> Result<Self> {
        let state = UdpSocketState::new((&socket).into())?;
        // Creating the `UdpSocketState` configures the socket to be nonblocking already
        let socket = Async::new_nonblocking(socket)?;
        Ok(Self { socket, state })
    }

    pub(crate) fn max_gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    #[expect(dead_code)]
    pub(crate) fn gro_segments(&self) -> usize {
        self.state.gro_segments()
    }

    pub(crate) fn may_fragment(&self) -> bool {
        self.state.may_fragment()
    }

    pub(crate) fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.as_ref().local_addr()
    }

    pub(crate) fn receive(
        &self,
        receive_buffer: &mut [u8],
        mut handle: impl FnMut(RecvMeta, BytesMut) -> Result<()>,
    ) -> Result<()> {
        // Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L714
        let mut metas = [RecvMeta::default(); quinn_udp::BATCH_SIZE];
        let mut iovs: [IoSliceMut; quinn_udp::BATCH_SIZE] = {
            let mut bufs = receive_buffer
                .chunks_mut(receive_buffer.len() / quinn_udp::BATCH_SIZE)
                .map(IoSliceMut::new);

            // This won't panic as `from_fn` calls the closure `BATCH_SIZE` times, which is exactly the length of the `bufs` iterator
            std::array::from_fn(|_| bufs.next().unwrap())
        };

        loop {
            match self
                .state
                .recv(self.socket.as_ref().into(), &mut iovs, &mut metas)
            {
                Ok(msgs) => {
                    for (&meta, buf) in metas.iter().zip(iovs.iter()).take(msgs) {
                        let mut data: BytesMut = buf[0..meta.len].into();
                        while !data.is_empty() {
                            let buf = data.split_to(meta.stride.min(data.len()));
                            handle(meta, buf)?;
                        }
                    }
                }
                Err(e) => match e.kind() {
                    // WouldBlock means there isn't any more data to read from the socket
                    std::io::ErrorKind::WouldBlock => return Ok(()),
                    // Ignore ECONNRESET as it's undefined in QUIC and may be injected by an attacker
                    std::io::ErrorKind::ConnectionReset => continue,
                    _ => return Err(e),
                },
            }
        }
    }

    pub(crate) fn send(&self, transmit: &Transmit) -> Result<()> {
        self.state.send(self.socket.as_ref().into(), transmit)
    }
}

pub(crate) struct DatagramEvent {
    pub(crate) event: quinn_proto::DatagramEvent,
    pub(crate) response_buffer: Vec<u8>,
}

/// Future that drives receiving data from the socket. Uses `async_io` to only wake the task when there is data to be read.
/// The alternative is to poll the socket in an ECS system each frame, but that introduces as much latency to reads as there is
/// time between frames. Using a task to handle reading drastically improves this latency, as the task scheduler will wake the
/// task and poll the future as soon as new data arrives. This in turn improves the transport layer's RTT estimate,
/// which in turn improves the quality of high-level networking features, such as predication and lag compensation.
pub(crate) struct UdpSocketRecvDriver {
    socket: Arc<UdpSocket>,
    receive_buffer: Box<[u8]>,
    endpoint: Arc<Mutex<quinn_proto::Endpoint>>,
    sender: Sender<Result<DatagramEvent>>,
}

impl UdpSocketRecvDriver {
    pub(crate) fn new(
        socket: Arc<UdpSocket>,
        max_udp_payload_size: u64,
        endpoint: Arc<Mutex<quinn_proto::Endpoint>>,
        sender: Sender<Result<DatagramEvent>>,
    ) -> Self {
        // Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L691
        let receive_buffer = vec![
            0;
            max_udp_payload_size.min(64 * 1024) as usize
                * socket.state.gro_segments()
                * quinn_udp::BATCH_SIZE
        ]
        .into_boxed_slice();
        Self {
            socket,
            receive_buffer,
            endpoint,
            sender,
        }
    }
}

impl Future for UdpSocketRecvDriver {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Wake the task when the socket has data to be read
        if let Err(e) = ready!(self.socket.socket.poll_readable(cx)) {
            let _ = self.sender.send(Err(e));
            return Poll::Ready(());
        }

        let now = Instant::now();
        let this = self.get_mut();
        let mut endpoint = this.endpoint.lock().unwrap();

        match this.socket.receive(&mut this.receive_buffer, |meta, data| {
            let mut response_buffer = Vec::new();
            endpoint
                .handle(
                    now,
                    meta.addr,
                    meta.dst_ip,
                    meta.ecn.map(proto_ecn),
                    data,
                    &mut response_buffer,
                )
                .map_or(Ok(()), |event| {
                    this.sender
                        .send(Ok(DatagramEvent {
                            event,
                            response_buffer,
                        }))
                        // Channel disconnected == endpoint dropped, kill the driver
                        .map_err(std::io::Error::other)
                })
        }) {
            Err(e) => {
                // This erroring == channel disconnected, nowhere to send the error
                let _ = this.sender.send(Err(e));
                Poll::Ready(())
            }
            Ok(()) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

#[inline]
fn proto_ecn(ecn: quinn_udp::EcnCodepoint) -> quinn_proto::EcnCodepoint {
    match ecn {
        quinn_udp::EcnCodepoint::Ect0 => quinn_proto::EcnCodepoint::Ect0,
        quinn_udp::EcnCodepoint::Ect1 => quinn_proto::EcnCodepoint::Ect1,
        quinn_udp::EcnCodepoint::Ce => quinn_proto::EcnCodepoint::Ce,
    }
}
