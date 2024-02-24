# bevy_quinntegration

[![crate](https://img.shields.io/crates/v/bevy_quinntegration.svg)](https://crates.io/crates/bevy_quinntegration)
[![documentation](https://docs.rs/bevy_quinntegration/badge.svg)](https://docs.rs/bevy_quinntegration)

A networking plugin for using the [QUIC](https://quicwg.org/) protocol with the [Bevy game engine](https://bevyengine.org/).

This crate integrates the [Quinn](https://github.com/quinn-rs/quinn) library, a pure-rust implementation of QUIC, with the Bevy ECS, providing an idiomatic, plugin-based API.

## Design and Motivation

The Quinn crate has two layers: `quinn`, the Tokio-based async API, and `quinn_proto`, the fully-synchronous, sans-I/O, actual implementation of the QUIC protocol.

`bevy_quinntegration` does not depend on `quinn` at all, instead directly integrating `quinn_proto` with the Bevy ECS. Given that Bevy's systems are normal, synchronous rust functions, that have relatively poor UX for interacting with anything async, this drastically reduces complexity and boilerplate when compared to naively integrating `quinn`'s async API with Bevy.

There already exists a crate that integrates Quinn with Bevy, namely [bevy_quinnet](https://crates.io/crates/bevy_quinnet). However, I was dissatisfied with the way it is implemented, as it makes the aforementioned naive decision of integrating `quinn`'s async API, while also dragging in the entire Tokio runtime, which competes with Bevy's built-in async runtime for system resources. These factors led to me creating this crate - initially just for personal use, before deciding to make it general-purpose.

## Scope

`bevy_quinntegration` aims to be a relatively unopinionated, low-level crate, offering ECS-idiomatic translations of `quinn_proto`'s API, and little else. Most notably, this crate only deals in raw bytes, and has no built-in serialization/deserialization functionality. The intent is for other crates to be build on-top of this one, that offer more opinionated, higher-level functionality.
