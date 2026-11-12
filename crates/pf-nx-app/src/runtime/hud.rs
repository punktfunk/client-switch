//! The stats overlay: core's own lines (`hud::format`) on a dark plate, top left, refreshed once
//! a second. Core samples for the whole session, so every tenth second's window also goes to the
//! log, whether the overlay shows or not. The stream-start notice sits on the same plate.

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
    /// Lines this client adds: the lane, pacing misses, the audio ring.
    local: Vec<String>,
    /// The widest line at a scale, measured once per change of lines.
    widest: Option<(f32, f32)>,
    taken: Option<Instant>,
    logged: Instant,
}

impl StatsOverlay {
    pub fn new(tier: StatsVerbosity, advanced: bool) -> Self {
        Self {
            tier,
            advanced,
            lines: Vec::new(),
            local: Vec::new(),
            widest: None,
            taken: None,
            logged: Instant::now(),
        }
    }

    /// Steps to the next tier.
    pub fn cycle(&mut self) {
        self.tier = self.tier.next();
        self.taken = None;
    }

    /// Whether [`Self::refresh`] takes a new window this frame.
    pub fn due(&self) -> bool {
        !self.taken.is_some_and(|t| t.elapsed() < REFRESH)
    }

    /// Takes a new window when one is due. Every [`LOG_EVERY`] it returns that window's lines at
    /// the detailed tier, joined, for the log.
    pub fn refresh(&mut self, connected: &Connected, decoder: &str) -> Option<String> {
        if !self.due() {
            return None;
        }
        self.taken = Some(Instant::now());
        let snap = connected.hud_snapshot(decoder);
        self.lines = hud::format(&snap, self.tier, self.advanced);
        self.widest = None;
        if self.logged.elapsed() < LOG_EVERY {
            return None;
        }
        self.logged = Instant::now();
        let lines = hud::format(&snap, StatsVerbosity::Detailed, true);
        Some(lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join(" | "))
    }

    pub fn set_local(&mut self, local: Vec<String>) {
        if local != self.local {
            self.local = local;
            self.widest = None;
        }
    }

    fn texts(&self) -> impl Iterator<Item = (&str, Role)> {
        let local = self.local.iter().map(|t| (t.as_str(), Role::Muted));
        self.lines.iter().map(|l| (l.text.as_str(), l.role)).chain(local)
    }

    pub fn draw(&mut self, canvas: &Canvas, fonts: &Fonts, scale: f32) {
        if self.tier == StatsVerbosity::Off {
            return;
        }
        let size = f64::from(13.0 * scale);
        let line_h = (size * 1.3) as f32;
        let pad = 10.0 * scale;
        let width = match self.widest.filter(|&(at, _)| at == scale) {
            Some((_, width)) => width,
            None => {
                let width = self
                    .texts()
                    .map(|(t, _)| fonts.measure(t, W::Medium, size))
                    .fold(0.0f32, f32::max);
                self.widest = Some((scale, width));
                width
            }
        };
        let height = line_h * (self.lines.len() + self.local.len()) as f32;
        plate(
            canvas,
            Rect::from_xywh(pad, pad, width + pad * 2.0, height + pad * 2.0),
            scale,
        );
        for (i, (text, role)) in self.texts().enumerate() {
            let baseline = f64::from(pad * 2.0 + line_h * (i as f32 + 0.8));
            fonts.draw(
                canvas,
                text,
                f64::from(pad * 2.0),
                baseline,
                W::Medium,
                size,
                colour(role),
            );
        }
    }
}

/// One sentence on the overlay's plate, centred near the bottom of a `width` × `height` canvas.
pub(super) fn draw_notice(canvas: &Canvas, fonts: &Fonts, text: &str, (width, height): (u32, u32), scale: f32) {
    let size = f64::from(16.0 * scale);
    let line_h = (size * 1.3) as f32;
    let pad = 12.0 * scale;
    let w = fonts.measure(text, W::Medium, size) + pad * 2.0;
    let h = line_h + pad * 2.0;
    let rect = Rect::from_xywh((width as f32 - w) / 2.0, height as f32 - h - pad * 3.0, w, h);
    plate(canvas, rect, scale);
    let baseline = f64::from(rect.top + pad + line_h * 0.8);
    let x = f64::from(rect.left + pad);
    fonts.draw(canvas, text, x, baseline, W::Medium, size, colour(Role::Primary));
}

fn plate(canvas: &Canvas, rect: Rect, scale: f32) {
    let mut paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.62), None);
    paint.set_anti_alias(true);
    canvas.draw_rrect(RRect::new_rect_xy(rect, 8.0 * scale, 8.0 * scale), &paint);
}

fn colour(role: Role) -> Color4f {
    match role {
        Role::Warn => Color4f::new(1.0, 0.78, 0.3, 1.0),
        Role::Muted => Color4f::new(0.75, 0.75, 0.78, 1.0),
        _ => Color4f::new(1.0, 1.0, 1.0, 1.0),
    }
}
