use std::ops::Deref;

use bevy::{
    color::palettes::tailwind::*, ecs::system::IntoObserverSystem, prelude::*, ui::FocusPolicy,
};
use bevy_simple_text_input::{
    TextInput, TextInputInactive, TextInputPlaceholder, TextInputPlugin, TextInputValue,
};
use bevy_state::state::NextState;

use crate::{
    client::ServerAddress, server::EditPermissionMode, AppState, ErrorMessage, Password, Username,
};

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
            .add_systems(Update, (button, textinput_focus, textinput_change))
            .add_observer(switch_menu_handler)
            .add_observer(error_message);
    }
}

#[derive(Event)]
struct ButtonPress;

#[derive(Event)]
struct SwitchMenu {
    from: Entity,
    to: Entity,
}

#[derive(Event)]
struct TextInputChanged(String);

#[derive(Resource)]
struct CurrentMenu(Entity);

#[derive(Resource, Deref)]
pub struct TitleRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct HostRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct JoinRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct ErrorRoot(pub Entity);

#[derive(Resource, Deref)]
pub struct GameRoot(pub Entity);

#[derive(Resource)]
struct ErrorText(Entity);

/* Startup systems */

/// Spawn the menus
pub fn spawn_menus(world: &mut World) {
    spawn_title(world);
    spawn_host(world);
    spawn_join(world);
    spawn_error(world);
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
            spawn_textbox(parent, "Enter Username...", copy_to_resource::<Username>);
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
    world.insert_resource(CurrentMenu(id));
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
            spawn_textbox(parent, "Server Password...", copy_to_resource::<Password>);

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
                    spawn_button(
                        parent,
                        "Whitelist",
                        |_: Trigger<_>, mut mode: ResMut<EditPermissionMode>| {
                            *mode = EditPermissionMode::Whitelist;
                        },
                    );
                    spawn_button(
                        parent,
                        "Blacklist",
                        |_: Trigger<_>, mut mode: ResMut<EditPermissionMode>| {
                            *mode = EditPermissionMode::Blacklist;
                        },
                    );
                });

            spawn_button(parent, "Start Server", switch_menu::<HostRoot, GameRoot>).observe(
                |_: Trigger<ButtonPress>, mut state: ResMut<NextState<AppState>>| {
                    state.set(AppState::Server)
                },
            );
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
            spawn_textbox(
                parent,
                "Enter Server Address...",
                copy_to_resource::<ServerAddress>,
            );
            spawn_textbox(
                parent,
                "Enter Server Password...",
                copy_to_resource::<Password>,
            );
            spawn_button(parent, "Join Server", switch_menu::<JoinRoot, GameRoot>).observe(
                |_: Trigger<ButtonPress>, mut state: ResMut<NextState<AppState>>| {
                    state.set(AppState::Client)
                },
            );
            spawn_button(parent, "Back", switch_menu::<JoinRoot, TitleRoot>);
        })
        .id();
    world.insert_resource(JoinRoot(id));
}

/// Spawn the error message screen:
/// - Title text
/// - Error message
/// - Back to title button
fn spawn_error(world: &mut World) {
    let mut text_id = Entity::PLACEHOLDER;
    let id = spawn_root(world, Display::None)
        .with_children(|parent| {
            spawn_text(parent, "Error", TITLE_TEXT_SIZE);
            text_id = spawn_text(parent, "", TITLE_TEXT_SIZE).id();
            spawn_button(parent, "Back", switch_menu::<ErrorRoot, TitleRoot>);
        })
        .id();
    world.insert_resource(ErrorRoot(id));
    world.insert_resource(ErrorText(text_id));
}

/// Spawn the in-game GUI:
/// - Nothing for now
fn spawn_game(world: &mut World) {
    let id = spawn_root(world, Display::None).id();
    world.insert_resource(GameRoot(id));
}

/* Helper functions */

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

fn spawn_textbox<B: Bundle, M>(
    parent: &mut WorldChildBuilder,
    placeholder: impl Into<String>,
    on_change: impl IntoObserverSystem<TextInputChanged, B, M>,
) {
    parent
        .spawn((
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
        ))
        .observe(on_change);
}

fn spawn_text<'a>(
    parent: &'a mut WorldChildBuilder<'_>,
    text: impl Into<String>,
    font_size: f32,
) -> EntityWorldMut<'a> {
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
    )
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

fn switch_menu_handler(
    trigger: Trigger<SwitchMenu>,
    mut style: Query<&mut Style>,
    mut current_menu: ResMut<CurrentMenu>,
) {
    let &SwitchMenu { from, to } = trigger.event();
    style.get_mut(from).unwrap().display = Display::None;
    style.get_mut(to).unwrap().display = Display::Flex;
    current_menu.0 = to;
}

fn switch_menu<F: Deref<Target = Entity> + Resource, T: Deref<Target = Entity> + Resource>(
    _: Trigger<ButtonPress>,
    mut commands: Commands,
    from: Res<F>,
    to: Res<T>,
) {
    commands.trigger(SwitchMenu {
        from: **from,
        to: **to,
    });
}

fn copy_to_resource<T: From<String> + Resource>(
    trigger: Trigger<TextInputChanged>,
    mut res: ResMut<T>,
) {
    *res = trigger.event().0.clone().into();
}

/* Other systems */

#[expect(clippy::type_complexity)]
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

fn textinput_focus(
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

fn textinput_change(
    mut commands: Commands,
    query: Query<(Entity, &TextInputValue), Changed<TextInputValue>>,
) {
    for (entity, value) in &query {
        commands.trigger_targets(TextInputChanged(value.0.clone()), entity);
    }
}

fn error_message(
    trigger: Trigger<ErrorMessage>,
    mut commands: Commands,
    mut query: Query<&mut Text>,
    current_menu: Res<CurrentMenu>,
    error_menu: Res<ErrorRoot>,
    error_text: Res<ErrorText>,
) {
    commands.trigger(SwitchMenu {
        from: current_menu.0,
        to: error_menu.0,
    });

    let mut text = query.get_mut(error_text.0).unwrap();
    text.sections.clear();
    text.sections.push(TextSection::new(
        trigger.event().0.to_string(),
        TextStyle {
            font_size: TITLE_TEXT_SIZE,
            color: TEXT_COLOR,
            ..default()
        },
    ));
}
