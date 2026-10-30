//! STS policies (`sts.toml`, beside `known_hosts`; IRCv3 STS): hosts that
//! must only be reached over TLS, on which port, and until when. A policy
//! is recorded from a secure connection's `duration`, rescheduled each time
//! one closes, and removed by `duration=0`. Expired policies are dropped
//! when the file is next written.

use super::toml::{self, Table, Value};

/// Hosts remembered at most; past this the soonest to expire goes.
const MAX_POLICIES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StsPolicy {
    /// The host as connected to, lowercase.
    pub host: String,
    pub port: u16,
    /// The duration the server last advertised, in seconds.
    pub duration: u64,
    /// When the policy lapses, in seconds since the Unix epoch.
    pub expires: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StsPolicies {
    list: Vec<StsPolicy>,
}

impl StsPolicies {
    /// Reads `sts.toml`; entries that don't make sense are left out.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root = toml::parse(text).map_err(|error| format!("sts.toml: {error}"))?;
        let list = root
            .tables("policy")
            .iter()
            .filter_map(|table| {
                Some(StsPolicy {
                    host: table.string("host")?.to_ascii_lowercase(),
                    port: u16::try_from(table.integer("port")?).ok()?,
                    duration: u64::try_from(table.integer("duration")?).ok()?,
                    expires: table.integer("expires")?,
                })
            })
            .collect();
        Ok(Self { list })
    }

    pub fn to_toml(&self, now: i64) -> String {
        let mut root = Table::default();
        let tables = self
            .list
            .iter()
            .filter(|policy| policy.expires > now)
            .map(|policy| {
                let mut table = Table::default();
                table.insert("host", Value::String(policy.host.clone()));
                table.insert("port", Value::Integer(i64::from(policy.port)));
                table.insert(
                    "duration",
                    Value::Integer(i64::try_from(policy.duration).unwrap_or(i64::MAX)),
                );
                table.insert("expires", Value::Integer(policy.expires));
                table
            })
            .collect();
        root.insert("policy", Value::Tables(tables));
        toml::to_string(
            &root,
            "Hosts that asked to be reached over TLS only (IRCv3 STS), until `expires`.",
        )
    }

    /// The policy in force for `host` at `now` (seconds since the epoch).
    pub fn get(&self, host: &str, now: i64) -> Option<&StsPolicy> {
        self.list
            .iter()
            .find(|policy| policy.host.eq_ignore_ascii_case(host) && policy.expires > now)
    }

    /// Records a persistence policy of `duration` seconds from `now` for
    /// `host`, reached securely on `port`; `duration` 0 removes it.
    pub fn set(&mut self, host: &str, port: u16, duration: u64, now: i64) {
        self.list
            .retain(|policy| !policy.host.eq_ignore_ascii_case(host) && policy.expires > now);
        if duration == 0 {
            return;
        }
        if self.list.len() >= MAX_POLICIES
            && let Some(soonest) = (0..self.list.len()).min_by_key(|&i| self.list[i].expires)
        {
            self.list.remove(soonest);
        }
        self.list.push(StsPolicy {
            host: host.to_ascii_lowercase(),
            port,
            duration,
            expires: now.saturating_add(i64::try_from(duration).unwrap_or(i64::MAX)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_are_kept_until_they_expire_and_zero_removes_them() {
        let mut policies = StsPolicies::default();
        policies.set("IRC.Example.org", 6697, 3600, 1_000);
        assert_eq!(
            policies.get("irc.example.org", 2_000).map(|p| p.port),
            Some(6697)
        );
        assert!(policies.get("irc.example.org", 5_000).is_none(), "expired");
        // Rescheduled when a connection closes later.
        policies.set("irc.example.org", 6697, 3600, 4_000);
        assert!(policies.get("irc.example.org", 5_000).is_some());
        let text = policies.to_toml(4_500);
        assert_eq!(StsPolicies::parse(&text).unwrap(), policies);
        policies.set("irc.example.org", 6697, 0, 4_600);
        assert!(policies.get("irc.example.org", 4_700).is_none());
        let odd = StsPolicies::parse(
            "[[policy]]\nhost = \"a\"\nport = 99999\nduration = 1\nexpires = 9\n",
        )
        .unwrap();
        assert!(odd.list.is_empty(), "a port out of range is left out");
    }
}
