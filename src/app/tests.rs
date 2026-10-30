//! The app without a window or threads: commands, completion, buffer
//! switching and routing, with what it would send captured.

use super::*;

fn app() -> App {
    let options = Options {
        isolated: true,
        ..Options::default()
    };
    App::new(options, || -> Wake { Box::new(|| {}) }).unwrap()
}

/// A network registered as `ruby`, shown.
fn registered(app: &mut App) -> NetworkId {
    let id = app.add_network(
        String::from("Test"),
        ConnectConfig::new("irc.test", 6697, true, "ruby"),
    );
    app.active = app.index_of(id).unwrap();
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    id
}

fn receive(app: &mut App, id: NetworkId, line: &str) {
    app.apply(NetEvent::Received(id, Message::parse(line).unwrap()));
}

/// Lines sent since last asked.
fn sent(app: &App) -> Vec<String> {
    app.sent
        .borrow_mut()
        .drain(..)
        .filter_map(|command| match command {
            NetCommand::Send(_, line) => Some(line),
            _ => None,
        })
        .collect()
}

fn type_line(app: &mut App, text: &str) {
    app.editor().set(text);
    app.submit();
}

fn network(app: &App) -> &Network {
    &app.networks[app.active]
}

fn shown(app: &App) -> &str {
    &network(app).buffer().name
}

#[test]
fn text_goes_to_the_channel_with_local_echo() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    assert_eq!(
        network(&app).active,
        0,
        "a join from the server opens the buffer in the background"
    );
    let rust = network(&app).state.find("#rust").unwrap();
    app.activate_buffer(rust);
    type_line(&mut app, "hello there");
    assert_eq!(sent(&app), ["PRIVMSG #rust :hello there"]);
    let last = network(&app).buffer().lines().back().unwrap();
    assert!(last.own && last.text == "hello there" && last.nick == "ruby");
    type_line(&mut app, "/me waves");
    type_line(&mut app, "//slash");
    assert_eq!(
        sent(&app),
        [
            "PRIVMSG #rust :\u{1}ACTION waves\u{1}",
            "PRIVMSG #rust /slash"
        ]
    );
    // The server buffer has no one to talk to.
    app.activate_buffer(0);
    type_line(&mut app, "hello?");
    assert!(sent(&app).is_empty());
    assert_eq!(
        network(&app).buffer().lines().back().unwrap().kind,
        LineKind::Error
    );
}

#[test]
fn long_messages_are_split_to_fit_a_line() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    app.activate_buffer(1);
    let text = "lorem ipsum dolor sit amet ".repeat(40);
    type_line(&mut app, &text);
    let lines = sent(&app);
    assert!(lines.len() >= 3);
    for line in &lines {
        assert!(line.starts_with("PRIVMSG #rust :"));
        // Room for the relayed prefix stays under the 512-byte limit.
        assert!(
            line.len() + 2 + commands::RELAY_PREFIX + "ruby".len() <= 512,
            "{}",
            line.len()
        );
    }
    let echoed: Vec<&str> = network(&app)
        .buffer()
        .lines()
        .iter()
        .filter(|l| l.own)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(echoed.len(), lines.len(), "each piece shows as sent");
}

#[test]
fn joining_opens_at_once_and_parting_closes() {
    let mut app = app();
    let id = registered(&mut app);
    type_line(&mut app, "/join rust,#labwc");
    assert_eq!(sent(&app), ["JOIN #rust,#labwc"]);
    assert_eq!(shown(&app), "#rust", "the first channel shows at once");
    let header_joined = network(&app).buffer().joined;
    assert!(!header_joined, "joining until the server agrees");
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    assert!(network(&app).buffer().joined);
    // Joining an open channel just shows it.
    app.activate_buffer(0);
    type_line(&mut app, "/join #rust");
    assert!(sent(&app).is_empty());
    assert_eq!(shown(&app), "#rust");
    type_line(&mut app, "/part see you");
    assert_eq!(sent(&app), ["PART #rust :see you"]);
    assert!(network(&app).state.find("#rust").is_none());
    assert_eq!(shown(&app), "Test", "the row above the closed one shows");
    // #labwc was never joined: closing it sends nothing.
    let labwc = network(&app).state.find("#labwc").unwrap();
    app.close_buffer(labwc);
    assert!(sent(&app).is_empty());
}

#[test]
fn channels_rejoin_after_reconnecting() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":ruby!u@h JOIN #labwc");
    receive(&mut app, id, ":op!o@h KICK #labwc ruby :bye");
    let reason = String::from("Connection reset");
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Disconnected {
            reason,
            retry_in: None,
        },
    ));
    assert!(!network(&app).state.buffers[1].joined);
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    assert_eq!(sent(&app), ["JOIN #rust"], "not where you were kicked");
    // Joining while disconnected waits for the connection.
    app.apply(NetEvent::Status(id, LinkStatus::Connecting));
    type_line(&mut app, "/join #later");
    assert!(sent(&app).is_empty());
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    assert_eq!(sent(&app), ["JOIN #rust,#later"]);
}

#[test]
fn queries_messages_and_whois_replies_go_where_they_belong() {
    let mut app = app();
    let id = registered(&mut app);
    type_line(&mut app, "/query alice hi there");
    assert_eq!(shown(&app), "alice");
    assert_eq!(sent(&app), ["PRIVMSG alice :hi there"]);
    app.activate_buffer(0);
    type_line(&mut app, "/msg alice also this");
    type_line(&mut app, "/msg bob not open");
    assert_eq!(
        sent(&app),
        ["PRIVMSG alice :also this", "PRIVMSG bob :not open"]
    );
    let alice = network(&app).state.find("alice").unwrap();
    assert_eq!(
        network(&app).state.buffers[alice]
            .lines()
            .back()
            .unwrap()
            .text,
        "also this"
    );
    assert_eq!(
        network(&app).buffer().lines().back().unwrap().text,
        "*bob* not open",
        "shown here"
    );
    app.activate_buffer(alice);
    type_line(&mut app, "/whois alice");
    assert_eq!(sent(&app), ["WHOIS alice"]);
    receive(&mut app, id, ":srv 311 ruby alice a host * :Alice A.");
    assert_eq!(
        network(&app).buffer().lines().back().unwrap().text,
        "alice is a@host (Alice A.)"
    );
}

#[test]
fn tab_completes_nicks_channels_and_commands() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(
        &mut app,
        id,
        ":srv 353 ruby = #rust :@ferris alice albert ruby",
    );
    receive(&mut app, id, ":srv 366 ruby #rust :End of /NAMES list.");
    receive(&mut app, id, ":albert!a@h PRIVMSG #rust :I spoke last");
    app.activate_buffer(1);
    let input = |app: &App| network(app).buffer().input.text().to_string();
    app.editor().set("al");
    app.complete(false);
    assert_eq!(
        input(&app),
        "albert: ",
        "recent speakers first, with ': ' at the start"
    );
    app.complete(false);
    assert_eq!(input(&app), "alice: ");
    app.complete(true);
    assert_eq!(input(&app), "albert: ");
    app.completion = None;
    app.editor().set("thanks fer");
    app.complete(false);
    assert_eq!(input(&app), "thanks ferris ");
    app.completion = None;
    app.editor().set("/jo");
    app.complete(false);
    assert_eq!(input(&app), "/join ");
    app.completion = None;
    app.editor().set("see #ru");
    app.complete(false);
    assert_eq!(input(&app), "see #rust ");
    app.completion = None;
    app.editor().set("ru");
    app.complete(false);
    assert_eq!(input(&app), "ru", "never yourself");
}

#[test]
fn buffers_switch_in_sidebar_order_and_keep_their_drafts() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":bob!b@h PRIVMSG ruby :psst");
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    // Sidebar order: server, channels, then queries.
    app.activate_nth_buffer(1);
    assert_eq!(shown(&app), "#rust");
    app.editor().set("half-typed");
    app.cycle_buffer(1);
    assert_eq!(shown(&app), "bob");
    assert!(network(&app).buffer().input.is_empty());
    app.cycle_buffer(1);
    assert_eq!(shown(&app), "Test", "wrapping around");
    app.cycle_buffer(-1);
    app.cycle_buffer(-1);
    assert_eq!(network(&app).buffer().input.text(), "half-typed");
}

#[test]
fn unread_counts_and_badges_follow_what_is_seen() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":alice!a@h PRIVMSG #rust :ruby: ping");
    let rust = network(&app).state.find("#rust").unwrap();
    assert_eq!(
        network(&app).state.buffers[rust].highlights,
        1,
        "not shown, so unread"
    );
    assert_eq!(network(&app).badge(), (1, true));
    assert_eq!(
        network(&app).tab(true).highlights,
        0,
        "a shown network's tab shows no badge"
    );
    assert_eq!(network(&app).tab(false).highlights, 1);
    app.activate_buffer(rust);
    // No window: nothing is seen until one opens with focus.
    assert_eq!(network(&app).state.buffers[rust].highlights, 1);
    let marker = network(&app).buffer().marker;
    assert!(marker.is_some(), "the rule marks where unread lines begin");
}

#[test]
fn copying_takes_the_chat_selection_else_the_input_selection() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":alice!a@h PRIVMSG #rust :copy this text");
    app.activate_buffer(1);
    let line = network(&app).buffer().lines().back().unwrap().id;
    app.selection = Some(Selection {
        anchor: TextPosition {
            id: line,
            offset: 9,
        },
        focus: TextPosition {
            id: line,
            offset: 5,
        },
        dragging: false,
    });
    app.copy();
    assert_eq!(app.copied, "this");
    app.selection = None;
    app.editor().set("draft words");
    app.editor().select_all();
    app.cut();
    assert_eq!(
        (app.copied.as_str(), network(&app).buffer().input.text()),
        ("draft words", "")
    );
    app.insert_pasted("one\ntwo\r\n");
    assert_eq!(network(&app).buffer().input.text(), "one two");
}

#[test]
fn closing_networks_and_demo_networks_never_send() {
    let mut app = app();
    app.open_demo();
    assert_eq!(app.networks.len(), 4);
    type_line(&mut app, "hello");
    assert!(sent(&app).is_empty());
    assert_eq!(
        network(&app).buffer().lines().back().unwrap().kind,
        LineKind::Error
    );
    let id = network(&app).id;
    app.close_network(app.active);
    assert!(app.networks.iter().all(|n| n.id != id));
    assert!(
        app.sent.borrow().is_empty(),
        "demo networks have nothing to close"
    );
}

#[test]
fn menus_offer_what_fits_and_act_through_commands() {
    let mut app = app();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":srv 353 ruby = #rust :ruby alice +bob");
    receive(&mut app, id, ":srv 366 ruby #rust :End of /NAMES list.");
    let rust = network(&app).state.find("#rust").unwrap();
    app.activate_buffer(rust);
    let at = Point::new(300.0, 200.0);
    app.open_nick_menu("alice", at);
    let labels = app.menu_labels();
    let enabled = |label: &str| labels.iter().find(|(l, _)| l == label).map(|(_, e)| *e);
    assert_eq!(enabled("Open a query"), Some(true));
    assert_eq!(enabled("Make operator"), Some(false), "only operators may");
    assert_eq!(enabled("Kick from channel"), Some(false));
    receive(&mut app, id, ":ChanServ!s@services MODE #rust +o ruby");
    app.open_nick_menu("bob", at);
    let labels = app.menu_labels();
    assert!(
        labels.contains(&(String::from("Take voice"), true)),
        "{labels:?}"
    );
    app.choose_labeled("Take voice");
    assert_eq!(sent(&app), ["MODE #rust -v bob"]);
    assert!(app.menu.is_none(), "choosing closes the menu");
    // The keyboard skips what is disabled.
    app.open_nick_menu("ruby", at);
    let labels = app.menu_labels();
    assert!(
        !labels
            .iter()
            .any(|(l, _)| l == "Open a query" || l == "Kick from channel"),
        "not for yourself: {labels:?}"
    );
    use winit::keyboard::{Key, NamedKey};
    assert!(app.menu_key(&Key::Named(NamedKey::ArrowUp)));
    assert!(app.menu_key(&Key::Named(NamedKey::Enter)));
    assert_eq!(
        sent(&app),
        ["MODE #rust +v ruby"],
        "up from nothing is the last item"
    );
    // A channel's row offers leaving it.
    app.open_buffer_menu(rust, at);
    assert_eq!(app.menu_labels().last().unwrap().0, "Leave #rust");
    app.choose_labeled("Leave #rust");
    assert_eq!(sent(&app), ["PART #rust"]);
    assert!(network(&app).state.find("#rust").is_none());
    app.open_link_menu(Target::Url(String::from("https://example.org")), at);
    app.choose_labeled("Copy link address");
    assert_eq!(app.copied, "https://example.org");
}

#[test]
fn an_irc_url_opens_its_channel_and_joins_with_its_key() {
    let mut app = app();
    let server =
        commands::parse_server("ircs://:secret@irc.example.net/%23foobar?key=bazqux").unwrap();
    app.connect(server);
    let NetCommand::Connect(id, config) = app.sent.borrow_mut().remove(0) else {
        panic!("connects first");
    };
    assert_eq!(config.password.as_deref(), Some("secret"));
    assert_eq!(shown(&app), "#foobar", "the channel shows at once");
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    assert_eq!(sent(&app), ["JOIN #foobar bazqux"]);
}

fn saved_libera() -> crate::config::networks::SavedNetwork {
    use crate::config::networks::{AutoJoin, Login, LoginMethod, SavedNetwork, SavedServer};
    SavedNetwork {
        name: String::from("Libera.Chat"),
        servers: vec![
            SavedServer::new("irc.libera.chat", 6697, true),
            SavedServer::new("irc.eu.libera.chat", 6697, true),
        ],
        nick: String::from("ruby"),
        login: Login {
            method: LoginMethod::SaslPlain,
            account: String::from("ruby"),
            password: String::from("hunter2"),
            require: true,
            client_certificate: String::new(),
        },
        autojoin: vec![
            AutoJoin {
                channel: String::from("#rust"),
                key: String::new(),
            },
            AutoJoin {
                channel: String::from("#secret"),
                key: String::from("sesame"),
            },
        ],
        commands: vec![String::from("/mode ruby +R")],
        part_message: String::from("bye for now"),
        ..SavedNetwork::default()
    }
}

#[test]
fn saved_networks_connect_with_their_logins_channels_and_commands() {
    let mut app = app();
    app.known_hosts.pin("irc.libera.chat", 6697, [7; 32]);
    app.connect_saved(&saved_libera(), None).unwrap();
    let NetCommand::Connect(id, config) = app.sent.borrow_mut().remove(0) else {
        panic!("connects first");
    };
    assert!(
        matches!(&config.login, crate::net::Login::Sasl { account, password, .. } if account == "ruby" && password == "hunter2")
    );
    assert!(config.require_login);
    assert_eq!(config.pins, [[7; 32]], "pinned in known_hosts");
    assert_eq!(shown(&app), "#rust", "the first channel shows at once");
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    assert_eq!(
        sent(&app),
        ["JOIN #secret,#rust sesame", "MODE ruby +R"],
        "keyed channels first, then the commands"
    );
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    let rust = network(&app).state.find("#rust").unwrap();
    app.close_buffer(rust);
    assert_eq!(sent(&app), ["PART #rust :bye for now"]);
}

#[test]
fn a_failing_server_hands_over_to_the_next() {
    let mut app = app();
    app.saved.list.push(saved_libera());
    app.connect_saved(&saved_libera(), None).unwrap();
    let id = network(&app).id;
    app.sent.borrow_mut().clear();
    let reason = String::from("Connection refused");
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Disconnected {
            reason,
            retry_in: Some(Duration::from_secs(2)),
        },
    ));
    let commands = app.sent.borrow_mut().drain(..).collect::<Vec<_>>();
    assert!(
        matches!(commands.as_slice(), [NetCommand::Update(_, config)] if config.host == "irc.eu.libera.chat"),
        "{commands:?}"
    );
}

#[test]
fn sessions_restore_what_was_open() {
    let mut app = app();
    app.saved.list.push(saved_libera());
    app.connect_saved(&saved_libera(), None).unwrap();
    let id = network(&app).id;
    app.apply(NetEvent::Status(
        id,
        LinkStatus::Registered {
            nick: String::from("ruby"),
        },
    ));
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":bob!b@h PRIVMSG ruby :hi");
    let quick = commands::parse_server("irc://irc.example.net/#chat").unwrap();
    app.connect(quick);
    app.activate(0);
    let bob = network(&app).state.find("bob").unwrap();
    app.activate_buffer(bob);
    let session = app.session();
    assert_eq!(session.networks.len(), 2);
    assert_eq!(
        (
            session.networks[0].name.as_str(),
            session.networks[0].shown.as_str(),
            session.active
        ),
        ("Libera.Chat", "bob", 0)
    );
    let names: Vec<&str> = session.networks[0]
        .buffers
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert_eq!(names, ["#rust", "#secret", "bob"]);
    // The next start opens the same.
    let mut next = super::tests::app();
    next.saved.list.push(saved_libera());
    next.keep_session = true;
    next.startup.session = Some(Some(session.clone()));
    next.start();
    assert_eq!(next.networks.len(), 2);
    assert_eq!(next.active, 0);
    assert_eq!(shown(&next), "bob");
    let restored = next.session();
    assert_eq!(restored.networks[0].buffers, session.networks[0].buffers);
    assert_eq!(restored.networks[1].host, "irc.example.net");
}

#[test]
fn the_trust_prompt_trusts_once_or_pins() {
    let mut app = app();
    let id = registered(&mut app);
    let problem = || {
        Box::new(crate::net::CertificateProblem {
            host: String::from("irc.test"),
            port: 6697,
            fingerprint: [9; 32],
            reason: String::from("it signed itself"),
            changed: false,
            facts: None,
        })
    };
    app.certificate_refused(id, problem());
    assert!(app.trust.is_some());
    app.answer_certificate(None);
    assert!(app.sent.borrow().is_empty(), "cancelled: nothing happens");
    app.certificate_refused(id, problem());
    app.answer_certificate(Some(false));
    let commands = app.sent.borrow_mut().drain(..).collect::<Vec<_>>();
    assert!(
        matches!(commands.as_slice(), [NetCommand::Connect(_, config)] if config.pins.contains(&[9; 32]))
    );
    assert_eq!(
        app.known_hosts.get("irc.test", 6697),
        None,
        "trusted once, not pinned"
    );
    app.certificate_refused(id, problem());
    app.answer_certificate(Some(true));
    assert_eq!(app.known_hosts.get("irc.test", 6697), Some([9; 32]));
}

/// What went to the desktop thread since last asked.
fn desktop(app: &mut App) -> Vec<crate::platform::desktop::DesktopCommand> {
    std::mem::take(&mut app.shell.sent)
}

#[test]
fn closing_goes_to_the_tray_only_when_one_shows_rubychat() {
    use crate::platform::desktop::{DesktopCommand, DesktopEvent};
    let mut app = app();
    app.close_window();
    assert!(app.quit, "without a tray, closing quits");
    let mut app = app_with_tray();
    app.close_window();
    assert!(!app.quit && app.shell.hidden, "closed to the tray");
    let explained = desktop(&mut app)
        .into_iter()
        .filter(|command| matches!(command, DesktopCommand::Notify(notice) if notice.key.0 == 0))
        .count();
    assert_eq!(explained, 1, "the first time says RubyChat still runs");
    app.show_window(None);
    app.close_window();
    assert!(desktop(&mut app).is_empty(), "only the first time");
    // The tray going away brings the window back.
    app.desktop_event(DesktopEvent::Tray(false));
    assert!(!app.shell.hidden);
}

fn app_with_tray() -> App {
    let mut app = app();
    app.desktop_event(crate::platform::desktop::DesktopEvent::Tray(true));
    app
}

#[test]
fn highlights_notify_and_a_click_opens_their_buffer() {
    use crate::platform::desktop::{DesktopCommand, DesktopEvent};
    let mut app = app_with_tray();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    app.close_window();
    desktop(&mut app);
    receive(
        &mut app,
        id,
        ":ferris!f@h PRIVMSG #rust :ruby: look at this",
    );
    receive(&mut app, id, ":ferris!f@h PRIVMSG #rust :no mention here");
    receive(
        &mut app,
        id,
        ":alice!a@h PRIVMSG ruby :\u{1}ACTION waves\u{1}",
    );
    let notices: Vec<_> = desktop(&mut app)
        .into_iter()
        .filter_map(|command| match command {
            DesktopCommand::Notify(notice) => Some(notice),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].key, (id.0, String::from("#rust")));
    assert_eq!(notices[0].title, "#rust · Test");
    assert_eq!(
        (notices[0].nick.as_str(), notices[0].text.as_str()),
        ("ferris", "ruby: look at this")
    );
    assert_eq!(
        (notices[1].nick.as_str(), notices[1].text.as_str()),
        ("", "* alice waves")
    );
    // Playback from a bouncer, by server time, doesn't notify.
    receive(
        &mut app,
        id,
        "@time=2020-01-01T00:00:00.000Z :ferris!f@h PRIVMSG #rust :ruby: old news",
    );
    assert!(desktop(&mut app).is_empty());
    app.desktop_event(DesktopEvent::Open(
        (id.0, String::from("#rust")),
        Some(String::from("token")),
    ));
    assert_eq!(shown(&app), "#rust");
    assert!(!app.shell.hidden, "the window comes back");
    assert_eq!(app.shell.token.as_deref(), Some("token"));
    // Marking all read withdraws what's up.
    app.desktop_event(DesktopEvent::Menu(
        crate::platform::desktop::MenuAction::MarkAllRead,
    ));
    let withdrawn = desktop(&mut app)
        .into_iter()
        .filter(|command| matches!(command, DesktopCommand::Withdraw(_)))
        .count();
    assert_eq!(withdrawn, 2);
    assert!(network(&app).state.buffers.iter().all(|b| b.unread == 0));
}

#[test]
fn the_tray_hears_of_unread_messages_and_away() {
    use crate::platform::desktop::{Attention, DesktopCommand, DesktopEvent, MenuAction};
    let mut app = app_with_tray();
    let id = registered(&mut app);
    receive(&mut app, id, ":ruby!u@h JOIN #rust");
    receive(&mut app, id, ":ferris!f@h PRIVMSG #rust :hello");
    app.sync_tray();
    let state = desktop(&mut app)
        .into_iter()
        .find_map(|command| match command {
            DesktopCommand::Tray(state) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.attention, Attention::Unread);
    assert_eq!(state.tooltip, "1 unread");
    app.sync_tray();
    assert!(desktop(&mut app).is_empty(), "sent only when it changes");
    app.desktop_event(DesktopEvent::Menu(MenuAction::Away));
    assert_eq!(sent(&app), ["AWAY :Away"]);
    receive(
        &mut app,
        id,
        ":irc.test 306 ruby :You have been marked as being away",
    );
    assert!(app.shell.away);
    app.desktop_event(DesktopEvent::Menu(MenuAction::Away));
    assert_eq!(sent(&app), ["AWAY"]);
    app.desktop_event(DesktopEvent::Menu(MenuAction::Quit));
    assert!(app.quit);
}

#[test]
fn a_later_launchs_link_joins_on_the_network_already_open() {
    use crate::platform::instance::Request;
    let mut app = app();
    let id = registered(&mut app);
    app.handed_over(Request {
        show: false,
        addresses: vec![String::from("ircs://irc.test:6697/rust?key=sesame")],
        token: Some(String::from("token")),
    });
    assert_eq!(app.networks.len(), 1, "no second connection");
    assert_eq!(sent(&app), ["JOIN #rust sesame"]);
    assert_eq!(network(&app).id, id);
    assert_eq!(
        app.shell.token.as_deref(),
        Some("token"),
        "it shows the window"
    );
    // Another server connects.
    app.handed_over(Request {
        show: true,
        addresses: vec![String::from("irc.example.org")],
        token: None,
    });
    assert_eq!(app.networks.len(), 2);
}
