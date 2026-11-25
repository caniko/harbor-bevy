//! Semantic identifiers for interactive UI entities.

use std::borrow::Cow;

use bevy::prelude::Component;

/// Supplies a semantic label for an application-defined action.
pub trait UiActionLabel {
    fn label(&self) -> &str;
}

impl UiActionLabel for Cow<'static, str> {
    fn label(&self) -> &str {
        self.as_ref()
    }
}

/// Marks an entity with its action, expressed as a string or an application type.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UiActionId<A: Send + Sync + 'static = Cow<'static, str>>(pub A);

impl<A: UiActionLabel + Send + Sync + 'static> UiActionId<A> {
    pub fn label(&self) -> &str {
        self.0.label()
    }
}
