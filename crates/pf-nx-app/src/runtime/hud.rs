//! The stats overlay: core's own lines (`hud::format`) on a dark plate, top left, refreshed once
//! a second. Core samples for the whole session, so every tenth second's window also goes to the
//! log, whether the overlay shows or not.

use std::time::{Duration, Instant};

use pf_console_ui::theme::{Fonts, W};
use punktfunk_core::hud::{self, HudLine, Role, StatsVerbosity};
use skia_safe::{Canvas, Color4f, Paint, RRect, Rect};

use crate::session::Connected;

const REFRESH: Duration = Duration::from_secs(1);
const LOG_EVERY: Duration = Duration::from_secs(10);

pub(super) struct StatsOverlay {
    pub tier: StatsVerbosity,
    advanced: bool,
    lines: Vec<HudLine>,
    taken: Option<Instant>,
    logged: Instant,
    /// Lines this client adds: the lane, pacing misses, the audio ring.
    pub local: Vec<String>,
}

impl StatsOverlay {
    pub fn new(tier: StatsVerbosity, advanced: bool) -> Self {
        Self {
            tier,
            advanced,
            lines: Vec::new(),
            taken: None,
            logged: Instant::now(),
            local: Vec::new(),
        }
    }

    /// Steps to the next tier.
    pub fn cycle(&mut self) {
        self.tier = self.tier.next();
        self.taken = None;
    }

    /// Takes a new window when one is due. Every [`LOG_EVERY`] it returns that window's lines at
    /// the detailed tier, joined, for the log.
    pub fn refresh(&mut self, connected: &Connected, decoder: &str) -> Option<String> {
        if self.taken.is_some_and(|t| t.elapsed() < REFRESH) {
            return None;
        }
        self.taken = Some(Instant::now());
        let snap = connected.hud_snapshot(decoder);
        self.lines = hud::format(&snap, self.tier, self.advanced);
        if self.logged.elapsed() < LOG_EVERY {
            return None;
        }
        self.logged = Instant::now();
        let lines = hud::format(&snap, StatsVerbosity::Detailed, true);
        Some(lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join(" | "))
    }

    pub fn draw(&self, canvas: &Canvas, fonts: &Fonts, scale: f32) {
        if self.tier == StatsVerbosity::Off {
            return;
        }
        let size = f64::from(13.0 * scale);
        let line_h = (size * 1.3) as f32;
        let pad = 10.0 * scale;
        let texts: Vec<(&str, Role)> = self
            .lines
            .iter()
            .map(|l| (l.text.as_str(), l.role))
            .chain(self.local.iter().map(|t| (t.as_str(), Role::Muted)))
            .collect();
        let width = texts
            .iter()
            .map(|(t, _)| fonts.measure(t, W::Medium, size))
            .fold(0.0f32, f32::max);
        let height = line_h * texts.len() as f32;
        let plate = Rect::from_xywh(pad, pad, width + pad * 2.0, height + pad * 2.0);
        let mut paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.62), None);
        paint.set_anti_alias(true);
        canvas.draw_rrect(RRect::new_rect_xy(plate, 8.0 * scale, 8.0 * scale), &paint);
        for (i, (text, role)) in texts.iter().enumerate() {
            let baseline = f64::from(pad * 2.0 + line_h * (i as f32 + 0.8));
            fonts.draw(
                canvas,
                text,
                f64::from(pad * 2.0),
                baseline,
                W::Medium,
                size,
                colour(*role),
            );
        }
    }
}

fn colour(role: Role) -> Color4f {
    match role {
        Role::Warn => Color4f::new(1.0, 0.78, 0.3, 1.0),
        Role::Muted => Color4f::new(0.75, 0.75, 0.78, 1.0),
        _ => Color4f::new(1.0, 1.0, 1.0, 1.0),
    }
}
