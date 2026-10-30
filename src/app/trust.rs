//! The certificate trust prompt's input: it is modal while open, and its
//! buttons answer it (DESIGN.md §6.2).

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::ui::trust::{TrustAction, TrustLayout};

use super::App;

/// The order Tab and the arrows move through.
const ORDER: [TrustAction; 3] = [TrustAction::Once, TrustAction::Always, TrustAction::Cancel];

impl App {
    pub(super) fn trust_layout(&self) -> Option<TrustLayout> {
        self.trust.as_ref()?;
        Some(TrustLayout::new(self.regions()?.body))
    }

    fn answer(&mut self, action: TrustAction) {
        self.trust_focus = TrustAction::Cancel;
        self.trust_hover = None;
        self.answer_certificate(match action {
            TrustAction::Once => Some(false),
            TrustAction::Always => Some(true),
            TrustAction::Cancel => None,
        });
    }

    fn trust_changed(&mut self) {
        if let Some(layout) = self.trust_layout() {
            self.mark_under(layout.damage());
        }
    }

    pub(super) fn trust_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let shift = self.modifiers.shift_key();
        let position = ORDER
            .iter()
            .position(|action| *action == self.trust_focus)
            .unwrap_or(2) as isize;
        let step = |step: isize| ORDER[(position + step).rem_euclid(ORDER.len() as isize) as usize];
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.answer(TrustAction::Cancel),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.answer(self.trust_focus)
            }
            Key::Named(NamedKey::Tab) => {
                self.trust_focus = step(if shift { -1 } else { 1 });
                self.trust_changed();
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.trust_focus = step(-1);
                self.trust_changed();
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.trust_focus = step(1);
                self.trust_changed();
            }
            Key::Character(text)
                if self.modifiers.control_key() && text.eq_ignore_ascii_case("q") =>
            {
                self.quit = true
            }
            _ => {}
        }
    }

    pub(super) fn trust_pointer_moved(&mut self) {
        let point = self.view.as_ref().map(|view| view.pointer);
        let hit = match (point, self.trust_layout()) {
            (Some(point), Some(layout)) => layout.hit(point),
            _ => None,
        };
        if hit != self.trust_hover {
            self.trust_hover = hit;
            self.trust_changed();
        }
        self.set_cursor(if hit.is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    pub(super) fn trust_pointer_pressed(&mut self) {
        let point = self.view.as_ref().map(|view| view.pointer);
        if let (Some(point), Some(layout)) = (point, self.trust_layout())
            && let Some(action) = layout.hit(point)
        {
            self.answer(action);
        }
    }
}
