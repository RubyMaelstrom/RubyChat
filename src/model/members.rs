//! A channel's members: found by casefolded nick in constant time, and
//! listed by rank (the server's PREFIX order) and then nick, as the nick list
//! shows them. Large channels stay cheap: the order is rebuilt only when it
//! is next shown.

use std::collections::HashMap;

use crate::irc::isupport::Isupport;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    /// Membership prefixes, highest rank first (`@+`).
    pub prefixes: String,
    pub away: bool,
    /// When they last spoke, for completing recent speakers first.
    pub spoke: i64,
}

impl Member {
    pub fn new(nick: impl Into<String>, prefixes: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            prefixes: prefixes.into(),
            away: false,
            spoke: 0,
        }
    }

    /// The highest prefix, if any.
    pub fn prefix(&self) -> Option<char> {
        self.prefixes.chars().next()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Members {
    by_key: HashMap<String, Member>,
    /// Keys in display order, valid while `sorted`.
    order: Vec<String>,
    sorted: bool,
}

impl Members {
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_key.clear();
        self.order.clear();
        self.sorted = true;
    }

    pub fn get(&self, nick: &str, isupport: &Isupport) -> Option<&Member> {
        self.by_key.get(&isupport.casemap.fold(nick))
    }

    pub fn contains(&self, nick: &str, isupport: &Isupport) -> bool {
        self.get(nick, isupport).is_some()
    }

    /// Adds or replaces a member.
    pub fn insert(&mut self, member: Member, isupport: &Isupport) {
        self.by_key
            .insert(isupport.casemap.fold(&member.nick), member);
        self.sorted = false;
    }

    pub fn remove(&mut self, nick: &str, isupport: &Isupport) -> Option<Member> {
        let removed = self.by_key.remove(&isupport.casemap.fold(nick));
        if removed.is_some() {
            self.sorted = false;
        }
        removed
    }

    /// A nick change; false if `old` isn't a member.
    pub fn rename(&mut self, old: &str, new: &str, isupport: &Isupport) -> bool {
        let Some(mut member) = self.remove(old, isupport) else {
            return false;
        };
        member.nick = new.to_string();
        self.insert(member, isupport);
        true
    }

    /// Gives or takes a membership prefix, keeping prefixes in rank order.
    pub fn set_prefix(
        &mut self,
        nick: &str,
        prefix: char,
        adding: bool,
        isupport: &Isupport,
    ) -> bool {
        let Some(member) = self.by_key.get_mut(&isupport.casemap.fold(nick)) else {
            return false;
        };
        let mut prefixes: Vec<char> = member.prefixes.chars().filter(|&c| c != prefix).collect();
        if adding {
            prefixes.push(prefix);
        }
        prefixes.sort_by_key(|&c| isupport.rank(c).unwrap_or(usize::MAX));
        member.prefixes = prefixes.into_iter().collect();
        self.sorted = false;
        true
    }

    pub fn touch(&mut self, nick: &str, time: i64, isupport: &Isupport) {
        if let Some(member) = self.by_key.get_mut(&isupport.casemap.fold(nick)) {
            member.spoke = time;
        }
    }

    pub fn set_away(&mut self, nick: &str, away: bool, isupport: &Isupport) -> bool {
        match self.by_key.get_mut(&isupport.casemap.fold(nick)) {
            Some(member) if member.away != away => {
                member.away = away;
                true
            }
            _ => false,
        }
    }

    /// Members in display order: by rank, then by casefolded nick.
    pub fn sorted(&mut self, isupport: &Isupport) -> impl ExactSizeIterator<Item = &Member> + '_ {
        if !self.sorted {
            let rank = |member: &Member| {
                member
                    .prefix()
                    .and_then(|p| isupport.rank(p))
                    .unwrap_or(usize::MAX)
            };
            let mut keyed: Vec<(usize, &String)> = self
                .by_key
                .iter()
                .map(|(key, member)| (rank(member), key))
                .collect();
            keyed.sort();
            self.order = keyed.into_iter().map(|(_, key)| key.clone()).collect();
            self.sorted = true;
        }
        self.order.iter().map(|key| &self.by_key[key])
    }

    /// Nicks starting with `prefix` (casefolded), most recent speakers
    /// first, then alphabetically.
    pub fn completions(&self, prefix: &str, isupport: &Isupport) -> Vec<&str> {
        let folded = isupport.casemap.fold(prefix);
        let mut matches: Vec<(&String, &Member)> = self
            .by_key
            .iter()
            .filter(|(key, _)| key.starts_with(&folded))
            .collect();
        matches.sort_by(|a, b| b.1.spoke.cmp(&a.1.spoke).then_with(|| a.0.cmp(b.0)));
        matches
            .into_iter()
            .map(|(_, member)| member.nick.as_str())
            .collect()
    }

    /// Re-keys every member after the casemapping changed.
    pub fn rekey(&mut self, isupport: &Isupport) {
        let members: Vec<Member> = self.by_key.drain().map(|(_, member)| member).collect();
        for member in members {
            self.insert(member, isupport);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isupport() -> Isupport {
        let mut isupport = Isupport::default();
        isupport.apply(["PREFIX=(qaohv)~&@%+"]);
        isupport
    }

    #[test]
    fn lists_by_rank_then_nick() {
        let isupport = isupport();
        let mut members = Members::default();
        for (nick, prefixes) in [
            ("zed", ""),
            ("Alice", "@"),
            ("bob", "+"),
            ("Carol", "~@"),
            ("amy", ""),
        ] {
            members.insert(Member::new(nick, prefixes), &isupport);
        }
        let order: Vec<&str> = members.sorted(&isupport).map(|m| m.nick.as_str()).collect();
        assert_eq!(order, ["Carol", "Alice", "bob", "amy", "zed"]);
        members.set_prefix("ZED", '@', true, &isupport);
        members.set_prefix("zed", '+', true, &isupport);
        assert_eq!(members.get("zed", &isupport).unwrap().prefixes, "@+");
        members.set_prefix("carol", '~', false, &isupport);
        let order: Vec<&str> = members.sorted(&isupport).map(|m| m.nick.as_str()).collect();
        assert_eq!(order, ["Alice", "Carol", "zed", "bob", "amy"]);
    }

    #[test]
    fn renames_removes_and_completes() {
        let isupport = isupport();
        let mut members = Members::default();
        for nick in ["ruby", "rust", "rustacean", "bob"] {
            members.insert(Member::new(nick, ""), &isupport);
        }
        members.touch("rustacean", 10, &isupport);
        assert_eq!(
            members.completions("RU", &isupport),
            ["rustacean", "ruby", "rust"]
        );
        assert!(members.rename("bob", "Robert", &isupport));
        assert!(members.contains("robert", &isupport) && !members.contains("bob", &isupport));
        assert!(members.remove("RUBY", &isupport).is_some());
        assert_eq!(members.len(), 3);
        assert!(!members.rename("nobody", "x", &isupport));
    }
}
