use std::{net::SocketAddr, sync::Arc};

use bevy_app::{App, Plugin};
use bevy_ecs::system::Resource;
use quinn_proto::{Dir, EndpointConfig};

use crate::{connection::Connection, Error};

#[derive(Debug)]
pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {}
}
