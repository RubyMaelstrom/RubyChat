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
