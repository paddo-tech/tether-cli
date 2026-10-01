use super::truncate;
use crate::dashboard::theme::{mix, Theme};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};
use std::time::{Duration, Instant};

pub const MAX_TOASTS: usize = 4;
const SLIDE: Duration = Duration::from_millis(220);
const FADE: Duration = Duration::from_millis(600);
const MAX_WIDTH: u16 = 56;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

/// A transient notice in the top-right corner. It slides in, waits, fades and expires.
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub born: Instant,
}

impl Toast {
    pub fn new(kind: ToastKind, text: String, born: Instant) -> Self {
        Self { kind, text, born }
    }

    /// Errors stay longer: they usually need reading.
    pub fn ttl(&self) -> Duration {
        match self.kind {
            ToastKind::Error => Duration::from_secs(6),
            _ => Duration::from_secs(3),
        }
    }

    pub fn alive(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.born) < self.ttl()
    }

    /// Slide-in progress 0..1 (eased) and fade-out amount 0..1 at `now`.
    pub fn phase(&self, now: Instant) -> (f32, f32) {
        let age = now.saturating_duration_since(self.born);
        let slide = (age.as_secs_f32() / SLIDE.as_secs_f32()).min(1.0);
        let eased = 1.0 - (1.0 - slide).powi(3);
        let left = self.ttl().saturating_sub(age);
        let fade = 1.0 - (left.as_secs_f32() / FADE.as_secs_f32()).min(1.0);
        (eased, fade)
    }
}

/// Stack toasts under the header, newest on top.
pub fn render(f: &mut Frame, toasts: &[Toast], t: &Theme) {
    let area = f.area();
    if area.width < 24 || area.height < 8 {
        return;
    }
    let now = Instant::now();
    let mut y = area.y + 2;
    for toast in toasts.iter().rev() {
        let (icon, color) = match toast.kind {
            ToastKind::Info => ("●", t.info),
            ToastKind::Success => ("✓", t.ok),
            ToastKind::Error => ("✗", t.error),
        };
        let max_w = MAX_WIDTH.min(area.width.saturating_sub(4));
        let text_w = toast.text.chars().count() as u16 + 6;
        let width = text_w.clamp(20, max_w);
        let inner_w = width.saturating_sub(6).max(1) as usize;
        let lines = toast.text.chars().count().div_ceil(inner_w).clamp(1, 3) as u16;
        let height = lines + 2;
        if y + height > area.bottom().saturating_sub(1) {
            break;
        }

        let (slide, fade) = toast.phase(now);
        let offset = ((1.0 - slide) * (width + 2) as f32).round() as u16;
        let x = area.right().saturating_sub(width + 2) + offset;
        let visible_w = area.right().saturating_sub(x).min(width);
        if visible_w >= 4 {
            let rect = Rect::new(x, y, visible_w, height);
            let fg = |c: Color| if t.rgb { mix(c, t.base_bg, fade) } else { c };
            let bg = if t.rgb {
                mix(t.popup_bg, t.base_bg, fade)
            } else {
                t.popup_bg
            };
            f.render_widget(Clear, rect);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(fg(color)))
                .padding(Padding::horizontal(1))
                .style(Style::default().bg(bg));
            let text = truncate(&toast.text, inner_w * 3);
            let body = Paragraph::new(Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default().fg(fg(color)).bold()),
                Span::styled(text, Style::default().fg(fg(t.text))),
            ]))
            .wrap(Wrap { trim: true })
            .block(block);
            f.render_widget(body, rect);
        }
        y += height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_expire_after_their_ttl() {
        let now = Instant::now();
        let info = Toast::new(ToastKind::Info, "hi".into(), now - Duration::from_secs(4));
        let error = Toast::new(ToastKind::Error, "bad".into(), now - Duration::from_secs(4));
        assert!(!info.alive(now));
        assert!(error.alive(now));
    }

    #[test]
    fn toast_slides_in_then_fades_out() {
        let born = Instant::now();
        let toast = Toast::new(ToastKind::Success, "ok".into(), born);
        let (slide, fade) = toast.phase(born);
        assert_eq!((slide, fade), (0.0, 0.0));
        let (slide, fade) = toast.phase(born + Duration::from_secs(1));
        assert_eq!((slide, fade), (1.0, 0.0));
        let (_, fade) = toast.phase(born + Duration::from_millis(2700));
        assert!(fade > 0.4 && fade < 0.6);
    }
}
