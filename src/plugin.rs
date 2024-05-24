use bevy_app::{App, Plugin, Update};
use bevy_ecs::schedule::{apply_deferred, IntoSystemConfigs};
use bevy_time::TimePlugin;

use crate::{
    connection::{
        poll_connections, send_connection_established_events, ConnectionEstablished,
        HandshakeDataReady,
    },
    endpoint::{find_new_connections, poll_endpoints},
    incoming::{handle_incoming_responses, send_new_incoming_events},
    EntityError, IncomingResponse, NewIncoming,
};

#[derive(Debug)]
pub struct QuicPlugin;

impl Plugin for QuicPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<TimePlugin>() {
            app.add_plugins(TimePlugin);
        }

        app.add_event::<NewIncoming>()
            .add_event::<IncomingResponse>()
            .add_event::<EntityError>()
            .add_event::<ConnectionEstablished>()
            .add_event::<HandshakeDataReady>()
            .add_systems(
                Update,
                ((
                    handle_incoming_responses, // Adds connections to entities
                    apply_deferred,
                    find_new_connections, // Adds ConnectionId -> Entity mappings for client connections
                    (
                        poll_endpoints, // Needs to see connections on entities, and adds Incomings to entities
                        poll_connections, // Needs to see connections on entities, and signals connection established
                    ),
                    apply_deferred,
                    (
                        send_new_incoming_events,           // Needs to see Incomings on entities
                        send_connection_established_events, // Needs to see connection established signals
                    ),
                )
                    .chain(),),
            );
    }
}
