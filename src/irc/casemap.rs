//! Casemapping (Modern IRC, RPL_ISUPPORT `CASEMAPPING`): which names are the
//! same nick or channel.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Casemap {
    /// `a`–`z` are the lower-case forms of `A`–`Z`, and nothing else.
    Ascii,
    /// ASCII, plus `{}|^` as the lower-case forms of `[]\~`. Servers that
    /// don't advertise a casemapping use this.
    #[default]
    Rfc1459,
    /// RFC 1459 without the `~`/`^` pair.
    Rfc1459Strict,
    /// PRECIS (RFC 7613/8265): Unicode case folding. Approximated by
    /// Unicode lower-casing, which agrees for the names in use.
    Rfc7613,
}

impl Casemap {
    /// The casemapping a `CASEMAPPING` value names; unknown values keep the
    /// default.
    pub fn from_token(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "ascii" => Casemap::Ascii,
            "rfc1459-strict" | "strict-rfc1459" => Casemap::Rfc1459Strict,
            "rfc7613" | "precis" => Casemap::Rfc7613,
            _ => Casemap::Rfc1459,
        }
    }

    /// The name's lower-case form, for comparing and indexing.
    pub fn fold(self, name: &str) -> String {
        match self {
            Casemap::Rfc7613 => name.to_lowercase(),
            _ => name
                .chars()
                .map(|c| match (self, c) {
                    (_, 'A'..='Z') => c.to_ascii_lowercase(),
                    (Casemap::Rfc1459 | Casemap::Rfc1459Strict, '[') => '{',
                    (Casemap::Rfc1459 | Casemap::Rfc1459Strict, ']') => '}',
                    (Casemap::Rfc1459 | Casemap::Rfc1459Strict, '\\') => '|',
                    (Casemap::Rfc1459, '~') => '^',
                    _ => c,
                })
                .collect(),
        }
    }

    pub fn eq(self, a: &str, b: &str) -> bool {
        self.fold(a) == self.fold(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_per_the_advertised_mapping() {
        assert_eq!(Casemap::Rfc1459.fold("Ruby[Away]\\~"), "ruby{away}|^");
        assert_eq!(Casemap::Rfc1459Strict.fold("Ruby[Away]\\~"), "ruby{away}|~");
        assert_eq!(Casemap::Ascii.fold("Ruby[Away]\\~"), "ruby[away]\\~");
        assert_eq!(Casemap::Rfc7613.fold("ÉLAN"), "élan");
        assert!(Casemap::Rfc1459.eq("[ruby]", "{RUBY}"));
        assert!(!Casemap::Ascii.eq("[ruby]", "{RUBY}"));
        assert!(Casemap::Rfc7613.eq("ÉLAN", "élan"));
        assert_eq!(Casemap::from_token("ASCII"), Casemap::Ascii);
        assert_eq!(
            Casemap::from_token("rfc1459-strict"),
            Casemap::Rfc1459Strict
        );
        assert_eq!(Casemap::from_token("mystery"), Casemap::Rfc1459);
    }
}
