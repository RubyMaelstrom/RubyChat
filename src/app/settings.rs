//! Settings (DESIGN.md §8.10): applied as they load and change, and saved
//! as they change. `/set` lists and changes them, as does the settings
//! panel.

use crate::config::settings::{Key, Settings};
use crate::model::LineKind;
use crate::store::Document;
use crate::ui::chat_view::ChatFrame;

use super::App;

impl App {
    /// Puts `settings` in force everywhere they matter.
    pub(super) fn apply_settings(&mut self, settings: Settings) {
        self.smart_filter = settings.smart_filter;
        for network in &mut self.networks {
            network.state.set_highlight_words(&settings.highlight_words);
        }
        if settings.fallback_fonts != self.settings.fallback_fonts {
            self.text
                .set_preferred_fonts(settings.fallback_fonts.clone());
        }
        self.settings = settings;
        // Columns and highlights may have changed.
        self.chat_frame = ChatFrame::default();
        self.mark_all();
    }

    pub(super) fn save_settings(&self) {
        if let Some(store) = &self.store {
            store.save(Document::Settings, self.settings.to_toml());
        }
    }

    /// `/set` lists the settings; `/set name` shows one, or flips a
    /// switch; `/set name value` changes one (`-` empties a list).
    pub(super) fn set_command(&mut self, args: &str) {
        let (name, value) = args
            .trim()
            .split_once(char::is_whitespace)
            .map_or((args.trim(), ""), |(name, value)| (name, value.trim()));
        if name.is_empty() {
            for key in Key::ALL {
                let line = format!(
                    "{} = {} · {}",
                    key.name(),
                    self.settings.value(key),
                    key.description()
                );
                self.say(LineKind::Status, "•", line);
            }
            return;
        }
        let Some(key) = Key::from_name(name) else {
            return self.say(
                LineKind::Error,
                "!",
                format!("No setting is called {name}; /set lists them."),
            );
        };
        if value.is_empty() && !key.is_switch() {
            let line = format!("{} = {}", key.name(), self.settings.value(key));
            return self.say(LineKind::Status, "•", line);
        }
        let value = if value == "-" { "" } else { value };
        self.change_setting(key, value);
    }

    /// Changes one setting, saying what it is now.
    pub(super) fn change_setting(&mut self, key: Key, value: &str) {
        let mut settings = self.settings.clone();
        match settings.set(key, value) {
            Ok(()) => {
                self.apply_settings(settings);
                self.save_settings();
                let line = format!("{} = {}", key.name(), self.settings.value(key));
                self.say(LineKind::Status, "•", line);
            }
            Err(error) => self.say(LineKind::Error, "!", error),
        }
    }
}

/// The settings panel and what's being edited in the input row.
#[derive(Default)]
pub(super) struct SettingsPanel {
    pub selected: usize,
    /// The words setting being edited, in `editor`.
    pub editing: Option<Key>,
    pub editor: crate::ui::input::LineEditor,
}

impl App {
    /// `/settings`: opens the settings panel over the input row.
    pub(super) fn open_settings(&mut self) {
        if self.welcome_visible() {
            return;
        }
        self.close_menu();
        self.close_find();
        self.close_channels();
        self.close_switcher();
        self.settings_panel = Some(SettingsPanel::default());
        self.mark_under_settings();
        self.mark_input();
    }

    pub(super) fn close_settings(&mut self) {
        if self.settings_panel.is_some() {
            self.mark_under_settings();
            self.settings_panel = None;
            self.mark_input();
        }
    }

    pub(super) fn settings_geometry(&self) -> Option<crate::ui::settings::SettingsGeometry> {
        self.settings_panel.as_ref()?;
        let regions = self.regions()?;
        Some(crate::ui::settings::SettingsGeometry::new(
            regions.input.inset(12.0, 8.0),
            regions.body,
        ))
    }

    fn mark_under_settings(&mut self) {
        if let Some(geometry) = self.settings_geometry() {
            self.mark_under(geometry.damage());
        }
    }

    /// Keys while the panel is open; returns whether the key was used.
    pub(super) fn settings_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key as K, NamedKey};
        let Some(panel) = &mut self.settings_panel else {
            return false;
        };
        let selected = Key::ALL[panel.selected];
        match (key, panel.editing) {
            (K::Named(NamedKey::Escape), Some(_)) => {
                panel.editing = None;
                panel.editor = Default::default();
                self.mark_under_settings();
                self.mark_input();
            }
            (K::Named(NamedKey::Escape), None) => self.close_settings(),
            (K::Named(NamedKey::Enter), Some(editing)) => {
                let text = panel.editor.text().to_string();
                panel.editing = None;
                panel.editor = Default::default();
                self.change_setting(editing, &text);
                self.mark_under_settings();
                self.mark_input();
            }
            (K::Named(NamedKey::ArrowUp | NamedKey::ArrowDown), None) => {
                let step = if matches!(key, K::Named(NamedKey::ArrowDown)) {
                    1
                } else {
                    Key::ALL.len() - 1
                };
                panel.selected = (panel.selected + step) % Key::ALL.len();
                self.mark_under_settings();
            }
            (
                K::Named(
                    NamedKey::Enter | NamedKey::Space | NamedKey::ArrowLeft | NamedKey::ArrowRight,
                ),
                None,
            ) => self.activate_setting(selected),
            // Typing goes to the setting being edited, and nowhere else.
            (_, Some(_)) => return false,
            _ => {}
        }
        true
    }

    /// Enter on a setting: a switch flips; words start editing below.
    pub(super) fn activate_setting(&mut self, key: Key) {
        if key.is_switch() {
            self.change_setting(key, "toggle");
            self.mark_under_settings();
            return;
        }
        let value = match key {
            Key::HighlightWords => self.settings.highlight_words.join(", "),
            Key::FallbackFonts => self.settings.fallback_fonts.join(", "),
            _ => self.settings.link_command.clone(),
        };
        if let Some(panel) = &mut self.settings_panel {
            panel.editing = Some(key);
            panel.editor.set(&value);
        }
        self.mark_under_settings();
        self.mark_input();
    }

    /// A press while the panel is open: a row changes its setting; a press
    /// elsewhere closes the panel. Returns whether the press was used.
    pub(super) fn settings_pointer_pressed(&mut self, point: crate::render::Point) -> bool {
        let Some(geometry) = self.settings_geometry() else {
            return false;
        };
        if geometry.panel.contains(point) {
            if let Some(row) = geometry.row_at(point) {
                if let Some(panel) = &mut self.settings_panel {
                    panel.selected = row;
                }
                self.activate_setting(Key::ALL[row]);
            }
            return true;
        }
        self.close_settings();
        false
    }
}
