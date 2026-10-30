//! The D-Bus type system and its wire format (D-Bus Specification, "Type
//! System" and "Marshaling (Wire Format)"). Values are written
//! little-endian, which every receiver must accept; either byte order is
//! read. Reading is strict, as the specification asks of a protocol whose
//! peers may be hostile: padding must be nul, strings valid UTF-8 without
//! nul, booleans 0 or 1, and nesting within the specification's limits.

/// The longest array, in bytes ("Marshalling containers").
pub const MAX_ARRAY: usize = 1 << 26;
/// Nested arrays, and nested structs and dict entries, each up to 32 deep
/// ("Valid Signatures").
const MAX_NESTING: usize = 32;
/// Containers, variants included, up to 64 deep in all.
const MAX_DEPTH: usize = 64;
/// The longest signature.
pub const MAX_SIGNATURE: usize = 255;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Path(String),
    Signature(String),
    /// An array of bytes (`ay`), kept as bytes.
    Bytes(Vec<u8>),
    /// Any other array: its element type's signature, then its elements.
    Array(String, Vec<Value>),
    Struct(Vec<Value>),
    /// A dict entry `{kv}`, only ever an array's element.
    Entry(Box<Value>, Box<Value>),
    Variant(Box<Value>),
    /// UNIX_FD: an index into fds passed beside the message, which
    /// RubyChat never negotiates.
    Fd(u32),
}

impl Value {
    pub fn str(text: impl Into<String>) -> Self {
        Value::Str(text.into())
    }

    pub fn variant(value: Value) -> Self {
        Value::Variant(Box::new(value))
    }

    /// A string-keyed dictionary of variants, `a{sv}`.
    pub fn dict(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Self {
        Value::Array(
            String::from("{sv}"),
            entries
                .into_iter()
                .map(|(key, value)| {
                    Value::Entry(Box::new(Value::str(key)), Box::new(Value::variant(value)))
                })
                .collect(),
        )
    }

    /// An array of strings, `as`.
    pub fn strings<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Self {
        Value::Array(
            String::from("s"),
            items
                .into_iter()
                .map(|item| Value::Str(item.into()))
                .collect(),
        )
    }

    pub fn signature(&self) -> String {
        let mut signature = String::new();
        self.write_signature(&mut signature);
        signature
    }

    fn write_signature(&self, out: &mut String) {
        match self {
            Value::Byte(_) => out.push('y'),
            Value::Bool(_) => out.push('b'),
            Value::I16(_) => out.push('n'),
            Value::U16(_) => out.push('q'),
            Value::I32(_) => out.push('i'),
            Value::U32(_) => out.push('u'),
            Value::I64(_) => out.push('x'),
            Value::U64(_) => out.push('t'),
            Value::F64(_) => out.push('d'),
            Value::Str(_) => out.push('s'),
            Value::Path(_) => out.push('o'),
            Value::Signature(_) => out.push('g'),
            Value::Bytes(_) => out.push_str("ay"),
            Value::Array(element, _) => {
                out.push('a');
                out.push_str(element);
            }
            Value::Struct(fields) => {
                out.push('(');
                for field in fields {
                    field.write_signature(out);
                }
                out.push(')');
            }
            Value::Entry(key, value) => {
                out.push('{');
                key.write_signature(out);
                value.write_signature(out);
                out.push('}');
            }
            Value::Variant(_) => out.push('v'),
            Value::Fd(_) => out.push('h'),
        }
    }

    /// The text of a string, object path or signature.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) | Value::Path(text) | Value::Signature(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::I32(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// An array's elements.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(_, items) => Some(items),
            _ => None,
        }
    }

    /// What a variant holds; other values as they are.
    pub fn unwrap_variant(&self) -> &Value {
        match self {
            Value::Variant(inner) => inner,
            other => other,
        }
    }
}

/// The length of the single complete type that starts `signature`,
/// checking the rules of "Valid Signatures" on the way.
fn complete_type(signature: &[u8], arrays: usize, structs: usize) -> Result<usize, String> {
    match signature.first() {
        None => Err(String::from("signature ends inside a type")),
        Some(code) if is_basic(*code) || *code == b'v' => Ok(1),
        Some(b'a') if arrays == MAX_NESTING => Err(String::from("arrays nested too deeply")),
        Some(b'a') if signature.get(1) == Some(&b'{') => {
            if structs == MAX_NESTING {
                return Err(String::from("dict entries nested too deeply"));
            }
            if !signature.get(2).is_some_and(|code| is_basic(*code)) {
                return Err(String::from("a dict entry's key must be a basic type"));
            }
            let value = complete_type(&signature[3..], arrays + 1, structs + 1)?;
            if signature.get(3 + value) != Some(&b'}') {
                return Err(String::from("a dict entry has exactly two fields"));
            }
            Ok(4 + value)
        }
        Some(b'a') => Ok(1 + complete_type(&signature[1..], arrays + 1, structs)?),
        Some(b'(') => {
            if structs == MAX_NESTING {
                return Err(String::from("structs nested too deeply"));
            }
            let mut length = 1;
            loop {
                match signature.get(length) {
                    None => return Err(String::from("unclosed struct")),
                    Some(b')') if length == 1 => return Err(String::from("empty struct")),
                    Some(b')') => return Ok(length + 1),
                    Some(_) => length += complete_type(&signature[length..], arrays, structs + 1)?,
                }
            }
        }
        Some(b'{') => Err(String::from("a dict entry outside an array")),
        Some(code) => Err(format!("unknown type code {:?}", char::from(*code))),
    }
}

fn is_basic(code: u8) -> bool {
    matches!(
        code,
        b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o' | b'g' | b'h'
    )
}

/// Splits a signature into its single complete types.
pub fn split_signature(signature: &str) -> Result<Vec<&str>, String> {
    if signature.len() > MAX_SIGNATURE {
        return Err(String::from("signature longer than 255 bytes"));
    }
    let bytes = signature.as_bytes();
    let mut types = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let length = complete_type(&bytes[at..], 0, 0)?;
        types.push(&signature[at..at + length]);
        at += length;
    }
    Ok(types)
}

/// One single complete type, as a variant's signature must be.
fn single_type(signature: &str) -> Result<(), String> {
    match split_signature(signature)?.as_slice() {
        [_] => Ok(()),
        _ => Err(format!("{signature:?} is not one complete type")),
    }
}

fn alignment(code: u8) -> usize {
    match code {
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b'h' | b's' | b'o' | b'a' => 4,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 1,
    }
}

/// "Valid Object Paths": `/`, or `/`-separated non-empty elements of
/// `[A-Za-z0-9_]`.
pub fn valid_path(path: &str) -> bool {
    path == "/"
        || path.strip_prefix('/').is_some_and(|rest| {
            rest.split('/').all(|element| {
                !element.is_empty()
                    && element
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        })
}

/// Values in the wire format, always little-endian, from the start of a
/// message (alignment counts from there).
#[derive(Default)]
pub struct Writer {
    pub bytes: Vec<u8>,
}

impl Writer {
    fn pad(&mut self, align: usize) {
        while !self.bytes.len().is_multiple_of(align) {
            self.bytes.push(0);
        }
    }

    pub fn u32(&mut self, n: u32) {
        self.pad(4);
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        if text.contains('\0') {
            return Err(String::from("a string contains nul"));
        }
        self.u32(text.len() as u32);
        self.bytes.extend_from_slice(text.as_bytes());
        self.bytes.push(0);
        Ok(())
    }

    pub fn value(&mut self, value: &Value) -> Result<(), String> {
        self.value_at(value, 0)
    }

    fn value_at(&mut self, value: &Value, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(String::from("values nested too deeply"));
        }
        match value {
            Value::Byte(n) => self.bytes.push(*n),
            Value::Bool(b) => self.u32(u32::from(*b)),
            Value::I16(n) => {
                self.pad(2);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::U16(n) => {
                self.pad(2);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::I32(n) => {
                self.pad(4);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::U32(n) | Value::Fd(n) => self.u32(*n),
            Value::I64(n) => {
                self.pad(8);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::U64(n) => {
                self.pad(8);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::F64(n) => {
                self.pad(8);
                self.bytes.extend_from_slice(&n.to_le_bytes());
            }
            Value::Str(text) => self.text(text)?,
            Value::Path(path) => {
                if !valid_path(path) {
                    return Err(format!("invalid object path {path:?}"));
                }
                self.text(path)?;
            }
            Value::Signature(signature) => {
                split_signature(signature)?;
                self.bytes.push(signature.len() as u8);
                self.bytes.extend_from_slice(signature.as_bytes());
                self.bytes.push(0);
            }
            Value::Bytes(bytes) => {
                if bytes.len() > MAX_ARRAY {
                    return Err(String::from("array longer than 64 MiB"));
                }
                self.u32(bytes.len() as u32);
                self.bytes.extend_from_slice(bytes);
            }
            Value::Array(element, items) => {
                single_type(&format!("a{element}"))?;
                self.u32(0);
                let length_at = self.bytes.len() - 4;
                // The first element's padding comes even when there is none.
                self.pad(alignment(element.as_bytes()[0]));
                let start = self.bytes.len();
                for item in items {
                    if item.signature() != *element {
                        return Err(format!(
                            "an array of {element} holds a {}",
                            item.signature()
                        ));
                    }
                    self.value_at(item, depth + 1)?;
                }
                let length = self.bytes.len() - start;
                if length > MAX_ARRAY {
                    return Err(String::from("array longer than 64 MiB"));
                }
                self.bytes[length_at..length_at + 4]
                    .copy_from_slice(&(length as u32).to_le_bytes());
            }
            Value::Struct(fields) => {
                if fields.is_empty() {
                    return Err(String::from("empty struct"));
                }
                self.pad(8);
                for field in fields {
                    self.value_at(field, depth + 1)?;
                }
            }
            Value::Entry(key, value) => {
                if !key.signature().bytes().next().is_some_and(is_basic) {
                    return Err(String::from("a dict entry's key must be a basic type"));
                }
                self.pad(8);
                self.value_at(key, depth + 1)?;
                self.value_at(value, depth + 1)?;
            }
            Value::Variant(inner) => {
                let signature = inner.signature();
                self.value_at(&Value::Signature(signature), depth)?;
                self.value_at(inner, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Reads values from a message whose first byte is `data[0]`.
pub struct Reader<'a> {
    data: &'a [u8],
    pub at: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], big_endian: bool) -> Self {
        Self {
            data,
            at: 0,
            big_endian,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or("truncated message")?;
        let bytes = &self.data[self.at..end];
        self.at = end;
        Ok(bytes)
    }

    /// Skips padding to `align`, which must be nul.
    pub fn align(&mut self, align: usize) -> Result<(), String> {
        let padding = (align - self.at % align) % align;
        if self.take(padding)?.iter().any(|&b| b != 0) {
            return Err(String::from("padding that isn't nul"));
        }
        Ok(())
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.align(N)?;
        let mut bytes: [u8; N] = self.take(N)?.try_into().expect("N bytes");
        if self.big_endian {
            bytes.reverse();
        }
        Ok(bytes)
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        self.fixed::<4>().map(u32::from_le_bytes)
    }

    fn text(&mut self, length: usize) -> Result<String, String> {
        let bytes = self.take(length)?;
        if self.take(1)? != [0] {
            return Err(String::from("string without its nul"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "string that isn't UTF-8")?;
        if text.contains('\0') {
            return Err(String::from("string containing nul"));
        }
        Ok(text.to_string())
    }

    /// One value of the single complete type `signature`.
    pub fn value(&mut self, signature: &str) -> Result<Value, String> {
        single_type(signature)?;
        self.value_at(signature.as_bytes(), 0)
    }

    fn value_at(&mut self, signature: &[u8], depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(String::from("values nested too deeply"));
        }
        let value = match signature[0] {
            b'y' => Value::Byte(self.take(1)?[0]),
            b'b' => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(String::from("a boolean other than 0 or 1")),
            },
            b'n' => Value::I16(i16::from_le_bytes(self.fixed()?)),
            b'q' => Value::U16(u16::from_le_bytes(self.fixed()?)),
            b'i' => Value::I32(i32::from_le_bytes(self.fixed()?)),
            b'u' => Value::U32(self.u32()?),
            b'h' => Value::Fd(self.u32()?),
            b'x' => Value::I64(i64::from_le_bytes(self.fixed()?)),
            b't' => Value::U64(u64::from_le_bytes(self.fixed()?)),
            b'd' => Value::F64(f64::from_le_bytes(self.fixed()?)),
            b's' => {
                let length = self.u32()? as usize;
                Value::Str(self.text(length)?)
            }
            b'o' => {
                let length = self.u32()? as usize;
                let path = self.text(length)?;
                if !valid_path(&path) {
                    return Err(format!("invalid object path {path:?}"));
                }
                Value::Path(path)
            }
            b'g' => {
                let length = usize::from(self.take(1)?[0]);
                let text = self.text(length)?;
                split_signature(&text)?;
                Value::Signature(text)
            }
            b'v' => {
                let length = usize::from(self.take(1)?[0]);
                let inner = self.text(length)?;
                single_type(&inner)?;
                Value::variant(self.value_at(inner.as_bytes(), depth + 1)?)
            }
            b'a' => {
                let length = self.u32()? as usize;
                if length > MAX_ARRAY {
                    return Err(String::from("array longer than 64 MiB"));
                }
                // The array's whole type, less its `a`: a dict entry is only
                // valid as an element.
                let element = &signature[1..complete_type(signature, 0, 0)?];
                self.align(alignment(element[0]))?;
                let end = self
                    .at
                    .checked_add(length)
                    .filter(|end| *end <= self.data.len())
                    .ok_or("truncated array")?;
                if element == b"y" {
                    Value::Bytes(self.take(length)?.to_vec())
                } else {
                    let mut items = Vec::new();
                    while self.at < end {
                        items.push(self.value_at(element, depth + 1)?);
                    }
                    if self.at != end {
                        return Err(String::from("array elements overrun its length"));
                    }
                    let element = String::from_utf8(element.to_vec()).expect("ASCII");
                    Value::Array(element, items)
                }
            }
            b'(' => {
                self.align(8)?;
                let mut fields = Vec::new();
                let mut at = 1;
                while signature[at] != b')' {
                    let length = complete_type(&signature[at..], 0, 0)?;
                    fields.push(self.value_at(&signature[at..at + length], depth + 1)?);
                    at += length;
                }
                Value::Struct(fields)
            }
            b'{' => {
                self.align(8)?;
                let key = self.value_at(&signature[1..2], depth + 1)?;
                let value = self.value_at(&signature[2..signature.len() - 1], depth + 1)?;
                Value::Entry(Box::new(key), Box::new(value))
            }
            code => return Err(format!("unknown type code {:?}", char::from(code))),
        };
        Ok(value)
    }

    /// The values of a body with `signature`, which must fill `data` from
    /// where the reader is to the end.
    pub fn body(&mut self, signature: &str) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        for single in split_signature(signature)? {
            values.push(self.value(single)?);
        }
        if self.at != self.data.len() {
            return Err(String::from("body longer than its signature"));
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(value: &Value) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.value(value).unwrap();
        writer.bytes
    }

    fn round_trip(value: Value) {
        let bytes = written(&value);
        let mut reader = Reader::new(&bytes, false);
        assert_eq!(reader.value(&value.signature()).unwrap(), value);
        assert_eq!(reader.at, bytes.len());
    }

    #[test]
    fn strings_marshal_as_the_specification_shows() {
        // "Marshalling basic types": 'foo', '+' and 'bar' in sequence.
        let mut writer = Writer::default();
        for text in ["foo", "+", "bar"] {
            writer.value(&Value::str(text)).unwrap();
        }
        assert_eq!(
            writer.bytes,
            [
                3, 0, 0, 0, b'f', b'o', b'o', 0, 1, 0, 0, 0, b'+', 0, 0, 0, 3, 0, 0, 0, b'b', b'a',
                b'r', 0
            ]
        );
    }

    #[test]
    fn big_endian_containers_read_as_the_specification_shows() {
        // "Marshalling containers": an array holding the 64-bit integer 5,
        // and a variant holding it, both big-endian.
        let array = [0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5];
        assert_eq!(
            Reader::new(&array, true).value("at").unwrap(),
            Value::Array(String::from("t"), vec![Value::U64(5)])
        );
        let variant = [1, b't', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5];
        assert_eq!(
            Reader::new(&variant, true).value("v").unwrap(),
            Value::variant(Value::U64(5))
        );
        // Written little-endian, the same layout with the bytes reversed.
        assert_eq!(
            written(&Value::Array(String::from("t"), vec![Value::U64(5)])),
            [8, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn empty_arrays_still_pad_for_their_first_element() {
        assert_eq!(
            written(&Value::Struct(vec![
                Value::Byte(1),
                Value::Array(String::from("(i)"), Vec::new())
            ])),
            [1, 0, 0, 0, 0, 0, 0, 0]
        );
        round_trip(Value::Struct(vec![
            Value::Byte(1),
            Value::Array(String::from("(i)"), Vec::new()),
            Value::Byte(2),
        ]));
    }

    #[test]
    fn nested_values_round_trip() {
        round_trip(Value::dict([
            ("label", Value::str("Show RubyChat")),
            ("enabled", Value::Bool(true)),
            ("toggle-state", Value::I32(-1)),
        ]));
        // A dbusmenu layout, (ia{sv}av), with a child.
        let child = Value::Struct(vec![
            Value::I32(1),
            Value::dict([("type", Value::str("separator"))]),
            Value::Array(String::from("v"), Vec::new()),
        ]);
        round_trip(Value::Struct(vec![
            Value::I32(0),
            Value::dict([("children-display", Value::str("submenu"))]),
            Value::Array(String::from("v"), vec![Value::variant(child)]),
        ]));
        // An icon pixmap, a(iiay).
        round_trip(Value::Array(
            String::from("(iiay)"),
            vec![Value::Struct(vec![
                Value::I32(2),
                Value::I32(1),
                Value::Bytes(vec![255, 1, 2, 3, 255, 4, 5, 6]),
            ])],
        ));
        round_trip(Value::Struct(vec![
            Value::I16(-2),
            Value::U16(3),
            Value::I64(-4),
            Value::F64(0.5),
            Value::Path(String::from("/StatusNotifierItem")),
            Value::Signature(String::from("a{sv}")),
            Value::Fd(0),
        ]));
    }

    #[test]
    fn signatures_follow_the_rules() {
        assert_eq!(
            split_signature("ia{sv}(ii)as").unwrap(),
            ["i", "a{sv}", "(ii)", "as"]
        );
        for bad in [
            "aa", "(ii", "ii)", "()", "a{vs}", "a{sss}", "{sv}", "r", "e",
        ] {
            assert!(split_signature(bad).is_err(), "{bad} is invalid");
        }
        let deep = format!("{}i", "a".repeat(33));
        assert!(split_signature(&deep).is_err());
        let fine = format!("{}i", "a".repeat(32));
        assert!(split_signature(&fine).is_ok());
    }

    #[test]
    fn malformed_values_are_refused() {
        // A boolean of 2.
        assert!(Reader::new(&[2, 0, 0, 0], false).value("b").is_err());
        // Nonzero padding before a uint32.
        assert!(
            Reader::new(&[1, 9, 0, 0, 5, 0, 0, 0], false)
                .value("(yu)")
                .is_err()
        );
        // Invalid UTF-8, a missing nul, an inner nul.
        assert!(
            Reader::new(&[1, 0, 0, 0, 0xff, 0], false)
                .value("s")
                .is_err()
        );
        assert!(
            Reader::new(&[1, 0, 0, 0, b'a', b'b'], false)
                .value("s")
                .is_err()
        );
        assert!(
            Reader::new(&[2, 0, 0, 0, b'a', 0, 0], false)
                .value("s")
                .is_err()
        );
        // An object path with an empty element; an array overrunning the data.
        assert!(
            Reader::new(&[2, 0, 0, 0, b'/', b'/', 0], false)
                .value("o")
                .is_err()
        );
        assert!(
            Reader::new(&[8, 0, 0, 0, 1, 0, 0, 0], false)
                .value("ai")
                .is_err()
        );
        // A variant holding two types.
        assert!(Reader::new(&[2, b'i', b'i', 0], false).value("v").is_err());
        // Nesting variants past 64 levels.
        let mut deep = Value::Byte(0);
        for _ in 0..70 {
            deep = Value::variant(deep);
        }
        let mut writer = Writer::default();
        assert!(writer.value(&deep).is_err());
        // Writing mismatched array elements or a string with nul.
        assert!(
            Writer::default()
                .value(&Value::Array(String::from("s"), vec![Value::I32(1)]))
                .is_err()
        );
        assert!(Writer::default().value(&Value::str("a\0b")).is_err());
    }

    #[test]
    fn object_paths() {
        assert!(valid_path("/"));
        assert!(valid_path("/StatusNotifierItem"));
        assert!(valid_path("/org/freedesktop/DBus"));
        for bad in ["", "a", "/a/", "//", "/a-b", "/a//b"] {
            assert!(!valid_path(bad), "{bad:?}");
        }
    }
}
