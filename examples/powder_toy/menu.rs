use bevy::{
    color::palettes::tailwind::*, ecs::system::IntoObserverSystem, prelude::*, ui::FocusPolicy,
};
use bevy_simple_text_input::{
    TextInput, TextInputInactive, TextInputPlaceholder, TextInputTextStyle,
};

const WINDOW_BACKGROUND: ClearColor = ClearColor(Color::Srgba(ZINC_700));
const BUTTON_BACKGROUND: Color = Color::Srgba(ZINC_800);
const TEXT_COLOR: Color = Color::Srgba(ZINC_50);

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WINDOW_BACKGROUND)
            .add_systems(Startup, spawn_menus)
            .add_systems(Update, (focus, button));
    }
}

#[derive(Event)]
struct ButtonPress;

/// Spawn the menus
pub fn spawn_menus(world: &mut World) {
    spawn_title(world);
    spawn_host(world);
    spawn_join(world);
}

/// Spawn the title screen:
/// - Title text
/// - Username input
/// - Host game button
/// - Join game button
/// - Quit game button
fn spawn_title(world: &mut World) {
    world
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceAround,
                    ..default()
                },
                ..default()
            },
            Interaction::None,
        ))
        .with_children(|parent| {
            parent.spawn(
                TextBundle::from_section(
                    "Powder Toy Example",
                    TextStyle {
                        font_size: 80.0,
                        color: TEXT_COLOR,
                        ..default()
                    },
                )
                .with_text_justify(JustifyText::Center),
            );

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
                    value: "Enter Username...".into(),
                    text_style: None,
                },
                TextInputInactive(true),
            ));

            spawn_button(parent, "Host Game", |_: Trigger<ButtonPress>| {});
            spawn_button(parent, "Join Game", |_: Trigger<ButtonPress>| {});
            spawn_button(
                parent,
                "Quit Game",
                |_: Trigger<ButtonPress>, mut e: EventWriter<AppExit>| {
                    e.send(AppExit::Success);
                },
            );
        });
}

/// Spawn the 'host a game' menu:
/// - Title text
/// - Permissions menu
///     - Join password input
///     - Edit permission whitelist or blacklist
/// - Start server button
/// - Back to title button
fn spawn_host(world: &mut World) {}

/// Spawn the 'join a game' menu:
/// - Title text
/// - Server input
/// - Join button
/// - Back to title button
fn spawn_join(world: &mut World) {}

fn spawn_button<B: Bundle, M>(
    parent: &mut WorldChildBuilder,
    text: impl Into<String>,
    on_press: impl IntoObserverSystem<ButtonPress, B, M>,
) {
    parent
        .spawn(ButtonBundle {
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
        })
        .with_child(TextBundle::from_section(
            text,
            TextStyle {
                font_size: 40.0,
                color: TEXT_COLOR,
                ..default()
            },
        ))
        .observe(on_press);
}

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
