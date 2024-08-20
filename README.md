# bevy_quicsilver

[![crate](https://img.shields.io/crates/v/bevy_quicsilver.svg)](https://crates.io/crates/bevy_quicsilver)
[![documentation](https://docs.rs/bevy_quicsilver/badge.svg)](https://docs.rs/bevy_quicsilver)

A networking plugin for using the [QUIC](https://quicwg.org/) transport layer protocol with the [Bevy game engine](https://bevyengine.org/).

This crate integrates the [`quinn_proto`](https://github.com/quinn-rs/quinn) library, a pure-rust implementation of QUIC, with the Bevy ECS, providing an idiomatic ecs-based API.

## Development Status

This library is still very new and in active development. Although most of the core functionality is in place, it's not in a usable state yet.

## Supported Bevy Versions

- `0.15.*`

## Features

- Both unreliable-unordered messages and reliable-ordered streams
- Pluggable cryptography, with a standard implementation powered by [rustls](https://github.com/rustls/rustls) and [*ring*](https://github.com/briansmith/ring)
- Head-of-line blocking control and stream bandwidth sharing
- Simultaneous client/server operation
- IPv4 and IPv6 support
- Cross-platform

`bevy_quicsilver` is a low-level networking library that offers granular control over connections and data transfer. Implementing higher-level features such as ser/deserialization, specific network topologies, automatic state transfer/interpolation, etc. are out of scope for the library.
