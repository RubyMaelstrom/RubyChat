//! Settings (`settings.toml`, in the configuration directory; DESIGN.md
//! §8.10, §11): how RubyChat behaves across networks. `/set` and the
//! settings panel change them; a setting left out has its default.

use super::toml::{self, Table, Value};

/// A setting's name, as `/set` and `settings.toml` spell it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    SmartFilter,
    Seconds,
    HighlightWords,
    Notifications,
    CloseToTray,
    Logging,
    FallbackFonts,
    LinkCommand,
}

impl Key {
    pub const ALL: [Key; 8] = [
        Key::SmartFilter,
        Key::Seconds,
        Key::HighlightWords,
        Key::Notifications,
        Key::CloseToTray,
        Key::Logging,
        Key::FallbackFonts,
        Key::LinkCommand,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Key::SmartFilter => "smart_filter",
            Key::Seconds => "seconds",
            Key::HighlightWords => "highlight_words",
            Key::Notifications => "notifications",
            Key::CloseToTray => "close_to_tray",
            Key::Logging => "logging",
            Key::FallbackFonts => "fallback_fonts",
            Key::LinkCommand => "link_command",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|key| key.name().eq_ignore_ascii_case(name.trim()))
    }

    /// What it does, for the panel and `/set`.
    pub fn description(self) -> &'static str {
        match self {
            Key::SmartFilter => "Hide joins, parts and quits of people who haven't spoken lately",
            Key::Seconds => "Show seconds in the time column",
            Key::HighlightWords => "Words that highlight a line, besides your nick",
            Key::Notifications => "Notify of highlights and private messages while away",
            Key::CloseToTray => "Closing the window leaves RubyChat in the tray",
            Key::Logging => "Keep chat logs on disk",
            Key::FallbackFonts => "Font families tried first for what the chat face lacks",
            Key::LinkCommand => "A command to open links with, instead of the system's",
        }
    }

    /// Whether it's on or off, rather than words.
    pub fn is_switch(self) -> bool {
        !matches!(
            self,
            Key::HighlightWords | Key::FallbackFonts | Key::LinkCommand
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub smart_filter: bool,
    pub seconds: bool,
    pub highlight_words: Vec<String>,
    pub notifications: bool,
    pub close_to_tray: bool,
    pub logging: bool,
    /// Empty: the default order for the locale.
    pub fallback_fonts: Vec<String>,
    /// Empty: the system's handler.
    pub link_command: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            smart_filter: true,
            seconds: false,
            highlight_words: Vec::new(),
            notifications: true,
            close_to_tray: true,
            logging: true,
            fallback_fonts: Vec::new(),
            link_command: String::new(),
        }
    }
}

/// Words from `text`: separated by commas, or by spaces when there's no
/// comma, as `/set` takes them.
fn words(text: &str) -> Vec<String> {
    let separated: Vec<&str> = if text.contains(',') {
        text.split(',').collect()
    } else {
        text.split_whitespace().collect()
    };
    separated
        .into_iter()
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(String::from)
        .collect()
}

fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    match value? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text.clone()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

impl Settings {
    /// Reads `settings.toml`; what's missing or of the wrong type keeps its
    /// default.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root = toml::parse(text).map_err(|error| format!("settings.toml: {error}"))?;
        let defaults = Self::default();
        Ok(Self {
            smart_filter: root
                .boolean("smart_filter")
                .unwrap_or(defaults.smart_filter),
            seconds: root.boolean("seconds").unwrap_or(defaults.seconds),
            highlight_words: strings(root.get("highlight_words")).unwrap_or_default(),
            notifications: root
                .boolean("notifications")
                .unwrap_or(defaults.notifications),
            close_to_tray: root
                .boolean("close_to_tray")
                .unwrap_or(defaults.close_to_tray),
            logging: root.boolean("logging").unwrap_or(defaults.logging),
            fallback_fonts: strings(root.get("fallback_fonts")).unwrap_or_default(),
            link_command: root.string("link_command").unwrap_or("").to_string(),
        })
    }

    pub fn to_toml(&self) -> String {
        let mut root = Table::default();
        let list =
            |items: &[String]| Value::Array(items.iter().cloned().map(Value::String).collect());
        root.insert("smart_filter", Value::Boolean(self.smart_filter));
        root.insert("seconds", Value::Boolean(self.seconds));
        root.insert("highlight_words", list(&self.highlight_words));
        root.insert("notifications", Value::Boolean(self.notifications));
        root.insert("close_to_tray", Value::Boolean(self.close_to_tray));
        root.insert("logging", Value::Boolean(self.logging));
        root.insert("fallback_fonts", list(&self.fallback_fonts));
        root.insert("link_command", Value::String(self.link_command.clone()));
        toml::to_string(
            &root,
            "RubyChat's settings; /set and /settings change them.",
        )
    }

    /// A setting's value as `/set` shows it.
    pub fn value(&self, key: Key) -> String {
        let switch = |on: bool| String::from(if on { "on" } else { "off" });
        let list = |items: &[String], empty: &str| {
            if items.is_empty() {
                empty.to_string()
            } else {
                items.join(", ")
            }
        };
        match key {
            Key::SmartFilter => switch(self.smart_filter),
            Key::Seconds => switch(self.seconds),
            Key::HighlightWords => list(&self.highlight_words, "none"),
            Key::Notifications => switch(self.notifications),
            Key::CloseToTray => switch(self.close_to_tray),
            Key::Logging => switch(self.logging),
            Key::FallbackFonts => list(&self.fallback_fonts, "the default order"),
            Key::LinkCommand => {
                if self.link_command.is_empty() {
                    String::from("the system's handler")
                } else {
                    self.link_command.clone()
                }
            }
        }
    }

    /// Sets `key` from `text` as typed: on or off (also true, false, yes,
    /// no, 1, 0; toggle flips it), words separated by commas, or a command.
    pub fn set(&mut self, key: Key, text: &str) -> Result<(), String> {
        let text = text.trim();
        if key.is_switch() {
            let current = self.switch(key);
            let on = match text.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "toggle" | "" => !current,
                _ => return Err(format!("{} is on or off", key.name())),
            };
            match key {
                Key::SmartFilter => self.smart_filter = on,
                Key::Seconds => self.seconds = on,
                Key::Notifications => self.notifications = on,
                Key::CloseToTray => self.close_to_tray = on,
                Key::Logging => self.logging = on,
                _ => unreachable!("switches only"),
            }
            return Ok(());
        }
        match key {
            Key::HighlightWords => self.highlight_words = words(text),
            Key::FallbackFonts => self.fallback_fonts = words_by_comma(text),
            Key::LinkCommand => {
                if text.chars().any(char::is_control) {
                    return Err(String::from("A command is one line"));
                }
                self.link_command = text.to_string();
            }
            _ => unreachable!("words only"),
        }
        Ok(())
    }

    /// A switch's state.
    pub fn switch(&self, key: Key) -> bool {
        match key {
            Key::SmartFilter => self.smart_filter,
            Key::Seconds => self.seconds,
            Key::Notifications => self.notifications,
            Key::CloseToTray => self.close_to_tray,
            Key::Logging => self.logging,
            _ => false,
        }
    }
}

/// Family names, which have spaces in them: separated by commas only.
fn words_by_comma(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_what_is_missing() {
        let mut settings = Settings::default();
        settings.set(Key::Seconds, "on").unwrap();
        settings.set(Key::SmartFilter, "toggle").unwrap();
        settings
            .set(Key::HighlightWords, "rubychat ferris")
            .unwrap();
        settings
            .set(Key::FallbackFonts, "Noto Sans CJK JP, DejaVu Sans")
            .unwrap();
        settings.set(Key::LinkCommand, "firefox --new-tab").unwrap();
        assert!(settings.seconds && !settings.smart_filter);
        assert_eq!(settings.highlight_words, ["rubychat", "ferris"]);
        assert_eq!(settings.fallback_fonts, ["Noto Sans CJK JP", "DejaVu Sans"]);
        assert_eq!(Settings::parse(&settings.to_toml()).unwrap(), settings);
        let sparse = Settings::parse("seconds = true\nlogging = \"maybe\"\n").unwrap();
        assert!(
            sparse.seconds && sparse.logging,
            "a wrong type keeps the default"
        );
        assert!(Settings::parse("seconds = \n").is_err());
        assert!(settings.set(Key::Logging, "perhaps").is_err());
        assert_eq!(Key::from_name(" Close_To_Tray "), Some(Key::CloseToTray));
        assert_eq!(settings.value(Key::HighlightWords), "rubychat, ferris");
        assert_eq!(
            Settings::default().value(Key::LinkCommand),
            "the system's handler"
        );
        settings.set(Key::HighlightWords, "one, two words").unwrap();
        assert_eq!(settings.highlight_words, ["one", "two words"]);
    }
}
