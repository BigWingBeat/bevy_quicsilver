use bevy_app::{App, Plugin, PostUpdate, PreUpdate};
use bevy_core::TaskPoolPlugin;
use bevy_ecs::schedule::IntoSystemConfigs;
use bevy_time::TimePlugin;

use crate::{
    connection::poll_connections, endpoint::poll_endpoints, incoming::handle_incoming_responses,
    IncomingResponse,
};

/// The library plugin. Adding this to your [`App`] is the first thing to do when using this library.
///
/// ## Plugin Dependencies
///
/// `QuicPlugin` depends on two of Bevy's built-in plugins, and will automatically add them to the `App`
/// if they have not already been added:
/// - [`TimePlugin`]
/// - [`TaskPoolPlugin`]
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

        if !app.is_plugin_added::<TaskPoolPlugin>() {
            app.add_plugins(TaskPoolPlugin::default());
        }

        // System ordering assumes relevant user code runs in Update
        app.add_event::<IncomingResponse>()
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
