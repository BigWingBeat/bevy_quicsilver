use bevy::prelude::Trigger;
use bevy_app::{App, Plugin};

pub(super) struct ServerPlugin;

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        todo!()
    }
}

pub fn start_server<T>(_: Trigger<T>) {}
