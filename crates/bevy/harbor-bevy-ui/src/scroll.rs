//! Pointer scrolling for Bevy UI nodes.

use bevy::{
    input::mouse::{MouseScrollPixelsPerLine, MouseScrollUnit},
    picking::events::PointerScroll,
    prelude::*,
    ui::{ComputedNode, OverflowAxis, ScrollPosition},
};

/// Attach with `.observe(apply_scroll)` to a scrolling `Node`.
///
/// Needs the `MouseScrollPixelsPerLine` resource from Bevy's input plugin.
/// Bevy's pointer traversal needs `Window` registered, even in windowless apps.
/// Line steps are logical; physical pixel deltas use the node's inverse scale.
/// The nearest node with an enabled axis matching the gesture consumes it.
pub fn apply_scroll(
    mut scroll: On<PointerScroll>,
    pixels_per_line: Res<MouseScrollPixelsPerLine>,
    mut nodes: Query<(&mut ScrollPosition, &Node, &ComputedNode)>,
) {
    let Ok((mut position, node, computed)) = nodes.get_mut(scroll.entity) else {
        return;
    };
    let inverse_scale = computed.inverse_scale_factor();
    let delta = Vec2::new(scroll.x, scroll.y);
    let delta = match scroll.unit {
        MouseScrollUnit::Line => delta * *pixels_per_line,
        MouseScrollUnit::Pixel => delta * inverse_scale,
    };
    // Match the padding-box bounds used by Bevy's layout, in logical pixels.
    let size = computed.size();
    let border = computed.border();
    let max = ((computed.content_size() + computed.scrollbar_size.min(size) - size
        + border.min_inset
        + border.max_inset)
        * inverse_scale)
        .max(Vec2::ZERO);
    let mut next = position.0;
    let mut handled = false;
    if node.overflow.x == OverflowAxis::Scroll && delta.x.is_finite() && delta.x != 0.0 {
        next.x = (next.x - delta.x).clamp(0.0, max.x);
        handled = true;
    }
    if node.overflow.y == OverflowAxis::Scroll && delta.y.is_finite() && delta.y != 0.0 {
        next.y = (next.y - delta.y).clamp(0.0, max.y);
        handled = true;
    }
    if next != position.0 {
        position.0 = next;
    }
    if handled {
        scroll.propagate(false);
    }
}
