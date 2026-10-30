//! A strict subset of TOML 1.0 for RubyChat's own files (DESIGN.md §11):
//! comments, `[table]` and `[[array of tables]]` headers with dotted keys,
//! `key = value` lines, and values that are basic or literal strings,
//! integers, booleans, or arrays of them (which may span lines). Anything
//! else—floats, dates, inline tables, multi-line strings—is an error with its
//! line and column, so a hand-edited file never half-loads silently.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Boolean(bool),
    Array(Vec<Value>),
    Table(Table),
    /// An array of tables, from `[[name]]` headers.
    Tables(Vec<Table>),
}

/// Keys and values in the order they appeared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    entries: Vec<(String, Value)>,
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Sets `key`, replacing any value it had.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        match self.get_mut(&key) {
            Some(slot) => *slot = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    pub fn integer(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Value::Integer(number)) => Some(*number),
            _ => None,
        }
    }

    pub fn boolean(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Boolean(flag)) => Some(*flag),
            _ => None,
        }
    }

    /// The tables of `[[key]]`, or none.
    pub fn tables(&self, key: &str) -> &[Table] {
        match self.get(key) {
            Some(Value::Tables(tables)) => tables,
            _ => &[],
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        &self.entries
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for Error {}

/// Files larger than this are refused rather than parsed.
const MAX_BYTES: usize = 4 << 20;
/// Arrays nest at most this deep.
const MAX_DEPTH: usize = 16;

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    line: usize,
    line_start: usize,
}

type Result<T> = std::result::Result<T, Error>;

impl<'a> Parser<'a> {
    fn error<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(Error {
            line: self.line,
            column: self.text[self.line_start..self.pos].chars().count() + 1,
            message: message.into(),
        })
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.line_start = self.pos;
        }
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.bump();
            }
        }
    }

    /// Spaces, comments and line breaks, as inside arrays.
    fn skip_blank(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            if self.eat('\r') && !self.eat('\n') {
                return;
            }
            if !self.eat('\n') {
                return;
            }
        }
    }

    /// The end of a line: optional spaces and comment, then a line break or
    /// the end of the file.
    fn end_of_line(&mut self) -> Result<()> {
        self.skip_spaces();
        self.skip_comment();
        match self.peek() {
            None => Ok(()),
            Some('\n') => {
                self.bump();
                Ok(())
            }
            Some('\r') => {
                self.bump();
                if self.eat('\n') {
                    Ok(())
                } else {
                    self.error("expected a line break after \\r")
                }
            }
            Some(c) => self.error(format!("unexpected {c:?}")),
        }
    }

    fn key(&mut self) -> Result<String> {
        match self.peek() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-' => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    self.bump();
                }
                Ok(self.text[start..self.pos].to_string())
            }
            _ => self.error("expected a key"),
        }
    }

    /// `a.b."c d"` in a header.
    fn key_path(&mut self) -> Result<Vec<String>> {
        let mut path = Vec::new();
        loop {
            self.skip_spaces();
            path.push(self.key()?);
            self.skip_spaces();
            if !self.eat('.') {
                return Ok(path);
            }
        }
    }

    fn basic_string(&mut self) -> Result<String> {
        self.bump();
        if self.text[self.pos..].starts_with("\"\"") {
            return self.error("multi-line strings are not supported");
        }
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return self.error("unterminated string"),
                Some('"') => return Ok(out),
                Some('\\') => {
                    let escaped = match self.bump() {
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('b') => '\u{8}',
                        Some('f') => '\u{c}',
                        Some(kind @ ('u' | 'U')) => {
                            let digits = if kind == 'u' { 4 } else { 8 };
                            let hex = self.text.get(self.pos..self.pos + digits).unwrap_or("");
                            let code = (hex.len() == digits
                                && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                            .then(|| u32::from_str_radix(hex, 16).ok())
                            .flatten()
                            .and_then(char::from_u32);
                            let Some(c) = code else {
                                return self.error("invalid unicode escape");
                            };
                            for _ in 0..digits {
                                self.bump();
                            }
                            c
                        }
                        _ => return self.error("invalid escape"),
                    };
                    out.push(escaped);
                }
                Some(c) if c.is_control() && c != '\t' => {
                    return self.error("control character in string");
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String> {
        self.bump();
        if self.text[self.pos..].starts_with("''") {
            return self.error("multi-line strings are not supported");
        }
        let start = self.pos;
        loop {
            match self.bump() {
                None | Some('\n') => return self.error("unterminated string"),
                Some('\'') => return Ok(self.text[start..self.pos - 1].to_string()),
                Some(c) if c.is_control() && c != '\t' => {
                    return self.error("control character in string");
                }
                Some(_) => {}
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        match self.peek() {
            Some('"') => self.basic_string().map(Value::String),
            Some('\'') => self.literal_string().map(Value::String),
            Some('[') => {
                if depth >= MAX_DEPTH {
                    return self.error("arrays nest too deeply");
                }
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_blank();
                    if self.eat(']') {
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value(depth + 1)?);
                    self.skip_blank();
                    if self.eat(']') {
                        return Ok(Value::Array(items));
                    }
                    if !self.eat(',') {
                        return self.error("expected , or ] in array");
                    }
                }
            }
            Some('{') => self.error("inline tables are not supported"),
            Some(_) => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.' | ':'))
                {
                    self.bump();
                }
                let word = &self.text[start..self.pos];
                match word {
                    "true" => Ok(Value::Boolean(true)),
                    "false" => Ok(Value::Boolean(false)),
                    "" => self.error("expected a value"),
                    _ => match integer(word) {
                        Some(number) => Ok(Value::Integer(number)),
                        None => self.error(format!("unsupported value {word:?}")),
                    },
                }
            }
            None => self.error("expected a value"),
        }
    }
}

/// A decimal integer: an optional sign, then digits with single underscores
/// between them and no leading zero.
fn integer(word: &str) -> Option<i64> {
    let digits = word.strip_prefix(['+', '-']).unwrap_or(word);
    let valid = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && (digits == "0" || !digits.starts_with('0'));
    if !valid {
        return None;
    }
    word.replace('_', "").parse().ok()
}

/// The table at `path` from `root`, descending into the last table of an
/// array of tables (TOML's rule for headers after `[[...]]`).
fn table_at<'t>(
    root: &'t mut Table,
    path: &[String],
) -> std::result::Result<&'t mut Table, String> {
    let mut table = root;
    for key in path {
        if table.get(key).is_none() {
            table.insert(key.clone(), Value::Table(Table::default()));
        }
        table = match table.get_mut(key) {
            Some(Value::Table(inner)) => inner,
            Some(Value::Tables(tables)) => {
                tables.last_mut().ok_or_else(|| format!("{key} is empty"))?
            }
            _ => return Err(format!("{key} is already a value")),
        };
    }
    Ok(table)
}

pub fn parse(text: &str) -> std::result::Result<Table, Error> {
    if text.len() > MAX_BYTES {
        return Err(Error {
            line: 1,
            column: 1,
            message: String::from("file too large"),
        });
    }
    let mut parser = Parser {
        text,
        pos: 0,
        line: 1,
        line_start: 0,
    };
    let mut root = Table::default();
    let mut current: Vec<String> = Vec::new();
    let mut defined: Vec<Vec<String>> = Vec::new();
    loop {
        parser.skip_spaces();
        match parser.peek() {
            None => return Ok(root),
            Some('#' | '\n' | '\r') => parser.end_of_line()?,
            Some('[') => {
                parser.bump();
                let array = parser.eat('[');
                let path = parser.key_path()?;
                if !parser.eat(']') || (array && !parser.eat(']')) {
                    return parser.error(if array { "expected ]]" } else { "expected ]" });
                }
                parser.end_of_line()?;
                let (last, parent) = path.split_last().expect("a key path has a key");
                let parent = match table_at(&mut root, parent) {
                    Ok(parent) => parent,
                    Err(message) => return parser.error(message),
                };
                if array {
                    match parent.get_mut(last) {
                        None => parent.insert(last.clone(), Value::Tables(vec![Table::default()])),
                        Some(Value::Tables(tables)) => tables.push(Table::default()),
                        Some(_) => {
                            return parser.error(format!("{last} is not an array of tables"));
                        }
                    }
                    // The new element's sub-tables may be defined afresh.
                    defined.retain(|done| !done.starts_with(&path));
                } else {
                    if defined.contains(&path) {
                        return parser.error(format!("[{}] is defined twice", path.join(".")));
                    }
                    match parent.get(last) {
                        None => parent.insert(last.clone(), Value::Table(Table::default())),
                        // Created implicitly by a longer header; defined now.
                        Some(Value::Table(_)) => {}
                        Some(Value::Tables(_)) => {
                            return parser.error(format!("{last} is an array of tables"));
                        }
                        Some(_) => return parser.error(format!("{last} is already a value")),
                    }
                    defined.push(path.clone());
                }
                current = path;
            }
            Some(_) => {
                let key = parser.key()?;
                parser.skip_spaces();
                if parser.peek() == Some('.') {
                    return parser.error("dotted keys are only supported in headers");
                }
                if !parser.eat('=') {
                    return parser.error("expected =");
                }
                parser.skip_spaces();
                let value = parser.value(0)?;
                parser.end_of_line()?;
                let table = match table_at(&mut root, &current) {
                    Ok(table) => table,
                    Err(message) => return parser.error(message),
                };
                if table.get(&key).is_some() {
                    return parser.error(format!("{key} is defined twice"));
                }
                table.insert(key, value);
            }
        }
    }
}

fn write_key(out: &mut String, key: &str) {
    if !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        out.push_str(key);
    } else {
        write_string(out, key);
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::String(text) => write_string(out, text),
        Value::Integer(number) => out.push_str(&number.to_string()),
        Value::Boolean(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Table(_) | Value::Tables(_) => unreachable!("tables are written as sections"),
    }
}

fn write_table(out: &mut String, table: &Table, path: &mut Vec<String>) {
    for (key, value) in &table.entries {
        if !matches!(value, Value::Table(_) | Value::Tables(_)) {
            write_key(out, key);
            out.push_str(" = ");
            write_value(out, value);
            out.push('\n');
        }
    }
    for (key, value) in &table.entries {
        path.push(key.clone());
        let header = |out: &mut String, brackets: (&str, &str), path: &[String]| {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(brackets.0);
            for (index, part) in path.iter().enumerate() {
                if index > 0 {
                    out.push('.');
                }
                write_key(out, part);
            }
            out.push_str(brackets.1);
            out.push('\n');
        };
        match value {
            Value::Table(inner) => {
                header(out, ("[", "]"), path);
                write_table(out, inner, path);
            }
            Value::Tables(tables) => {
                for inner in tables {
                    header(out, ("[[", "]]"), path);
                    write_table(out, inner, path);
                }
            }
            _ => {}
        }
        path.pop();
    }
}

/// The table as TOML that `parse` reads back to an equal table, prefixed by
/// `comment` lines.
pub fn to_string(table: &Table, comment: &str) -> String {
    let mut out = String::new();
    for line in comment.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    write_table(&mut out, table, &mut Vec::new());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# networks.toml
title = "RubyChat"   # a comment
[[network]]
name = "Libera.Chat"
connect_at_startup = true
alt_nicks = [ "ruby_", 'ruby\x', # comments in arrays
  "ruby__",
]
port = 6_697

[[network.server]]
host = "irc.libera.chat"
tls = true

[network.login]
method = "sasl-scram-sha-256"
note = "tab\tquote\" \u00e9 \U0001F496"

[[network]]
name = "OFTC"
"#;

    #[test]
    fn parses_the_subset() {
        let root = parse(SAMPLE).unwrap();
        assert_eq!(root.string("title"), Some("RubyChat"));
        let networks = root.tables("network");
        assert_eq!(networks.len(), 2);
        let libera = &networks[0];
        assert_eq!(libera.boolean("connect_at_startup"), Some(true));
        assert_eq!(libera.integer("port"), Some(6697));
        assert_eq!(
            libera.get("alt_nicks"),
            Some(&Value::Array(vec![
                Value::String("ruby_".into()),
                Value::String("ruby\\x".into()),
                Value::String("ruby__".into()),
            ]))
        );
        assert_eq!(
            libera.tables("server")[0].string("host"),
            Some("irc.libera.chat")
        );
        let Some(Value::Table(login)) = libera.get("login") else {
            panic!("login is a table");
        };
        assert_eq!(login.string("note"), Some("tab\tquote\" \u{e9} \u{1f496}"));
        assert_eq!(networks[1].string("name"), Some("OFTC"));
    }

    #[test]
    fn writing_round_trips() {
        let root = parse(SAMPLE).unwrap();
        let text = to_string(&root, "Written by RubyChat.\nEdit freely.");
        assert!(text.starts_with("# Written by RubyChat.\n# Edit freely.\n"));
        assert_eq!(parse(&text).unwrap(), root);
    }

    #[test]
    fn errors_name_their_line_and_column() {
        let cases = [
            ("a = 1\na = 2", 2, "defined twice"),
            ("[t]\n[t]", 2, "defined twice"),
            ("x = 1.5", 1, "unsupported value"),
            ("x = 007", 1, "unsupported value"),
            ("x = {a = 1}", 1, "inline tables"),
            ("x = \"open", 1, "unterminated"),
            ("x = \"\\q\"", 1, "invalid escape"),
            ("a.b = 1", 1, "dotted keys"),
            ("x = 1 y", 1, "unexpected"),
            ("x = [1, 2", 1, "expected"),
            ("[[a]]\n[a.b]\nk = 1\n[a]", 4, "array of tables"),
            ("[[a]]\n[a.b]\n[[a]]\n[a.b]\n[a.b]", 5, "defined twice"),
            ("a = 1\n[a]", 2, "already a value"),
        ];
        for (text, line, message) in cases {
            let error = parse(text).unwrap_err();
            assert_eq!(error.line, line, "{text:?}: {error}");
            assert!(error.message.contains(message), "{text:?}: {error}");
        }
        assert_eq!(parse("x = 1 y").unwrap_err().column, 7);
        assert_eq!(parse("").unwrap(), Table::default());
        assert_eq!(parse("x = -0").unwrap().integer("x"), Some(0));
    }
}
