//! The window's own chrome, now that the tab strip replaces the system title
//! bar: switching, closing, reordering and scrolling tabs, the new-tab
//! button, the window buttons, moving the window by its empty strip, and
//! resizing it from its edges. Ported from trust-desktop's
//! `window_chrome.rs`.

use std::time::{Duration, Instant};

use winit::dpi::LogicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::window::{CursorIcon, ResizeDirection};

use crate::net::NetworkId;
use crate::render::Point;
use crate::ui::tab_strip::{STRIP_HEIGHT, TabStripGeometry, TabStripPart};

use super::App;

/// The band along the window's edges that resizes it while it is neither
/// maximized nor fullscreen.
const RESIZE_BORDER: f32 = 5.0;
/// Along the top, between its corners, only this much of the band resizes:
/// the tabs fill the strip below it and must be easy to hit.
const TOP_RESIZE_BORDER: f32 = 2.0;
/// A second press on the empty strip within this time maximizes the window.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// How far a pressed tab moves before the press becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

#[derive(Default)]
pub(super) struct StripUi {
    pub(super) scroll: f32,
    pub(super) hovered: Option<TabStripPart>,
    /// The part a press began on, and its button; its release acts on it.
    pub(super) pressed: Option<(TabStripPart, MouseButton)>,
    pub(super) drag: Option<TabDrag>,
    last_empty_press: Option<Instant>,
    /// Scroll the active tab into view at the next paint.
    pub(super) reveal_active: bool,
}

/// A tab the pointer pressed, and how far it has been dragged.
pub(super) struct TabDrag {
    pub(super) id: NetworkId,
    /// The pointer's x where the dragged tab's slot would put it unmoved.
    origin: f32,
    pub(super) offset: f32,
    pub(super) moved: bool,
}

impl App {
    pub(super) fn strip_geometry(&self) -> Option<TabStripGeometry> {
        let view = self.view.as_ref()?;
        Some(TabStripGeometry::new(
            view.logical_width(),
            self.networks.len(),
            self.strip.scroll,
        ))
    }

    fn pointer(&self) -> Option<Point> {
        self.view.as_ref().map(|view| view.pointer)
    }

    /// The window edge under `point` that resizes the window, if any.
    fn resize_edge_at(&self, point: Point) -> Option<ResizeDirection> {
        let view = self.view.as_ref()?;
        if view.window.is_maximized() || view.window.fullscreen().is_some() {
            return None;
        }
        let (width, height) = (view.logical_width(), view.logical_height());
        let west = point.x < RESIZE_BORDER;
        let east = point.x >= width - RESIZE_BORDER;
        let north = point.y < TOP_RESIZE_BORDER || (point.y < RESIZE_BORDER && (west || east));
        let south = point.y >= height - RESIZE_BORDER;
        match (north, south, west, east) {
            (true, _, true, _) => Some(ResizeDirection::NorthWest),
            (true, _, _, true) => Some(ResizeDirection::NorthEast),
            (_, true, true, _) => Some(ResizeDirection::SouthWest),
            (_, true, _, true) => Some(ResizeDirection::SouthEast),
            (true, ..) => Some(ResizeDirection::North),
            (_, true, ..) => Some(ResizeDirection::South),
            (_, _, true, _) => Some(ResizeDirection::West),
            (_, _, _, true) => Some(ResizeDirection::East),
            _ => None,
        }
    }

    /// Pointer motion over the strip or a resizing edge. Returns whether the
    /// chrome took it.
    pub(super) fn chrome_pointer_moved(&mut self) -> bool {
        let Some(point) = self.pointer() else {
            return false;
        };
        if let Some(drag) = &mut self.strip.drag {
            let offset = point.x - drag.origin;
            drag.moved |= offset.abs() >= DRAG_THRESHOLD;
            drag.offset = offset;
            if drag.moved {
                self.reorder_dragged_tab();
            }
            self.mark_strip();
            return true;
        }
        if self.strip.pressed.is_some() {
            let hovered = self
                .strip_geometry()
                .and_then(|geometry| geometry.part_at(point));
            if hovered != self.strip.hovered {
                self.strip.hovered = hovered;
                self.mark_strip();
            }
            return true;
        }
        if let Some(edge) = self.resize_edge_at(point) {
            self.clear_strip_hover();
            self.set_cursor(match edge {
                ResizeDirection::East => CursorIcon::EResize,
                ResizeDirection::North => CursorIcon::NResize,
                ResizeDirection::NorthEast => CursorIcon::NeResize,
                ResizeDirection::NorthWest => CursorIcon::NwResize,
                ResizeDirection::South => CursorIcon::SResize,
                ResizeDirection::SouthEast => CursorIcon::SeResize,
                ResizeDirection::SouthWest => CursorIcon::SwResize,
                ResizeDirection::West => CursorIcon::WResize,
            });
            return true;
        }
        if point.y >= STRIP_HEIGHT {
            // The body sets its own cursor.
            self.clear_strip_hover();
            return false;
        }
        self.set_cursor(CursorIcon::Default);
        let hovered = self
            .strip_geometry()
            .and_then(|geometry| geometry.part_at(point));
        if hovered != self.strip.hovered {
            self.strip.hovered = hovered;
            self.mark_strip();
        }
        true
    }

    pub(super) fn clear_strip_hover(&mut self) {
        if self.strip.pressed.is_none() && self.strip.hovered.take().is_some() {
            self.mark_strip();
        }
    }

    /// A button press or release over the strip or a resizing edge. Returns
    /// whether the chrome took it.
    pub(super) fn chrome_pointer_button(
        &mut self,
        state: ElementState,
        button: MouseButton,
    ) -> bool {
        let Some(point) = self.pointer() else {
            return false;
        };
        if state == ElementState::Released {
            return self.release_strip_press(button, point);
        }
        if let Some(edge) = self.resize_edge_at(point) {
            if button == MouseButton::Left
                && let Some(view) = &self.view
            {
                let _ = view.window.drag_resize_window(edge);
            }
            return true;
        }
        if point.y >= STRIP_HEIGHT {
            return false;
        }
        let Some(geometry) = self.strip_geometry() else {
            return false;
        };
        let Some(part) = geometry.part_at(point) else {
            return false;
        };
        match (part, button) {
            (TabStripPart::Empty, MouseButton::Left) => {
                let now = Instant::now();
                if self
                    .strip
                    .last_empty_press
                    .take()
                    .is_some_and(|last| now.duration_since(last) <= DOUBLE_CLICK)
                {
                    self.toggle_maximized();
                } else {
                    self.strip.last_empty_press = Some(now);
                    if let Some(view) = &self.view {
                        let _ = view.window.drag_window();
                    }
                }
            }
            (TabStripPart::Empty, MouseButton::Right) => {
                if let Some(view) = &self.view {
                    view.window.show_window_menu(LogicalPosition::new(
                        f64::from(point.x),
                        f64::from(point.y),
                    ));
                }
            }
            (TabStripPart::Tab(index), MouseButton::Left) => {
                self.activate(index);
                if let Some(network) = self.networks.get(index) {
                    self.strip.drag = Some(TabDrag {
                        id: network.id,
                        origin: point.x,
                        offset: 0.0,
                        moved: false,
                    });
                }
                self.strip.pressed = Some((part, button));
            }
            (TabStripPart::ScrollBack | TabStripPart::ScrollForward, MouseButton::Left) => {
                self.strip.scroll = geometry.step(part == TabStripPart::ScrollBack);
                self.strip.pressed = Some((part, button));
            }
            (_, MouseButton::Left | MouseButton::Middle) => {
                self.strip.pressed = Some((part, button));
            }
            _ => {}
        }
        self.strip.hovered = Some(part);
        self.mark_strip();
        true
    }

    fn release_strip_press(&mut self, button: MouseButton, point: Point) -> bool {
        let Some((part, pressed_button)) = self.strip.pressed else {
            return false;
        };
        if pressed_button != button {
            return true;
        }
        self.strip.pressed = None;
        let drag = self.strip.drag.take();
        self.mark_strip();
        if drag.is_some_and(|drag| drag.moved) {
            return true;
        }
        let released_on = self
            .strip_geometry()
            .and_then(|geometry| geometry.part_at(point));
        if released_on != Some(part) {
            return true;
        }
        match (part, button) {
            (TabStripPart::CloseTab(index) | TabStripPart::Tab(index), MouseButton::Middle)
            | (TabStripPart::CloseTab(index), MouseButton::Left) => self.close_network(index),
            (TabStripPart::NewTab, MouseButton::Left | MouseButton::Middle) => self.open_prompt(),
            (TabStripPart::Minimize, MouseButton::Left) => {
                if let Some(view) = &self.view {
                    view.window.set_minimized(true);
                }
            }
            (TabStripPart::Maximize, MouseButton::Left) => self.toggle_maximized(),
            (TabStripPart::Close, MouseButton::Left) => self.close_window(),
            _ => {}
        }
        true
    }

    fn toggle_maximized(&mut self) {
        if let Some(view) = &self.view {
            view.window.set_maximized(!view.window.is_maximized());
        }
        self.mark_strip();
    }

    /// Moves a dragged tab to the slot its center is over.
    fn reorder_dragged_tab(&mut self) {
        let Some(drag) = &self.strip.drag else {
            return;
        };
        let (id, offset) = (drag.id, drag.offset);
        let (Some(geometry), Some(from)) = (self.strip_geometry(), self.index_of(id)) else {
            return;
        };
        let slot = geometry.tabs[from];
        let to = geometry.drop_index(slot.x + offset + slot.width / 2.0);
        if to == from {
            return;
        }
        let moved = self.networks.remove(from);
        self.networks.insert(to, moved);
        self.active = shifted_index(self.active, from, to);
        // The tab keeps its place under the pointer: its new slot is that
        // many widths away from the old one.
        let shift = (to as f32 - from as f32) * slot.width;
        if let Some(drag) = &mut self.strip.drag {
            drag.origin += shift;
            drag.offset -= shift;
        }
    }

    /// The wheel over the strip scrolls tabs that overflow it.
    pub(super) fn chrome_scroll(&mut self, delta: MouseScrollDelta) -> bool {
        let (Some(point), Some(geometry)) = (self.pointer(), self.strip_geometry()) else {
            return false;
        };
        if point.y >= STRIP_HEIGHT {
            return false;
        }
        if geometry.overflows() {
            let amount = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    (if x.abs() > y.abs() { x } else { -y }) * 60.0
                }
                MouseScrollDelta::PixelDelta(position) => {
                    let scale = f64::from(self.view.as_ref().map_or(1.0, |view| view.scale));
                    (if position.x.abs() > position.y.abs() {
                        -position.x
                    } else {
                        -position.y
                    } / scale) as f32
                }
            };
            self.strip.scroll = (geometry.scroll + amount).clamp(0.0, geometry.max_scroll);
            self.mark_strip();
        }
        true
    }

    pub(super) fn set_cursor(&mut self, icon: CursorIcon) {
        if let Some(view) = &mut self.view
            && view.cursor != icon
        {
            view.cursor = icon;
            view.window.set_cursor(icon);
        }
    }
}

/// Where the item at `index` ends up after the item at `from` moves to `to`.
fn shifted_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && to >= index {
        index - 1
    } else if from > index && to <= index {
        index + 1
    } else {
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_one_item_shifts_the_ones_between() {
        // [a b c d]: b (1) moves to 3 → [a c d b].
        assert_eq!(shifted_index(1, 1, 3), 3);
        assert_eq!(shifted_index(2, 1, 3), 1);
        assert_eq!(shifted_index(3, 1, 3), 2);
        assert_eq!(shifted_index(0, 1, 3), 0);
        // d (3) moves to 0 → [d a b c].
        assert_eq!(shifted_index(0, 3, 0), 1);
        assert_eq!(shifted_index(2, 3, 0), 3);
    }
}
