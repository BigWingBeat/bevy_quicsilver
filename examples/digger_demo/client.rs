use std::net::Ipv6Addr;

use bevy::prelude::{Commands, Trigger};
use bevy_app::{App, Plugin};
use bevy_quicsilver::EndpointBundle;

use crate::crypto::trust_on_first_use_config;

pub(super) struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        todo!()
    }
}

pub fn start_client<T>(_: Trigger<T>, mut commands: Commands) {
    let endpoint = EndpointBundle::new_client(
        (Ipv6Addr::UNSPECIFIED, 0).into(),
        Some(trust_on_first_use_config()),
    )
    .unwrap();

    commands.spawn(endpoint);
}
