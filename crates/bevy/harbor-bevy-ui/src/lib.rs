//! Reusable Bevy UI mechanics. Applications choose their own rendering and
//! windowing backends, layout observers, and action identifiers.

pub mod action_id;
pub mod reactive;
pub mod scroll;
pub mod toast;
pub mod viewport;
pub mod window;

/// Installs viewport tracking, reactive layout, and toast cleanup.
/// Add Bevy's `TimePlugin` separately, usually through a Bevy plugin group.
pub struct HarborUiPlugin;

impl bevy::app::Plugin for HarborUiPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        if !app.is_plugin_added::<viewport::ViewportPlugin>() {
            app.add_plugins(viewport::ViewportPlugin);
        }
        if !app.is_plugin_added::<reactive::LayoutReactivePlugin>() {
            app.add_plugins(reactive::LayoutReactivePlugin);
        }
        if !app.is_plugin_added::<toast::ToastPlugin>() {
            app.add_plugins(toast::ToastPlugin);
        }
    }
}

#[cfg(test)]
#[test]
fn ui_plugin_reuses_existing_viewport_plugin() {
    use bevy::app::App;

    let mut app = App::new();
    app.add_plugins(viewport::ViewportPlugin)
        .add_plugins(HarborUiPlugin);
    assert!(app.is_plugin_added::<reactive::LayoutReactivePlugin>());
    assert!(app.is_plugin_added::<toast::ToastPlugin>());
}
