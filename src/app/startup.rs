//! Starting up and keeping state across runs (DESIGN.md §11). Saved
//! networks, their secrets and certificate pins load on the store thread;
//! once they have, the session is restored, networks set to connect at
//! startup connect, and the addresses on the command line follow. The
//! session is saved as it changes and at exit.

use std::path::PathBuf;

use crate::config::known_hosts::KnownHosts;
use crate::config::networks::{AutoJoin, LoginMethod, Networks, SavedNetwork, SavedServer};
use crate::config::session::{Session, SessionBuffer, SessionKind, SessionNetwork};
use crate::model::LineKind;
use crate::model::buffer::BufferKind;
use crate::net::{
    CertificateProblem, ConnectConfig, LinkStatus, Login, Mechanism, NetCommand, NetworkId,
};
use crate::platform::paths;
use crate::store::Document;
use crate::ui::sidebar;

use super::{App, commands};

/// What startup waits for.
#[derive(Default)]
pub(super) struct Startup {
    networks: Option<Option<String>>,
    secrets: Option<Option<String>>,
    known_hosts: bool,
    pub(super) session: Option<Option<Session>>,
    /// Addresses from the command line, connected once started.
    pub(super) addresses: Vec<String>,
    pub(super) done: bool,
}

/// A client certificate's file: as given when absolute, under the home
/// directory for `~/`, else in RubyChat's `certs` directory (DESIGN.md §11).
pub(super) fn certificate_path(text: &str) -> PathBuf {
    let path = PathBuf::from(text);
    if path.is_absolute() {
        return path;
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    paths::config_dir().map_or(path.clone(), |dir| dir.join("certs").join(path))
}

impl App {
    /// Loads what startup needs; without a store, starts at once.
    pub(super) fn begin_startup(&mut self) {
        match &self.store {
            Some(store) => {
                for document in [
                    Document::Networks,
                    Document::Secrets,
                    Document::KnownHosts,
                    Document::Session,
                ] {
                    store.load(document);
                }
            }
            None => self.start(),
        }
    }

    /// A document startup waits for arrived.
    pub(super) fn startup_loaded(
        &mut self,
        document: Document,
        result: Result<Option<String>, String>,
    ) {
        let text = match result {
            Ok(text) => text,
            Err(error) => {
                if matches!(document, Document::Networks | Document::Secrets) {
                    // Unreadable: keep it from being overwritten.
                    self.saved_error = Some(error);
                } else {
                    eprintln!("rubychat: {error}");
                }
                None
            }
        };
        match document {
            Document::Networks => self.startup.networks = Some(text),
            Document::Secrets => self.startup.secrets = Some(text),
            Document::KnownHosts => {
                if let Some(text) = text {
                    self.known_hosts = KnownHosts::parse(&text);
                }
                self.startup.known_hosts = true;
            }
            Document::Session => {
                let session = text.and_then(|text| {
                    Session::parse(&text)
                        .map_err(|error| eprintln!("rubychat: {error}"))
                        .ok()
                });
                self.startup.session = Some(session);
            }
            Document::Recent => return,
        }
        if let (Some(networks), Some(secrets)) = (&self.startup.networks, &self.startup.secrets)
            && self.saved_error.is_none()
            && self.saved.list.is_empty()
        {
            match Networks::parse(networks.as_deref().unwrap_or(""), secrets.as_deref()) {
                Ok(saved) => self.saved = saved,
                Err(error) => self.saved_error = Some(error),
            }
        }
        let startup = &self.startup;
        if !startup.done
            && startup.networks.is_some()
            && startup.secrets.is_some()
            && startup.known_hosts
            && startup.session.is_some()
        {
            self.start();
        }
    }

    /// Opens what the session had open, then networks set to connect at
    /// startup, then the command line's addresses.
    pub(super) fn start(&mut self) {
        self.startup.done = true;
        let session = self.startup.session.take().flatten().unwrap_or_default();
        let restore = self.keep_session;
        let mut restored = Vec::new();
        for entry in session.networks.iter().filter(|_| restore) {
            if entry.name.is_empty() {
                let server = commands::Server {
                    host: entry.host.clone(),
                    port: entry.port,
                    tls: entry.tls,
                    password: None,
                    entity: None,
                };
                self.open_server(server, Some(entry));
            } else if let Some(saved) = self.saved.find(&entry.name).cloned() {
                self.connect_saved(&saved, Some(entry));
                restored.push(saved.name);
            }
        }
        if !self.networks.is_empty() {
            self.activate(session.active.min(self.networks.len() - 1));
        }
        if restore {
            for saved in self.saved.list.clone() {
                if saved.connect_at_startup && !restored.contains(&saved.name) {
                    self.connect_saved(&saved, None);
                }
            }
        }
        for address in std::mem::take(&mut self.startup.addresses) {
            match commands::parse_server(&address) {
                Ok(server) => self.connect(server),
                Err(message) => self.prompt_message = Some(message),
            }
        }
        if let Some(error) = &self.saved_error {
            self.prompt_message = Some(format!(
                "Saved networks weren't loaded, and won't be saved over: {error}"
            ));
        }
        self.sync_seen();
        self.mark_all();
        self.update_title();
    }

    /// The connection settings for one of a saved network's servers.
    pub(super) fn config_for(&self, saved: &SavedNetwork, server: &SavedServer) -> ConnectConfig {
        let nick = if saved.nick.is_empty() {
            commands::default_nick()
        } else {
            saved.nick.clone()
        };
        let mut config =
            ConnectConfig::new(server.host.clone(), server.port, server.tls, nick.clone());
        config.alt_nicks = saved.alt_nicks.clone();
        if !saved.username.is_empty() {
            config.username = saved.username.clone();
        }
        if !saved.realname.is_empty() {
            config.realname = saved.realname.clone();
        }
        let login = &saved.login;
        let account = if login.account.is_empty() {
            nick
        } else {
            login.account.clone()
        };
        let sasl = |mechanism| Login::Sasl {
            mechanism,
            account: account.clone(),
            password: login.password.clone(),
        };
        match login.method {
            LoginMethod::None => {}
            LoginMethod::ServerPassword => {
                config.password =
                    Some(login.password.clone()).filter(|password| !password.is_empty())
            }
            LoginMethod::SaslPlain => config.login = sasl(Mechanism::Plain),
            LoginMethod::SaslScramSha256 => config.login = sasl(Mechanism::ScramSha256),
            LoginMethod::SaslExternal => {
                config.login = Login::Sasl {
                    mechanism: Mechanism::External,
                    account: String::new(),
                    password: String::new(),
                }
            }
            LoginMethod::NickServ => {
                config.login = Login::NickServ {
                    account: login.account.clone(),
                    password: login.password.clone(),
                }
            }
        }
        config.require_login = login.require && login.method.is_sasl();
        if !login.client_certificate.is_empty() {
            config.client_certificate = Some(certificate_path(&login.client_certificate));
        }
        config.fallback = saved.fallback_encoding;
        config.flood = saved.flood.timing();
        config.reconnect = saved.reconnect;
        config.quit_message = saved.quit_message.clone();
        config.certificate = server.certificate;
        config.pins = self
            .known_hosts
            .get(&server.host, server.port)
            .into_iter()
            .collect();
        config
    }

    /// Connects a saved network, opening its channels (and, restoring a
    /// session, what was open) at once.
    pub(super) fn connect_saved(
        &mut self,
        saved: &SavedNetwork,
        restore: Option<&SessionNetwork>,
    ) -> Option<usize> {
        let Some(server) = saved.servers.first() else {
            self.prompt_message = Some(format!("{} has no server to connect to.", saved.name));
            self.mark_all();
            return None;
        };
        let config = self.config_for(saved, server);
        let id = self.add_network(saved.name.clone(), config.clone());
        let index = self.index_of(id).expect("just added");
        let network = &mut self.networks[index];
        network.saved = Some(saved.name.clone());
        network.commands = saved.commands.clone();
        network.state.ctcp_replies = saved.ctcp_replies;
        network.state.part_message = saved.part_message.clone();
        let buffers = restore
            .map(|entry| entry.buffers.as_slice())
            .unwrap_or_default();
        let shown = restore.map(|entry| entry.shown.as_str());
        open_buffers(network, buffers, &saved.autojoin, shown);
        self.send(NetCommand::Connect(id, config));
        self.prompt_open = false;
        self.activate(index);
        self.session_changed();
        Some(index)
    }

    /// Connects an address, restoring its buffers from the session if there
    /// is one.
    pub(super) fn open_server(
        &mut self,
        server: commands::Server,
        restore: Option<&SessionNetwork>,
    ) -> usize {
        let nick = restore
            .map(|entry| entry.nick.clone())
            .filter(|nick| !nick.is_empty())
            .unwrap_or_else(commands::default_nick);
        let mut config = ConnectConfig::new(server.host.clone(), server.port, server.tls, nick);
        config.password = server.password;
        config.pins = self
            .known_hosts
            .get(&server.host, server.port)
            .into_iter()
            .collect();
        let title = server
            .host
            .strip_prefix("irc.")
            .unwrap_or(&server.host)
            .to_string();
        let id = self.add_network(title, config.clone());
        let index = self.index_of(id).expect("just added");
        let network = &mut self.networks[index];
        let mut autojoin = Vec::new();
        match server.entity {
            Some(commands::Entity::Channel { name, key }) => autojoin.push(AutoJoin {
                channel: name,
                key: key.unwrap_or_default(),
            }),
            Some(commands::Entity::Nick(nick)) => {
                network.active = network.state.open(BufferKind::Query, &nick).0;
            }
            None => {}
        }
        let buffers = restore
            .map(|entry| entry.buffers.as_slice())
            .unwrap_or_default();
        let shown = restore.map(|entry| entry.shown.as_str());
        open_buffers(network, buffers, &autojoin, shown);
        self.send(NetCommand::Connect(id, config));
        self.prompt_open = false;
        self.activate(index);
        self.session_changed();
        index
    }

    /// Runs a network's commands after it registers, as typed in its server
    /// buffer.
    pub(super) fn run_connect_commands(&mut self, index: usize) {
        let lines = self.networks[index].commands.clone();
        if lines.is_empty() {
            return;
        }
        let (active, shown) = (self.active, self.networks[index].active);
        self.active = index;
        self.networks[index].active = 0;
        for line in lines {
            self.run_command(&line);
        }
        self.active = active;
        if let Some(network) = self.networks.get_mut(index)
            && shown < network.state.buffers.len()
        {
            network.active = shown;
        }
    }

    /// A saved network with several servers tries the next after a
    /// failure, on the link's own schedule.
    pub(super) fn next_server(&mut self, index: usize) {
        let network = &self.networks[index];
        let Some(saved) = network
            .saved
            .as_ref()
            .and_then(|name| self.saved.find(name))
        else {
            return;
        };
        if saved.servers.len() < 2 {
            return;
        }
        let next = (network.server + 1) % saved.servers.len();
        let config = self.config_for(saved, &saved.servers[next]);
        let id = network.id;
        let network = &mut self.networks[index];
        network.server = next;
        network.config = config.clone();
        network.push(
            0,
            LineKind::Status,
            "•",
            format!("Next time, trying {}", saved.servers[next].address()),
        );
        self.send(NetCommand::Update(id, config));
    }

    /// What is open now, for the next start: networks connected or trying
    /// to be, and the channels they are in or joining, with queries.
    pub(super) fn session(&self) -> Session {
        let mut active = 0;
        let mut networks = Vec::new();
        for (index, network) in self.networks.iter().enumerate() {
            let gone = matches!(
                network.status,
                LinkStatus::Disconnected { retry_in: None, .. } | LinkStatus::Failed { .. }
            );
            if network.demo || gone {
                continue;
            }
            if index == self.active {
                active = networks.len();
            }
            let buffers = sidebar::order(&network.state.buffers)
                .into_iter()
                .filter_map(|index| {
                    let buffer = &network.state.buffers[index];
                    let kind = match buffer.kind {
                        BufferKind::Channel if buffer.rejoin => SessionKind::Channel,
                        BufferKind::Query => SessionKind::Query,
                        _ => return None,
                    };
                    Some(SessionBuffer {
                        kind,
                        name: buffer.name.clone(),
                    })
                })
                .collect();
            networks.push(SessionNetwork {
                name: network.saved.clone().unwrap_or_default(),
                host: network.config.host.clone(),
                port: network.config.port,
                tls: network.config.tls,
                nick: network.state.nick.clone(),
                buffers,
                shown: if network.active == 0 {
                    String::new()
                } else {
                    network.buffer().name.clone()
                },
            });
        }
        Session { networks, active }
    }

    pub(super) fn session_changed(&mut self) {
        self.session_dirty = true;
    }

    /// Saves the session if it changed; the store keeps the newest.
    pub(super) fn save_session(&mut self) {
        if !std::mem::take(&mut self.session_dirty) || !self.keep_session || !self.startup.done {
            return;
        }
        if let Some(store) = &self.store {
            store.save(Document::Session, self.session().to_toml());
        }
    }

    pub(super) fn save_known_hosts(&self) {
        if let Some(store) = &self.store {
            store.save(Document::KnownHosts, self.known_hosts.to_text());
        }
    }

    /// Saves the saved networks and their secrets, unless the files failed
    /// to load: then they're left as they are.
    pub(super) fn save_networks(&mut self) -> Result<(), String> {
        if let Some(error) = &self.saved_error {
            return Err(format!(
                "networks.toml couldn't be read, so it isn't saved over ({error})"
            ));
        }
        if let Some(store) = &self.store {
            store.save(Document::Networks, self.saved.to_toml());
            store.save(Document::Secrets, self.saved.secrets_toml());
        }
        Ok(())
    }

    /// A server's certificate wasn't trusted: the prompt asks what to do.
    pub(super) fn certificate_refused(&mut self, id: NetworkId, problem: Box<CertificateProblem>) {
        if let Some(index) = self.index_of(id) {
            let text = if problem.changed {
                format!(
                    "The certificate of {} changed: {}. It may not be the server it claims to be.",
                    problem.host, problem.reason
                )
            } else {
                format!(
                    "The certificate of {} isn't trusted: {}.",
                    problem.host, problem.reason
                )
            };
            self.networks[index].push(0, LineKind::Error, "!", text);
            self.trust = Some((id, problem));
            self.forget_pointer_state();
            self.mark_all();
        }
    }

    /// The prompt's answer: trust once, pin for good, or not at all.
    pub(super) fn answer_certificate(&mut self, trust: Option<bool>) {
        let Some((id, problem)) = self.trust.take() else {
            return;
        };
        self.mark_all();
        let Some(always) = trust else {
            return;
        };
        if always {
            self.known_hosts
                .pin(&problem.host, problem.port, problem.fingerprint);
            self.save_known_hosts();
        }
        if let Some(network) = self.network_mut(id) {
            network.config.pins.push(problem.fingerprint);
            let config = network.config.clone();
            self.send(NetCommand::Connect(id, config));
        }
    }
}

/// Opens a network's buffers before it connects: the session's in their
/// order, then autojoin channels not among them; channels join on
/// registering. Shows `shown`, else the first channel.
fn open_buffers(
    network: &mut super::Network,
    buffers: &[SessionBuffer],
    autojoin: &[AutoJoin],
    shown: Option<&str>,
) {
    let state = &mut network.state;
    for buffer in buffers {
        let kind = match buffer.kind {
            SessionKind::Channel => BufferKind::Channel,
            SessionKind::Query => BufferKind::Query,
        };
        let (index, _) = state.open(kind, &buffer.name);
        state.buffers[index].rejoin = kind == BufferKind::Channel;
    }
    for join in autojoin {
        let (index, _) = state.open(BufferKind::Channel, &join.channel);
        let buffer = &mut state.buffers[index];
        buffer.rejoin = true;
        if !join.key.is_empty() && buffer.key_mode().is_none() {
            // Where rejoining looks for the key, until the server says.
            buffer.modes.push(('k', Some(join.key.clone())));
        }
    }
    network.active = match shown {
        Some("") => 0,
        Some(name) => state.find(name).unwrap_or(0),
        None => state
            .buffers
            .iter()
            .position(|buffer| buffer.kind == BufferKind::Channel)
            .unwrap_or(network.active),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_paths_resolve_like_the_design_says() {
        assert_eq!(
            certificate_path("/etc/irc/me.pem"),
            PathBuf::from("/etc/irc/me.pem")
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                certificate_path("~/me.pem"),
                PathBuf::from(home).join("me.pem")
            );
        }
        if let Some(dir) = paths::config_dir() {
            assert_eq!(certificate_path("libera.pem"), dir.join("certs/libera.pem"));
        }
    }
}
