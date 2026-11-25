//! Bevy friend picker for Steam connect-string invites and lobby overlays.

use std::collections::HashSet;

use bevy::{
    ecs::system::SystemParam,
    prelude::*,
    text::{FontSize, FontSource},
    ui::InteractionDisabled,
    ui_widgets::{Activate, Button},
};
use harbor_bevy_core::observer_ordering::AppObserverOrderingExt;

/// Applications provide their invite URI scheme (without `://`) and UI font.
/// A blank scheme disables connect-string invites.
#[derive(Resource, Default)]
pub struct ExternalInviteConfig {
    pub connect_scheme: String,
}

/// Set this to the UI font when the application does not use Bevy's default font.
#[derive(Resource, Default)]
pub struct ExternalInviteFont(pub Handle<Font>);

/// The application supplies Bevy's UI render, picking, and widget plugins.
pub struct ExternalInviteUiPlugin;

impl Plugin for ExternalInviteUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExternalInviteUiState>()
            .init_resource::<ExternalInviteConfig>()
            .init_resource::<ExternalInviteFont>()
            .init_resource::<ExternalInviteFriends>()
            .add_message::<ExternalInviteSent>()
            .add_message::<ExternalInviteFailed>()
            .add_presentation_observer(open_invite_friends_ui)
            .add_presentation_observer(close_invite_friends_ui)
            .add_presentation_observer(handle_invite_ui_buttons);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger<E: Event<Trigger<'static>: Default>>(app: &mut App, event: E) {
        let world = app.world_mut();
        world.trigger(event);
        // Stock Bevy applies the observer's Commands at the next flush.
        world.flush();
    }

    fn button(app: &mut App, action_matches: impl Fn(&Action) -> bool) -> Entity {
        let world = app.world_mut();
        world
            .query_filtered::<(Entity, &Action), With<Button>>()
            .iter(world)
            .find_map(|(entity, action)| action_matches(action).then_some(entity))
            .expect("button not found")
    }

    fn roots(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<ExternalInviteRoot>>()
            .iter(world)
            .count()
    }

    #[test]
    fn synthetic_picker_handles_activate_and_closes_without_steam() {
        let friend_id = ExternalFriendId::synthetic("ada");
        let mut app = App::new();
        app.insert_resource(ExternalInviteConfig {
            connect_scheme: "example-game".into(),
        })
        .insert_resource(ExternalInviteFriends {
            friends: vec![ExternalInviteFriend {
                id: friend_id.clone(),
                name: "Ada".into(),
                state: "Online".into(),
            }],
        })
        .add_plugins(ExternalInviteUiPlugin);

        let open = OpenInviteFriendsUi {
            mode: ExternalInviteMode::ConnectString {
                connect: "example-game://session/123".into(),
            },
            title: Some("Choose a friend".into()),
        };
        trigger(&mut app, open.clone());
        trigger(&mut app, open);
        assert_eq!(roots(&mut app), 1);

        let friend = button(&mut app, |action| matches!(action, Action::Friend(..)));
        let invite = button(&mut app, |action| matches!(action, Action::Invite));
        assert!(app.world().get::<InteractionDisabled>(invite).is_some());
        trigger(&mut app, Activate { entity: friend });
        assert!(app.world().get::<InteractionDisabled>(invite).is_none());
        assert!(
            app.world()
                .resource::<ExternalInviteUiState>()
                .selected
                .contains(&friend_id)
        );
        trigger(&mut app, Activate { entity: invite });

        let failed: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ExternalInviteFailed>>()
            .drain()
            .collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].friend_id.as_ref(), Some(&friend_id));
        assert!(failed[0].reason.contains("not available"));
        assert!(
            app.world()
                .resource::<Messages<ExternalInviteSent>>()
                .is_empty()
        );

        app.world_mut()
            .resource_mut::<ExternalInviteFriends>()
            .friends
            .clear();
        let refresh = button(&mut app, |action| matches!(action, Action::Refresh));
        trigger(&mut app, Activate { entity: refresh });
        assert_eq!(roots(&mut app), 1);
        assert!(
            app.world()
                .resource::<ExternalInviteUiState>()
                .selected
                .is_empty()
        );

        let close = button(&mut app, |action| matches!(action, Action::Close));
        trigger(&mut app, Activate { entity: close });
        assert_eq!(roots(&mut app), 0);
        assert!(!app.world().resource::<ExternalInviteUiState>().is_open);
    }

    #[test]
    fn combined_mode_uses_configured_transport_and_rejects_invalid_lobby() {
        let mut app = App::new();
        app.add_plugins(ExternalInviteUiPlugin);
        trigger(
            &mut app,
            OpenInviteFriendsUi {
                mode: ExternalInviteMode::Both {
                    lobby_id: 0,
                    connect: "example-game://session/123".into(),
                    default: InviteTransport::Lobby,
                },
                title: None,
            },
        );
        let invite = button(&mut app, |action| matches!(action, Action::Invite));
        assert!(app.world().get::<InteractionDisabled>(invite).is_none());
        trigger(&mut app, Activate { entity: invite });
        let failure = app
            .world_mut()
            .resource_mut::<Messages<ExternalInviteFailed>>()
            .drain()
            .next()
            .unwrap();
        assert!(failure.reason.contains("non-zero lobby ID"));

        let transport = button(&mut app, |action| {
            matches!(action, Action::Transport(InviteTransport::ConnectString))
        });
        trigger(&mut app, Activate { entity: transport });
        assert_eq!(
            app.world()
                .resource::<ExternalInviteUiState>()
                .active_transport,
            InviteTransport::ConnectString
        );
        assert!(app.world().get::<InteractionDisabled>(invite).is_some());
    }

    #[test]
    fn validates_connect_strings_before_forwarding_to_the_sdk() {
        let scheme = "example-game";
        assert!(valid_connect_string("example-game://session/123", scheme));
        assert!(valid_connect_string("Example-Game://session/123", scheme));
        for invalid in [
            "",
            "example-game://",
            "different://session/123",
            "example-game://a\0b",
            "example-game://a b",
            &format!("example-game://{}", "a".repeat(256)),
        ] {
            assert!(!valid_connect_string(invalid, scheme));
        }
        assert!(!valid_connect_string("example-game://session/123", ""));
    }
}

#[derive(Event, Debug, Clone)]
pub struct OpenInviteFriendsUi {
    pub mode: ExternalInviteMode,
    pub title: Option<String>,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct CloseInviteFriendsUi;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalFriendsProvider {
    Steam,
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalFriendId {
    pub provider: ExternalFriendsProvider,
    pub value: String,
}

impl ExternalFriendId {
    pub fn steam(id: u64) -> Self {
        Self {
            provider: ExternalFriendsProvider::Steam,
            value: id.to_string(),
        }
    }

    pub fn synthetic(id: impl Into<String>) -> Self {
        Self {
            provider: ExternalFriendsProvider::Synthetic,
            value: id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalInviteMode {
    /// Opens the Steam overlay for the lobby; no individual invites are submitted.
    Lobby {
        lobby_id: u64,
    },
    ConnectString {
        connect: String,
    },
    Both {
        lobby_id: u64,
        connect: String,
        default: InviteTransport,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InviteTransport {
    Lobby,
    #[default]
    ConnectString,
}

/// Submitted to the Steam SDK, not a delivery receipt.
#[derive(Message, Debug, Clone)]
pub struct ExternalInviteSent {
    pub friend_id: ExternalFriendId,
    pub transport: InviteTransport,
}

#[derive(Message, Debug, Clone)]
pub struct ExternalInviteFailed {
    pub friend_id: Option<ExternalFriendId>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalInviteFriend {
    pub id: ExternalFriendId,
    pub name: String,
    pub state: String,
}

/// Live Steam friends when available; callers may supply a synthetic list otherwise.
#[derive(Resource, Debug, Default, Clone)]
pub struct ExternalInviteFriends {
    pub friends: Vec<ExternalInviteFriend>,
}

#[derive(Resource, Debug, Default)]
pub struct ExternalInviteUiState {
    pub is_open: bool,
    pub mode: Option<ExternalInviteMode>,
    pub active_transport: InviteTransport,
    pub selected: HashSet<ExternalFriendId>,
    pub title: String,
}

impl ExternalInviteUiState {
    pub fn can_invite(&self) -> bool {
        self.is_open
            && self.mode.is_some()
            && (self.active_transport == InviteTransport::Lobby || !self.selected.is_empty())
    }
}

#[derive(Component)]
pub struct ExternalInviteRoot;

#[derive(Component)]
enum Action {
    Friend(ExternalFriendId, String),
    Invite,
    Refresh,
    Close,
    Transport(InviteTransport),
}

#[derive(SystemParam)]
struct InvitePresence<'w> {
    #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
    steam: steam_pressure::Presence<'w>,
    #[cfg(not(all(feature = "steam", not(any(target_os = "android", target_os = "ios")))))]
    marker: std::marker::PhantomData<&'w ()>,
}

impl InvitePresence<'_> {
    fn refresh(&self, friends: &mut ExternalInviteFriends) {
        #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
        if self.steam.is_available() {
            friends.friends = self
                .steam
                .friend_summaries()
                .into_iter()
                .map(|friend| ExternalInviteFriend {
                    id: ExternalFriendId::steam(friend.id),
                    name: friend.name,
                    state: friend.state,
                })
                .collect();
        }
        #[cfg(not(all(feature = "steam", not(any(target_os = "android", target_os = "ios")))))]
        let _ = friends;
    }

    fn invite(&self, friend: &ExternalFriendId, connect: &str) -> Result<(), String> {
        #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
        if self.steam.is_available() {
            let id = (friend.provider == ExternalFriendsProvider::Steam)
                .then(|| friend.value.parse::<u64>().ok())
                .flatten()
                .filter(|id| *id != 0)
                .ok_or("Selected friend has no valid Steam ID")?;
            return self
                .steam
                .invite_friend(id, connect)
                .map_err(|error| error.to_string());
        }
        let _ = (friend, connect);
        Err("Steam client is not available".into())
    }

    fn open_lobby_overlay(&self, lobby_id: u64) -> Result<(), String> {
        if lobby_id == 0 {
            return Err("Lobby invite requires a non-zero lobby ID".into());
        }
        #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
        if self
            .steam
            .with_steam(|client| {
                client
                    .friends()
                    .activate_invite_dialog(bevy_steamworks::LobbyId::from_raw(lobby_id));
            })
            .is_some()
        {
            return Ok(());
        }
        Err("Steam client is not available".into())
    }
}

fn open_invite_friends_ui(
    event: On<OpenInviteFriendsUi>,
    mut commands: Commands,
    presence: InvitePresence,
    mut state: ResMut<ExternalInviteUiState>,
    mut friends: ResMut<ExternalInviteFriends>,
    roots: Query<Entity, With<ExternalInviteRoot>>,
    font: Res<ExternalInviteFont>,
) {
    *state = ExternalInviteUiState {
        is_open: true,
        mode: Some(event.mode.clone()),
        active_transport: match &event.mode {
            ExternalInviteMode::Lobby { .. } => InviteTransport::Lobby,
            ExternalInviteMode::ConnectString { .. } => InviteTransport::ConnectString,
            ExternalInviteMode::Both { default, .. } => *default,
        },
        title: event
            .title
            .clone()
            .unwrap_or_else(|| "Invite Friends".into()),
        selected: HashSet::new(),
    };
    presence.refresh(&mut friends);
    despawn_roots(&mut commands, &roots);
    spawn_picker(&mut commands, &font, &state, &friends);
}

fn close_invite_friends_ui(
    _: On<CloseInviteFriendsUi>,
    mut commands: Commands,
    mut state: ResMut<ExternalInviteUiState>,
    roots: Query<Entity, With<ExternalInviteRoot>>,
) {
    *state = ExternalInviteUiState::default();
    despawn_roots(&mut commands, &roots);
}

fn despawn_roots(commands: &mut Commands, roots: &Query<Entity, With<ExternalInviteRoot>>) {
    for root in roots {
        commands.entity(root).despawn();
    }
}

#[derive(SystemParam)]
struct InviteUiQueries<'w, 's> {
    buttons: Query<
        'w,
        's,
        (
            Entity,
            &'static Action,
            &'static Children,
            &'static mut BackgroundColor,
        ),
        With<Button>,
    >,
    captions: Query<'w, 's, &'static mut Text>,
    roots: Query<'w, 's, Entity, With<ExternalInviteRoot>>,
}

fn handle_invite_ui_buttons(
    activate: On<Activate>,
    mut commands: Commands,
    presence: InvitePresence,
    mut state: ResMut<ExternalInviteUiState>,
    mut friends: ResMut<ExternalInviteFriends>,
    mut ui: InviteUiQueries,
    font: Res<ExternalInviteFont>,
    config: Res<ExternalInviteConfig>,
    mut sent: MessageWriter<ExternalInviteSent>,
    mut failed: MessageWriter<ExternalInviteFailed>,
) {
    if !state.is_open {
        return;
    }
    let Ok((_, action, _, _)) = ui.buttons.get(activate.entity) else {
        return;
    };
    match action {
        Action::Friend(id, _) => {
            if !state.selected.remove(id) {
                state.selected.insert(id.clone());
            }
        }
        Action::Invite => {
            if state.can_invite() {
                send_invites(&presence, &state, &config, &mut sent, &mut failed);
            }
            return;
        }
        Action::Refresh => {
            presence.refresh(&mut friends);
            state
                .selected
                .retain(|id| friends.friends.iter().any(|friend| friend.id == *id));
            despawn_roots(&mut commands, &ui.roots);
            spawn_picker(&mut commands, &font, &state, &friends);
            return;
        }
        Action::Close => {
            commands.trigger(CloseInviteFriendsUi);
            return;
        }
        Action::Transport(transport) => state.active_transport = *transport,
    }

    // Update in place so selecting friends does not discard keyboard focus.
    for (entity, action, children, mut color) in &mut ui.buttons {
        let (caption, bg, enabled) = button_style(action, &state);
        if let Some(child) = children.first()
            && let Ok(mut text) = ui.captions.get_mut(*child)
            && **text != caption
        {
            **text = caption;
        }
        color.0 = bg;
        if enabled {
            commands.entity(entity).remove::<InteractionDisabled>();
        } else {
            commands.entity(entity).insert(InteractionDisabled);
        }
    }
}

fn send_invites(
    presence: &InvitePresence,
    state: &ExternalInviteUiState,
    config: &ExternalInviteConfig,
    sent: &mut MessageWriter<ExternalInviteSent>,
    failed: &mut MessageWriter<ExternalInviteFailed>,
) {
    match (&state.mode, state.active_transport) {
        (
            Some(
                ExternalInviteMode::Lobby { lobby_id } | ExternalInviteMode::Both { lobby_id, .. },
            ),
            InviteTransport::Lobby,
        ) => {
            if let Err(reason) = presence.open_lobby_overlay(*lobby_id) {
                failed.write(ExternalInviteFailed {
                    friend_id: None,
                    reason,
                });
            }
        }
        (
            Some(
                ExternalInviteMode::ConnectString { connect }
                | ExternalInviteMode::Both { connect, .. },
            ),
            InviteTransport::ConnectString,
        ) => {
            if !valid_connect_string(connect, &config.connect_scheme) {
                failed.write(ExternalInviteFailed {
                    friend_id: None,
                    reason: "Connect string must use the configured URI scheme and fit Steam's length limit".into(),
                });
                return;
            }
            for friend_id in &state.selected {
                match presence.invite(friend_id, connect) {
                    Ok(()) => {
                        sent.write(ExternalInviteSent {
                            friend_id: friend_id.clone(),
                            transport: InviteTransport::ConnectString,
                        });
                    }
                    Err(reason) => {
                        failed.write(ExternalInviteFailed {
                            friend_id: Some(friend_id.clone()),
                            reason,
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

fn valid_connect_string(connect: &str, expected_scheme: &str) -> bool {
    let Some((scheme, payload)) = connect.split_once("://") else {
        return false;
    };
    !payload.is_empty()
        && connect.len() < 256 // Steam's rich-presence value limit includes the NUL.
        && !connect.chars().any(|c| c.is_whitespace() || c.is_control())
        && scheme.eq_ignore_ascii_case(expected_scheme)
        && scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && scheme.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
}

const BUTTON_BG: Color = Color::srgb(0.20, 0.28, 0.38);
const SELECTED_BG: Color = Color::srgb(0.28, 0.42, 0.60);
const DISABLED_BG: Color = Color::srgb(0.12, 0.12, 0.14);
const TEXT_COLOR: Color = Color::srgb(0.92, 0.90, 0.84);

fn button_style(action: &Action, state: &ExternalInviteUiState) -> (String, Color, bool) {
    let (label, selected) = match action {
        Action::Friend(id, label) => {
            let selected = state.selected.contains(id);
            let marker = if selected { "[x]" } else { "[ ]" };
            (format!("{marker} {label}"), selected)
        }
        Action::Transport(transport) => (
            match transport {
                InviteTransport::Lobby => "Steam Lobby",
                InviteTransport::ConnectString => "Connect Link",
            }
            .into(),
            state.active_transport == *transport,
        ),
        Action::Invite if state.active_transport == InviteTransport::Lobby => {
            ("Open Steam Invite Overlay".into(), true)
        }
        Action::Invite if !state.can_invite() => {
            return ("Select Friends".into(), DISABLED_BG, false);
        }
        Action::Invite => ("Invite".into(), true),
        Action::Refresh => ("Refresh".into(), false),
        Action::Close => ("Close".into(), false),
    };
    (label, if selected { SELECTED_BG } else { BUTTON_BG }, true)
}

fn spawn_picker(
    commands: &mut Commands,
    font: &ExternalInviteFont,
    state: &ExternalInviteUiState,
    friends: &ExternalInviteFriends,
) {
    let root = commands
        .spawn((
            ExternalInviteRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.02, 0.025, 0.92)),
            GlobalZIndex(250),
        ))
        .id();
    let panel = commands
        .spawn((
            Node {
                width: Val::Px(560.0),
                max_width: Val::Percent(92.0),
                max_height: Val::Percent(86.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(18.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.10, 0.10, 0.12)),
            ChildOf(root),
        ))
        .id();
    spawn_label(commands, panel, font, &state.title, 28.0);
    if matches!(state.mode, Some(ExternalInviteMode::Both { .. })) {
        for transport in [InviteTransport::ConnectString, InviteTransport::Lobby] {
            spawn_button(commands, panel, font, Action::Transport(transport), state);
        }
    }

    let list = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                overflow: Overflow::scroll_y(),
                row_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    if friends.friends.is_empty() {
        spawn_label(commands, list, font, "No Steam friends available.", 18.0);
    }
    for friend in &friends.friends {
        spawn_button(
            commands,
            list,
            font,
            Action::Friend(
                friend.id.clone(),
                format!("{} — {}", friend.name, friend.state),
            ),
            state,
        );
    }
    let actions = commands
        .spawn((
            Node {
                flex_wrap: FlexWrap::Wrap,
                justify_content: JustifyContent::FlexEnd,
                column_gap: Val::Px(8.0),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    for action in [Action::Refresh, Action::Close, Action::Invite] {
        spawn_button(commands, actions, font, action, state);
    }
}

fn spawn_button(
    commands: &mut Commands,
    parent: Entity,
    font: &ExternalInviteFont,
    action: Action,
    state: &ExternalInviteUiState,
) {
    let (label, bg, enabled) = button_style(&action, state);
    let mut button = commands.spawn((
        Button,
        action,
        Node {
            padding: UiRect::axes(Val::Px(14.0), Val::Px(9.0)),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(bg),
        ChildOf(parent),
    ));
    if !enabled {
        button.insert(InteractionDisabled);
    }
    let entity = button.id();
    spawn_label(commands, entity, font, label, 17.0);
}

fn spawn_label(
    commands: &mut Commands,
    parent: Entity,
    font: &ExternalInviteFont,
    label: impl Into<String>,
    size: f32,
) {
    commands.spawn((
        Text::new(label),
        TextFont {
            font: FontSource::Handle(font.0.clone()),
            font_size: FontSize::Rem(size / 16.0),
            ..default()
        },
        TextColor(TEXT_COLOR),
        ChildOf(parent),
    ));
}
