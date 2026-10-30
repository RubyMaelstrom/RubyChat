//! Benchmarks for DESIGN.md §10's budgets, at 2560 × 1440 and scale 1 with
//! a 5,000-line channel of 1,000 members:
//!
//! ```sh
//! cargo test --release --lib paint_budgets -- --ignored --nocapture
//! ```
//!
//! Optimized builds fail when a median breaks its budget; debug builds only
//! report.

use std::time::{Duration, Instant};

use crate::irc::casemap::Casemap;
use crate::model::buffer::{Buffer, BufferKind};
use crate::model::editor::LineEditor;
use crate::model::members::Member;
use crate::model::network::NetworkState;
use crate::model::{Line, LineKind};
use crate::render::{Canvas, Painter, Rect, TextSystem};

use super::chat_view::{self, ChatContext, ChatView};
use super::header::{HEADER_HEIGHT, HeaderView, paint_header};
use super::input::paint_input;
use super::nick_list::{NICK_LIST_WIDTH, paint_nick_list};
use super::sidebar::{SIDEBAR_WIDTH, SidebarLayout, SidebarView, Status, paint_sidebar};
use super::theme::CYAN;

const SIZE: (f32, f32) = (2560.0, 1440.0);
const RUNS: usize = 25;

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn measure(mut run: impl FnMut()) -> Duration {
    run();
    median(
        (0..RUNS)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed()
            })
            .collect(),
    )
}

fn channel() -> NetworkState {
    let mut state = NetworkState::new("ruby", "Libera.Chat");
    let (index, _) = state.open(BufferKind::Channel, "#rust");
    for n in 0..12 {
        state.open(BufferKind::Channel, &format!("#channel-{n}"));
    }
    let buffer = &mut state.buffers[index];
    buffer.joined = true;
    buffer.topic = Some(String::from(
        "Rust programming | https://www.rust-lang.org | Be kind",
    ));
    for n in 0..1000 {
        let prefix = match n {
            0..10 => "@",
            10..40 => "+",
            _ => "",
        };
        buffer.members.insert(
            Member::new(format!("member{n:04}"), prefix),
            &state.isupport,
        );
    }
    let texts = [
        "does anyone know why my iterator chain allocates? I collect() in the middle of it",
        "\u{2}collect()\u{2} in the middle, probably; try \u{1d}by_ref\u{1d} and keep it lazy",
        "see https://doc.rust-lang.org/std/iter/trait.Iterator.html#method.by_ref and #rust-beginners",
        "\u{3}04heads up:\u{3} the nightly from today breaks \u{1f}no_std\u{1f} builds for thumbv7em targets",
    ];
    let start = crate::platform::time::now_ms() - 5000 * 20_000;
    for n in 0..5000 {
        let time = start + n as i64 * 20_000;
        let line = if n % 7 == 0 {
            Line::new(
                time,
                LineKind::Event,
                "→",
                format!("member{:04} (u@host) joined", n % 1000),
            )
        } else {
            Line::message(
                time,
                LineKind::Message,
                format!("member{:04}", n % 1000),
                texts[n % texts.len()],
            )
        };
        buffer.push(line);
    }
    state
}

#[test]
#[ignore = "benchmark; run optimized with --nocapture"]
fn paint_budgets() {
    let mut state = channel();
    let index = state.find("#rust").unwrap();
    let mut canvas = Canvas::new(SIZE.0 as u32, SIZE.1 as u32);
    let mut text = TextSystem::new();
    let body = Rect::new(0.0, 26.0, SIZE.0, SIZE.1 - 26.0);
    let sidebar = Rect::new(0.0, body.y, SIDEBAR_WIDTH, body.height);
    let nicks = Rect::new(
        SIZE.0 - NICK_LIST_WIDTH,
        body.y,
        NICK_LIST_WIDTH,
        body.height,
    );
    let center = Rect::new(
        sidebar.right(),
        body.y,
        nicks.x - sidebar.right(),
        body.height,
    );
    let header = Rect::new(center.x, center.y, center.width, HEADER_HEIGHT);
    let input = Rect::new(center.x, center.bottom() - 52.0, center.width, 52.0);
    let chat = Rect::new(
        center.x,
        header.bottom(),
        center.width,
        input.y - header.bottom(),
    );
    let status = Status {
        text: String::from("TLS · 38 MS"),
        color: CYAN,
    };
    let context = |buffer: &Buffer| ChatContext {
        casemap: Casemap::Rfc1459,
        chantypes: "#",
        marker: buffer.marker,
        label_chars: buffer.label_chars,
    };

    // Switching to the buffer paints everything below the strip.
    let switch = measure(|| {
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let layout = SidebarLayout::new(sidebar, &state.buffers, 0.0);
        let view = SidebarView {
            title: "Libera.Chat",
            status: &status,
            buffers: &state.buffers,
            server: "irc.libera.chat",
            active: index,
            hovered: None,
            focused: true,
        };
        paint_sidebar(&mut painter, &layout, &view);
        let buffer = &state.buffers[index];
        let detail = format!(
            "{} · {} members",
            buffer.mode_string(),
            buffer.members.len()
        );
        paint_header(
            &mut painter,
            header,
            &HeaderView {
                name: &buffer.name,
                detail: &detail,
                topic: buffer.topic.as_deref(),
                placeholder: "No topic",
            },
        );
        let view = ChatView {
            lines: buffer.lines(),
            context: context(buffer),
            anchor: None,
            selection: None,
            hovered: None,
            focused: true,
        };
        chat_view::paint_chat(&mut painter, chat, &view, Default::default(), false);
        let mut editor = LineEditor::default();
        paint_input(
            &mut painter,
            input.inset(12.0, 8.0),
            &mut editor,
            "ruby›",
            "Message #rust",
            true,
        );
        let buffer = &mut state.buffers[index];
        let members: Vec<&Member> = buffer.members.sorted(&state.isupport).collect();
        paint_nick_list(&mut painter, nicks, &members, 0.0, None);
    });

    // A scrolling frame: the anchor moves, then the view repaints.
    let mut anchor = None;
    let mut frame = chat_view::ChatFrame::default();
    let scroll = measure(|| {
        let buffer = &state.buffers[index];
        anchor = chat_view::scroll(
            &mut text,
            chat,
            &context(buffer),
            buffer.lines(),
            anchor,
            60.0,
        );
        let view = ChatView {
            lines: buffer.lines(),
            context: context(buffer),
            anchor,
            selection: None,
            hovered: None,
            focused: true,
        };
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        frame = chat_view::paint_chat(&mut painter, chat, &view, std::mem::take(&mut frame), true);
        anchor = frame.anchor;
    });

    // A keystroke repaints the input field.
    let mut editor = LineEditor::default();
    editor.insert("hello there, this is a message being typed into the input line");
    let keystroke = measure(|| {
        editor.insert("x");
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        paint_input(
            &mut painter,
            input.inset(12.0, 8.0),
            &mut editor,
            "ruby›",
            "Message #rust",
            true,
        );
    });

    eprintln!("paint budgets at 2560 × 1440 (median of {RUNS}):");
    for (name, took, budget) in [
        ("keystroke", keystroke, Duration::from_millis(1)),
        ("switching buffer", switch, Duration::from_millis(4)),
        ("scrolling frame", scroll, Duration::from_millis(3)),
    ] {
        eprintln!(
            "  {name:<18} {:>7.3} ms  (budget {} ms)",
            took.as_secs_f64() * 1e3,
            budget.as_millis()
        );
        if cfg!(not(debug_assertions)) {
            assert!(
                took <= budget,
                "{name} took {took:?}, over its {budget:?} budget"
            );
        }
    }
}
