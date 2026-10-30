//! The certificate trust prompt (DESIGN.md §6.2): what a refused
//! certificate says about itself and why it wasn't trusted, with **Trust
//! once**, **Always trust** (pin) and **Cancel**. A pinned certificate that
//! changed gets a prominent warning. Cancel has the focus, so Enter never
//! trusts by accident.

use crate::config::known_hosts::display;
use crate::net::CertificateProblem;
use crate::net::x509;
use crate::render::paragraph;
use crate::render::{Align, Painter, Point, Rect, Span};

use super::theme::{self, AMBER, MUTED, PINK, TEXT};
use super::widgets;

const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 344.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustAction {
    Once,
    Always,
    Cancel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrustLayout {
    pub panel: Rect,
    pub buttons: Vec<(TrustAction, Rect)>,
}

impl TrustLayout {
    pub fn new(area: Rect) -> Self {
        let width = WIDTH.min(area.width - 24.0).max(0.0).round();
        let height = HEIGHT.min(area.height - 24.0).max(0.0).round();
        let panel = Rect::new(
            (area.x + (area.width - width) / 2.0).round(),
            (area.y + (area.height - height) * 0.4).round(),
            width,
            height,
        );
        let mut x = panel.right() - 20.0;
        let mut buttons = Vec::new();
        for (action, width) in [
            (TrustAction::Cancel, 110.0),
            (TrustAction::Always, 140.0),
            (TrustAction::Once, 120.0),
        ] {
            x -= width;
            buttons.push((action, Rect::new(x, panel.bottom() - 46.0, width, 30.0)));
            x -= 10.0;
        }
        Self { panel, buttons }
    }

    pub fn hit(&self, point: Point) -> Option<TrustAction> {
        self.buttons
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(action, _)| *action)
    }

    /// The panel with its shadow.
    pub fn damage(&self) -> Rect {
        self.panel.inset(-10.0, -10.0)
    }
}

pub struct TrustView<'a> {
    pub problem: &'a CertificateProblem,
    pub focus: TrustAction,
    pub hover: Option<TrustAction>,
}

/// The fingerprint over two lines of sixteen bytes.
fn fingerprint_lines(problem: &CertificateProblem) -> (String, String) {
    let full = display(&problem.fingerprint);
    let (first, second) = full.split_at(16 * 3);
    (first.trim_end_matches(':').to_string(), second.to_string())
}

pub fn paint_trust(painter: &mut Painter<'_>, layout: &TrustLayout, view: &TrustView<'_>) {
    let panel = layout.panel;
    if panel.width < 200.0 || panel.height < 160.0 {
        return;
    }
    let problem = view.problem;
    widgets::frame(painter, panel, 18.0);
    painter.save_clip(panel);
    let tag = Rect::new(
        panel.x + 16.0,
        panel.y + 12.0,
        if problem.changed { 104.0 } else { 120.0 },
        22.0,
    );
    widgets::tag(
        painter,
        tag,
        if problem.changed {
            "WARNING"
        } else {
            "CERTIFICATE"
        },
    );
    let address = if problem.host.contains(':') {
        format!("[{}]:{}", problem.host, problem.port)
    } else {
        format!("{}:{}", problem.host, problem.port)
    };
    painter.label(
        &address,
        Rect::new(
            tag.right() + 12.0,
            tag.y,
            panel.width - tag.width - 60.0,
            tag.height,
        ),
        &theme::chrome(12.0),
        TEXT,
        Align::Start,
    );
    // Why, wrapped to the panel.
    let message = if problem.changed {
        format!(
            "This server's certificate changed since you trusted it: {}. Someone may be impersonating the server. Trust the new certificate only if you know why it changed.",
            problem.reason
        )
    } else {
        format!(
            "RubyChat can't verify this server's certificate: {}. Trust it only if you expected this.",
            problem.reason
        )
    };
    let style = theme::body(12.5);
    let text_x = panel.x + 20.0;
    let width = panel.width - 40.0;
    let spans = [Span::new(
        &message,
        style,
        if problem.changed { PINK } else { TEXT },
    )];
    let laid = paragraph::layout(painter.text, &spans, width);
    let mut y = panel.y + 46.0;
    for row in 0..laid.rows.len().min(3) {
        painter.draw_row(&laid, row, text_x, (y + laid.ascent).round());
        y += laid.line_height + 2.0;
    }
    y += 10.0;
    // The facts.
    let facts = problem.facts.as_ref();
    let dates = facts.and_then(|facts| Some((facts.not_before?, facts.not_after?)));
    let expired = dates.is_some_and(|(_, after)| after * 1000 < crate::platform::time::now_ms());
    let rows: Vec<(&str, String)> = vec![
        (
            "Issued to",
            facts.map_or_else(
                || String::from("(unreadable)"),
                |facts| facts.subject.display(),
            ),
        ),
        (
            "Issued by",
            facts.map_or_else(String::new, |facts| {
                if facts.self_signed() {
                    String::from("itself (self-signed)")
                } else {
                    facts.issuer.display()
                }
            }),
        ),
        (
            "Valid",
            dates.map_or_else(String::new, |(before, after)| {
                format!(
                    "{} to {}{}",
                    x509::date(before),
                    x509::date(after),
                    if expired { "  (expired)" } else { "" }
                )
            }),
        ),
        (
            "Names",
            facts.map_or_else(String::new, |facts| {
                facts
                    .names()
                    .into_iter()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        ),
        ("SHA-256", fingerprint_lines(problem).0),
        ("", fingerprint_lines(problem).1),
    ];
    let label_style = theme::chrome(10.0);
    let value_style = theme::body(12.0);
    for (label, value) in rows {
        let row = Rect::new(text_x, y, width, 20.0);
        painter.label(
            label,
            Rect::new(row.x, row.y, 90.0, row.height),
            &label_style,
            MUTED,
            Align::Start,
        );
        let color = if label == "Valid" && expired {
            AMBER
        } else {
            TEXT
        };
        painter.label(
            &value,
            Rect::new(row.x + 96.0, row.y, row.width - 96.0, row.height),
            &value_style,
            color,
            Align::Start,
        );
        y += 20.0;
    }
    for (action, rect) in &layout.buttons {
        let lit = view.focus == *action || view.hover == Some(*action);
        let (text, caution) = match action {
            TrustAction::Once => ("Trust once", false),
            TrustAction::Always => (
                if problem.changed {
                    "Trust new one"
                } else {
                    "Always trust"
                },
                problem.changed,
            ),
            TrustAction::Cancel => ("Cancel  Esc", false),
        };
        widgets::button(painter, *rect, text, lit, caution, true);
    }
    painter.restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    fn problem(changed: bool) -> CertificateProblem {
        CertificateProblem {
            host: String::from("irc.example.net"),
            port: 6697,
            fingerprint: [0xab; 32],
            reason: String::from("it signed itself, so no certificate authority vouches for it"),
            changed,
            facts: None,
        }
    }

    #[test]
    fn buttons_hit_and_the_panel_paints_within_its_damage() {
        let area = Rect::new(0.0, 26.0, 1000.0, 600.0);
        let layout = TrustLayout::new(area);
        assert_eq!(
            layout.hit(layout.buttons[0].1.center()),
            Some(TrustAction::Cancel)
        );
        assert_eq!(layout.hit(layout.panel.center()), None);
        let mut canvas = Canvas::new(1000, 626);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        for changed in [false, true] {
            let problem = problem(changed);
            paint_trust(
                &mut painter,
                &layout,
                &TrustView {
                    problem: &problem,
                    focus: TrustAction::Cancel,
                    hover: None,
                },
            );
        }
        let damage = layout.damage().snap(1.0);
        for y in 0..626 {
            for x in 0..1000 {
                if canvas.pixel(x, y) != 0 {
                    assert!(
                        damage.contains_rect(crate::render::IRect::new(
                            x as i32,
                            y as i32,
                            x as i32 + 1,
                            y as i32 + 1
                        )),
                        "ink at ({x}, {y})"
                    );
                }
            }
        }
        assert_eq!(fingerprint_lines(&problem(false)).0, ["AB"; 16].join(":"));
    }
}

#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib trust_snapshots -- --ignored`
    /// writes the prompt for a self-signed certificate and a changed pin.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn trust_snapshots() {
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        use rustls::pki_types::pem::PemObject;
        let der = rustls::pki_types::CertificateDer::from_pem_slice(include_bytes!(
            "../net/testdata/server.pem"
        ))
        .unwrap();
        let mut text = TextSystem::new();
        for changed in [false, true] {
            let problem = CertificateProblem {
                host: String::from("irc.example.net"),
                port: 6697,
                fingerprint: crate::net::fingerprint(der.as_ref()),
                reason: String::from(if changed {
                    "it isn't the pinned certificate"
                } else {
                    "it signed itself, so no certificate authority vouches for it"
                }),
                changed,
                facts: x509::parse(der.as_ref()),
            };
            let mut canvas = Canvas::new(1100, 720);
            let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
            let area = Rect::new(0.0, 26.0, 1100.0, 694.0);
            painter.fill_rect(area, super::super::theme::CANVAS);
            let layout = TrustLayout::new(area);
            paint_trust(
                &mut painter,
                &layout,
                &TrustView {
                    problem: &problem,
                    focus: TrustAction::Cancel,
                    hover: Some(TrustAction::Once),
                },
            );
            let name = if changed {
                "trust-changed.png"
            } else {
                "trust.png"
            };
            crate::render::png::write_rgba(
                &std::path::Path::new(&dir).join(name),
                canvas.width(),
                canvas.height(),
                &canvas.to_rgba(),
            )
            .unwrap();
        }
    }
}
