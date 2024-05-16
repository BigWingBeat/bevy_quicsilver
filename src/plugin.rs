use bevy_app::{App, Plugin, Update};

use crate::{
    connection::{poll_connections, send_connection_established_events},
    endpoint::{find_new_connections, poll_endpoints},
    incoming::{handle_incoming_responses, send_new_incoming_events},
};

#[derive(Debug)]
pub struct QuinnPlugin;

impl Plugin for QuinnPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                find_new_connections,
                poll_endpoints,
                poll_connections,
                handle_incoming_responses,
                send_new_incoming_events,
                send_connection_established_events,
            ),
        );
    }
}
