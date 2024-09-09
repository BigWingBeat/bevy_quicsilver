use std::ops::Deref;

use bevy::{
    color::palettes::tailwind::*, ecs::system::IntoObserverSystem, prelude::*, ui::FocusPolicy,
};
use bevy_simple_text_input::{
    TextInput, TextInputInactive, TextInputPlaceholder, TextInputPlugin, TextInputTextStyle,
};

use crate::{client::start_client, server::start_server};

const WINDOW_BACKGROUND: ClearColor = ClearColor(Color::Srgba(ZINC_700));
const BUTTON_BACKGROUND: Color = Color::Srgba(ZINC_800);
const TEXT_COLOR: Color = Color::Srgba(ZINC_50);
const TITLE_TEXT_SIZE: f32 = 80.0;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(TextInputPlugin)
            .insert_resource(WINDOW_BACKGROUND)
            .add_systems(Startup, spawn_menus)
            .add_systems(Update, (focus, button));
    }
}

#[derive(Event)]
struct ButtonPress;

#[derive(Resource, Deref)]
pub struct TitleRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct HostRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct JoinRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct GameRoot(pub Entity);

/// Spawn the menus
pub fn spawn_menus(world: &mut World) {
    spawn_title(world);
    spawn_host(world);
    spawn_join(world);
    spawn_game(world);
}

/// Spawn the title screen:
/// - Title text
/// - Username input
/// - Host game button
/// - Join game button
/// - Quit game button
fn spawn_title(world: &mut World) {
    let id = spawn_root(world, Display::Flex)
        .with_children(|parent| {
            spawn_text(parent, "Digger Demo Example", TITLE_TEXT_SIZE);
            spawn_textbox(parent, "Enter Username...");
            spawn_button(parent, "Host Game", switch_menu::<TitleRoot, HostRoot>);
            spawn_button(parent, "Join Game", switch_menu::<TitleRoot, JoinRoot>);
            spawn_button(
                parent,
                "Quit Game",
                |_: Trigger<_>, mut e: EventWriter<AppExit>| {
                    e.send(AppExit::Success);
                },
            );
        })
        .id();
    world.insert_resource(TitleRoot(id));
}

/// Spawn the 'host a game' menu:
/// - Title text
/// - Permissions menu
///     - Join password input
///     - Edit permission whitelist or blacklist
/// - Start server button
/// - Back to title button
fn spawn_host(world: &mut World) {
    let id = spawn_root(world, Display::None)
        .with_children(|parent| {
            spawn_text(parent, "Host a game", TITLE_TEXT_SIZE);
            spawn_textbox(parent, "Server Password...");

            parent
                .spawn(NodeBundle {
                    style: Style {
                        column_gap: Val::Px(50.0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceAround,
                        ..default()
                    },
                    ..default()
                })
                .with_children(|parent| {
                    spawn_text(parent, "Edit permissions:", 40.0);
                    spawn_button(parent, "Whitelist", |_: Trigger<_>| {});
                    spawn_button(parent, "Blacklist", |_: Trigger<_>| {});
                });

            spawn_button(parent, "Start Server", switch_menu::<HostRoot, GameRoot>)
                .observe(start_server::<ButtonPress>);
            spawn_button(parent, "Back", switch_menu::<HostRoot, TitleRoot>);
        })
        .id();
    world.insert_resource(HostRoot(id));
}

/// Spawn the 'join a game' menu:
/// - Title text
/// - Server input
/// - Join button
/// - Back to title button
fn spawn_join(world: &mut World) {
    let id = spawn_root(world, Display::None)
        .with_children(|parent| {
            spawn_text(parent, "Join a game", TITLE_TEXT_SIZE);
            spawn_textbox(parent, "Enter Server IP...");
            spawn_button(parent, "Join Server", switch_menu::<JoinRoot, GameRoot>)
                .observe(start_client::<ButtonPress>);
            spawn_button(parent, "Back", switch_menu::<JoinRoot, TitleRoot>);
        })
        .id();
    world.insert_resource(JoinRoot(id));
}

/// Spawn the in-game GUI:
/// - Nothing for now
fn spawn_game(world: &mut World) {
    let id = spawn_root(world, Display::None).id();
    world.insert_resource(GameRoot(id));
}

fn spawn_root(world: &mut World, display: Display) -> EntityWorldMut {
    world.spawn((
        NodeBundle {
            style: Style {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceAround,
                display,
                ..default()
            },
            ..default()
        },
        Interaction::None,
    ))
}

fn spawn_textbox(parent: &mut WorldChildBuilder, placeholder: impl Into<String>) {
    parent.spawn((
        NodeBundle {
            style: Style {
                width: Val::Px(500.0),
                padding: UiRect::axes(Val::Percent(0.9), Val::Percent(0.25)),
                ..default()
            },
            focus_policy: FocusPolicy::Block,
            border_radius: BorderRadius::MAX,
            border_color: BUTTON_BACKGROUND.into(),
            background_color: BUTTON_BACKGROUND.into(),
            ..default()
        },
        TextInput,
        TextInputTextStyle(TextStyle {
            font_size: 40.0,
            color: TEXT_COLOR,
            ..default()
        }),
        TextInputPlaceholder {
            value: placeholder.into(),
            text_style: None,
        },
        TextInputInactive(true),
    ));
}

fn spawn_text(parent: &mut WorldChildBuilder, text: impl Into<String>, font_size: f32) {
    parent.spawn(
        TextBundle::from_section(
            text,
            TextStyle {
                font_size,
                color: TEXT_COLOR,
                ..default()
            },
        )
        .with_text_justify(JustifyText::Center),
    );
}

fn spawn_button<'a, B: Bundle, M>(
    parent: &'a mut WorldChildBuilder<'_>,
    text: impl Into<String>,
    on_press: impl IntoObserverSystem<ButtonPress, B, M>,
) -> EntityWorldMut<'a> {
    let mut e = parent.spawn(ButtonBundle {
        style: Style {
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Percent(0.9), Val::Percent(0.25)),
            ..default()
        },
        border_radius: BorderRadius::MAX,
        border_color: BUTTON_BACKGROUND.into(),
        background_color: BUTTON_BACKGROUND.into(),
        ..default()
    });
    e.with_child(TextBundle::from_section(
        text,
        TextStyle {
            font_size: 40.0,
            color: TEXT_COLOR,
            ..default()
        },
    ))
    .observe(on_press);
    e
}

fn switch_menu<F: Deref<Target = Entity> + Resource, T: Deref<Target = Entity> + Resource>(
    _: Trigger<ButtonPress>,
    mut style: Query<&mut Style>,
    from: Res<F>,
    to: Res<T>,
) {
    style.get_mut(**from).unwrap().display = Display::None;
    style.get_mut(**to).unwrap().display = Display::Flex;
}

#[allow(clippy::type_complexity)]
fn button(
    mut commands: Commands,
    query: Query<(Entity, &Interaction), (Changed<Interaction>, With<Button>)>,
) {
    for (entity, interaction) in &query {
        if let Interaction::Pressed = interaction {
            commands.trigger_targets(ButtonPress, entity);
        }
    }
}

fn focus(
    query: Query<(Entity, &Interaction), Changed<Interaction>>,
    mut text_input_query: Query<(Entity, &mut TextInputInactive)>,
) {
    for (interaction_entity, interaction) in &query {
        if *interaction == Interaction::Pressed {
            for (entity, mut inactive) in &mut text_input_query {
                inactive.0 = entity != interaction_entity;
            }
        }
    }
}