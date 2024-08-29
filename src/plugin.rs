use bevy_app::{App, Plugin, PostUpdate, PreUpdate};
use bevy_ecs::schedule::IntoSystemConfigs;
use bevy_time::TimePlugin;

use crate::{
    connection::{poll_connections, ConnectionError, HandshakeDataReady},
    endpoint::{poll_endpoints, EndpointError},
    incoming::{handle_incoming_responses, IncomingError},
    IncomingResponse, NewIncoming,
};

/// The library plugin. Adding this to your [`App`] is the first thing to do when using this library.
///
/// # Usage
/// ```
/// # use bevy_app::{App, AppExit};
/// # use bevy_quicsilver::QuicPlugin;
/// fn main() -> AppExit {
///     App::new()
///         .add_plugins(QuicPlugin)
///         /* ... */
///         .run()
/// }
/// ```
#[derive(Debug)]
pub struct QuicPlugin;

impl Plugin for QuicPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<TimePlugin>() {
            app.add_plugins(TimePlugin);
        }

        // System ordering assumes relevant user code runs in Update
        app.add_event::<NewIncoming>()
            .add_event::<IncomingResponse>()
            .add_event::<EndpointError>()
            .add_event::<IncomingError>()
            .add_event::<ConnectionError>()
            .add_event::<HandshakeDataReady>()
            .add_systems(
                PreUpdate,
                poll_endpoints, // Handles receiving data for the user to process, so runs in PreUpdate
            )
            .add_systems(
                PostUpdate,
                (
                    handle_incoming_responses, // Exclusive system, handles user responses and possibly sends data, so runs in PostUpdate
                    poll_connections, // Sends data, and handles new connections spawned by handle_incoming_responses
                )
                    .chain(),
            );
    }
}
