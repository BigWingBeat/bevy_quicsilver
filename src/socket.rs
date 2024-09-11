use std::{
    io::{IoSliceMut, Result},
    net::SocketAddr,
};

use bytes::BytesMut;
use quinn_udp::{RecvMeta, Transmit, UdpSocketState};

#[derive(Debug)]
pub(crate) struct UdpSocket {
    socket: std::net::UdpSocket,
    state: UdpSocketState,
    receive_buffer: Box<[u8]>,
}

impl UdpSocket {
    pub(crate) fn new(socket: std::net::UdpSocket, max_udp_payload_size: u64) -> Result<Self> {
        // Based on https://github.com/quinn-rs/quinn/blob/main/quinn/src/endpoint.rs#L691
        let state = UdpSocketState::new((&socket).into())?;
        let receive_buffer = vec![
            0;
            max_udp_payload_size.min(64 * 1024) as usize
                * state.gro_segments()
                * quinn_udp::BATCH_SIZE
        ]
        .into_boxed_slice();
        Ok(Self {
            socket,
            state,
            receive_buffer,
        })
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
        self.socket.local_addr()
    }

    pub(crate) fn receive(&mut self, mut handle: impl FnMut(RecvMeta, BytesMut)) -> Result<()> {
        // Based on https://github.com/quinn-rs/quinn/blob/0.11.1/quinn/src/endpoint.rs#L714
        let mut metas = [RecvMeta::default(); quinn_udp::BATCH_SIZE];
        let mut iovs: [IoSliceMut; quinn_udp::BATCH_SIZE] = {
            let mut bufs = self
                .receive_buffer
                .chunks_mut(self.receive_buffer.len() / quinn_udp::BATCH_SIZE)
                .map(IoSliceMut::new);

            // This won't panic as `from_fn` calls the closure `BATCH_SIZE` times, which is exactly the length of the `bufs` iterator
            std::array::from_fn(|_| bufs.next().unwrap())
        };

        loop {
            match self
                .state
                .recv((&self.socket).into(), &mut iovs, &mut metas)
            {
                Ok(msgs) => {
                    for (&meta, buf) in metas.iter().zip(iovs.iter()).take(msgs) {
                        let mut data: BytesMut = buf[0..meta.len].into();
                        while !data.is_empty() {
                            let buf = data.split_to(meta.stride.min(data.len()));
                            handle(meta, buf);
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
        self.state.send((&self.socket).into(), transmit)
    }
}
