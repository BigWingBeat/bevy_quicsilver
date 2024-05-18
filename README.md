# bevy_quicsilver

[![crate](https://img.shields.io/crates/v/bevy_quicsilver.svg)](https://crates.io/crates/bevy_quicsilver)
[![documentation](https://docs.rs/bevy_quicsilver/badge.svg)](https://docs.rs/bevy_quicsilver)

A networking plugin for using the [QUIC](https://quicwg.org/) transport layer protocol with the [Bevy game engine](https://bevyengine.org/).

This crate integrates the [`quinn_proto`](https://github.com/quinn-rs/quinn) library, a pure-rust implementation of QUIC, with the Bevy ECS, providing an idiomatic ecs-based API.

## Features

Ultimately, `bevy_quicsilver` doesn't offer anything more complicated than sending and receiving raw bytes. It is intended to be a foundation ontop of which other libraries implement higher-level features, by abstracting the complexity of the transport protocol behind an ecs-based API.

That is not to say it is entirely barebones though, as a large part of the appeal of QUIC as a transport protocol for games is the wide variety of desireable features that are baked into the spec:
- Both unreliable messages and reliable-ordered streams
- Pluggable cryptography, with a standard implementation powered by [rustls](https://github.com/rustls/rustls) and [*ring*](https://github.com/briansmith/ring)
- Head-of-line blocking control and stream bandwidth sharing
- Simultaneous client/server operation
- IPv4 and IPv6 support
- Cross-platform
