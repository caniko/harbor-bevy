//! Publish displayed action bounds in keyboard traversal order.

use bevy::prelude::*;

use crate::pointer_router::LogicalRect;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct FocusBounds<T> {
    pub target: T,
    pub bounds: LogicalRect,
    pub keyboard_focusable: bool,
}

#[derive(Debug, Clone, PartialEq, Resource)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct FocusBoundsPort<T> {
    /// Published in tab order. Focusable identities should be unique.
    pub targets: Vec<FocusBounds<T>>,
}

impl<T> Default for FocusBoundsPort<T> {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Resource)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct ActionFocus<T>(pub Option<T>);

impl<T> Default for ActionFocus<T> {
    fn default() -> Self {
        Self(None)
    }
}

/// Cycle eligible actions on Tab or Shift+Tab; clear stale focus even when
/// there is no keyboard input. Order this after the bounds publisher.
pub fn advance_action_focus<T: Clone + Eq + Send + Sync + 'static>(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    bounds: Res<FocusBoundsPort<T>>,
    mut focus: ResMut<ActionFocus<T>>,
) {
    let eligible = || {
        bounds
            .targets
            .iter()
            .filter(|entry| entry.keyboard_focusable)
    };
    let current = eligible().position(|entry| Some(&entry.target) == focus.0.as_ref());
    let next = if let Some(keys) = keys.filter(|keys| keys.just_pressed(KeyCode::Tab)) {
        if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            current
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| eligible().nth(i))
                .or_else(|| eligible().next_back())
        } else {
            current
                .and_then(|i| eligible().nth(i + 1))
                .or_else(|| eligible().next())
        }
    } else if current.is_some() {
        return;
    } else {
        None
    };
    let target = next.map(|entry| &entry.target);
    if focus.0.as_ref() != target {
        focus.0 = target.cloned();
    }
}
