//! What the server advertised in RPL_ISUPPORT (Modern IRC, "Feature
//! Advertisement"), with the specified defaults until it says otherwise, and
//! the MODE parsing its `PREFIX` and `CHANMODES` make possible.

use super::casemap::Casemap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Isupport {
    pub casemap: Casemap,
    /// Channel name prefixes; `#&` until advertised.
    pub chantypes: String,
    /// Membership modes and their prefixes, highest rank first; `(ov)@+`
    /// until advertised.
    pub prefixes: Vec<(char, char)>,
    /// Channel mode types A (lists), B (always a parameter), C (a parameter
    /// when set) and D (never a parameter).
    pub chanmodes: [String; 4],
    /// Prefixes a message target may carry to reach only members with them.
    pub statusmsg: String,
    pub network: Option<String>,
    pub nicklen: Option<usize>,
    pub topiclen: Option<usize>,
}

impl Default for Isupport {
    fn default() -> Self {
        Self {
            casemap: Casemap::default(),
            chantypes: String::from("#&"),
            prefixes: vec![('o', '@'), ('v', '+')],
            chanmodes: [
                String::from("b"),
                String::from("k"),
                String::from("l"),
                String::from("imnpst"),
            ],
            statusmsg: String::new(),
            network: None,
            nicklen: None,
            topiclen: None,
        }
    }
}

/// One change from a MODE line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeChange {
    pub adding: bool,
    pub mode: char,
    pub param: Option<String>,
}

impl Isupport {
    /// Applies an RPL_ISUPPORT line's tokens (between the target and the
    /// trailing text). `-TOKEN` restores a token's default.
    pub fn apply<'a>(&mut self, tokens: impl IntoIterator<Item = &'a str>) {
        let defaults = Isupport::default();
        for token in tokens {
            if let Some(name) = token.strip_prefix('-') {
                match name.to_ascii_uppercase().as_str() {
                    "CASEMAPPING" => self.casemap = defaults.casemap,
                    "CHANTYPES" => self.chantypes = defaults.chantypes.clone(),
                    "PREFIX" => self.prefixes = defaults.prefixes.clone(),
                    "CHANMODES" => self.chanmodes = defaults.chanmodes.clone(),
                    "STATUSMSG" => self.statusmsg.clear(),
                    "NETWORK" => self.network = None,
                    "NICKLEN" => self.nicklen = None,
                    "TOPICLEN" => self.topiclen = None,
                    _ => {}
                }
                continue;
            }
            let (name, value) = token.split_once('=').unwrap_or((token, ""));
            match name.to_ascii_uppercase().as_str() {
                "CASEMAPPING" => self.casemap = Casemap::from_token(value),
                "CHANTYPES" => self.chantypes = value.to_string(),
                "PREFIX" => {
                    self.prefixes = value
                        .strip_prefix('(')
                        .and_then(|rest| rest.split_once(')'))
                        .map(|(modes, prefixes)| modes.chars().zip(prefixes.chars()).collect())
                        .unwrap_or_default();
                }
                "CHANMODES" => {
                    let mut types = value.split(',').map(str::to_string);
                    self.chanmodes = std::array::from_fn(|_| types.next().unwrap_or_default());
                }
                "STATUSMSG" => self.statusmsg = value.to_string(),
                "NETWORK" => self.network = (!value.is_empty()).then(|| value.to_string()),
                "NICKLEN" => self.nicklen = value.parse().ok(),
                "TOPICLEN" => self.topiclen = value.parse().ok(),
                _ => {}
            }
        }
    }

    pub fn is_channel(&self, name: &str) -> bool {
        name.chars()
            .next()
            .is_some_and(|c| self.chantypes.contains(c))
    }

    /// A membership prefix's rank: 0 is the highest.
    pub fn rank(&self, prefix: char) -> Option<usize> {
        self.prefixes.iter().position(|&(_, p)| p == prefix)
    }

    pub fn prefix_for_mode(&self, mode: char) -> Option<char> {
        self.prefixes
            .iter()
            .find(|&&(m, _)| m == mode)
            .map(|&(_, p)| p)
    }

    /// Splits `nick` from leading membership prefixes (several with
    /// `multi-prefix`), as NAMES lists them.
    pub fn split_prefixes<'a>(&self, name: &'a str) -> (String, &'a str) {
        let mut prefixes = String::new();
        let mut rest = name;
        while let Some(c) = rest.chars().next() {
            if self.rank(c).is_none() {
                break;
            }
            prefixes.push(c);
            rest = &rest[c.len_utf8()..];
        }
        (prefixes, rest)
    }

    /// Removes a STATUSMSG prefix from a channel target: `@#rust` is
    /// `#rust`.
    pub fn strip_statusmsg<'a>(&self, target: &'a str) -> &'a str {
        let rest = target.trim_start_matches(|c| self.statusmsg.contains(c));
        if rest.len() < target.len() && self.is_channel(rest) {
            rest
        } else {
            target
        }
    }

    /// The changes in a channel MODE line: membership modes and CHANMODES
    /// types A and B take a parameter, type C only when set, type D never.
    pub fn channel_modes(&self, modestring: &str, params: &[&str]) -> Vec<ModeChange> {
        let mut params = params.iter();
        let mut adding = true;
        let mut changes = Vec::new();
        for c in modestring.chars() {
            match c {
                '+' => adding = true,
                '-' => adding = false,
                mode => {
                    let takes = self.prefix_for_mode(mode).is_some()
                        || self.chanmodes[0].contains(mode)
                        || self.chanmodes[1].contains(mode)
                        || (adding && self.chanmodes[2].contains(mode));
                    let param = if takes {
                        params.next().map(|p| p.to_string())
                    } else {
                        None
                    };
                    changes.push(ModeChange {
                        adding,
                        mode,
                        param,
                    });
                }
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn libera() -> Isupport {
        let mut isupport = Isupport::default();
        isupport.apply(
            "CHANTYPES=# CHANMODES=eIbq,k,flj,CFLMPQRSTcgimnprstuz PREFIX=(ov)@+ STATUSMSG=@+ CASEMAPPING=rfc1459 NETWORK=Libera.Chat NICKLEN=16"
                .split(' '),
        );
        isupport
    }

    #[test]
    fn applies_tokens_and_negations() {
        let mut isupport = libera();
        assert_eq!(isupport.network.as_deref(), Some("Libera.Chat"));
        assert_eq!(isupport.nicklen, Some(16));
        assert!(
            isupport.is_channel("#rust")
                && !isupport.is_channel("&local")
                && !isupport.is_channel("ruby")
        );
        isupport.apply(["PREFIX=(qaohv)~&@%+", "-NETWORK"]);
        assert_eq!(isupport.rank('~'), Some(0));
        assert_eq!(isupport.rank('+'), Some(4));
        assert_eq!(isupport.network, None);
        isupport.apply(["PREFIX="]);
        assert!(isupport.prefixes.is_empty(), "an empty PREFIX means none");
        assert_eq!(Isupport::default().chantypes, "#&");
    }

    #[test]
    fn splits_prefixes_and_statusmsg_targets() {
        let mut isupport = libera();
        isupport.apply(["PREFIX=(qaohv)~&@%+", "STATUSMSG=~&@%+"]);
        assert_eq!(
            isupport.split_prefixes("@+ruby"),
            (String::from("@+"), "ruby")
        );
        assert_eq!(isupport.split_prefixes("ruby"), (String::new(), "ruby"));
        assert_eq!(isupport.strip_statusmsg("@#rust"), "#rust");
        assert_eq!(
            isupport.strip_statusmsg("@ruby"),
            "@ruby",
            "not a channel behind the prefix"
        );
    }

    #[test]
    fn mode_parameters_follow_the_mode_types() {
        let isupport = libera();
        let changes = isupport.channel_modes(
            "+ovkl-l+b-k",
            &["alice", "bob", "secret", "10", "*!*@spam", "secret"],
        );
        let summary: Vec<(bool, char, Option<&str>)> = changes
            .iter()
            .map(|c| (c.adding, c.mode, c.param.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (true, 'o', Some("alice")),
                (true, 'v', Some("bob")),
                (true, 'k', Some("secret")),
                (true, 'l', Some("10")),
                (false, 'l', None),
                (true, 'b', Some("*!*@spam")),
                (false, 'k', Some("secret")),
            ]
        );
        assert_eq!(isupport.channel_modes("+nt", &[]).len(), 2);
    }
}
