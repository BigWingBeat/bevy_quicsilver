use bevy_app::{App, Plugin, PostUpdate, PreUpdate};
use bevy_ecs::schedule::{apply_deferred, IntoSystemConfigs};
use bevy_time::TimePlugin;

use crate::{
    connection::{
        poll_connections, send_connection_established_events, ConnectionEstablished,
        HandshakeDataReady,
    },
    endpoint::poll_endpoints,
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
                PreUpdate,
                (
                    poll_endpoints,           // Handles receiving data, so runs in PreUpdate
                    apply_deferred, // Manually insert apply_deferred because auto_insert_sync_points could be disabled by something else
                    send_new_incoming_events, // Handles new Incomings spawned by poll_endpoint
                )
                    .chain(),
            )
            .add_systems(
                PostUpdate,
                (
                    handle_incoming_responses, // Exclusive system, handles user responses and possibly sends data, so runs in PostUpdate
                    poll_connections, // Sends data, and handles new connections spawned by handle_incoming_responses
                    apply_deferred, // Manually insert apply_deferred because auto_insert_sync_points could be disabled by something else
                    send_connection_established_events, // Handles component changes made by poll_connections
                )
                    .chain(),
            );
    }
}
