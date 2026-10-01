use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use std::time::{Duration, Instant};

/// Every color the dashboard draws with. Components read colors from here, never from `Color`.
pub struct Theme {
    /// True-color palettes blend colors for fades; the ANSI palette cannot.
    pub rgb: bool,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub info: Color,
    pub key: Color,
    pub hash: Color,
    pub value: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub team: Color,
    pub selection: Color,
    pub base_bg: Color,
    pub popup_bg: Color,
    pub brand_fg: Color,
    pub brand_bg: Color,
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
}

const fn hex(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Theme {
    /// The 16-color ANSI palette, so the terminal's own color scheme applies.
    pub fn ansi() -> Self {
        Self {
            rgb: false,
            text: Color::White,
            muted: Color::Gray,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            border_focus: Color::Cyan,
            accent: Color::Cyan,
            info: Color::Blue,
            key: Color::Yellow,
            hash: Color::Yellow,
            value: Color::Yellow,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            team: Color::Magenta,
            selection: Color::Indexed(237),
            base_bg: Color::Reset,
            popup_bg: Color::Reset,
            brand_fg: Color::Black,
            brand_bg: Color::Cyan,
            diff_add_bg: Color::Reset,
            diff_del_bg: Color::Reset,
        }
    }

    /// Catppuccin Mocha, for dark backgrounds.
    pub fn mocha() -> Self {
        let base = hex(0x1e1e2e);
        let green = hex(0xa6e3a1);
        let red = hex(0xf38ba8);
        Self {
            rgb: true,
            text: hex(0xcdd6f4),
            muted: hex(0xa6adc8),
            dim: hex(0x6c7086),
            border: hex(0x45475a),
            border_focus: hex(0xb4befe),
            accent: hex(0xcba6f7),
            info: hex(0x89b4fa),
            key: hex(0xfab387),
            hash: hex(0xf9e2af),
            value: hex(0xfab387),
            ok: green,
            warn: hex(0xf9e2af),
            error: red,
            team: hex(0xf5c2e7),
            selection: hex(0x313244),
            base_bg: base,
            popup_bg: hex(0x181825),
            brand_fg: hex(0x11111b),
            brand_bg: hex(0xcba6f7),
            diff_add_bg: mix(base, green, 0.14),
            diff_del_bg: mix(base, red, 0.14),
        }
    }

    /// Catppuccin Latte, for light backgrounds.
    pub fn latte() -> Self {
        let base = hex(0xeff1f5);
        let green = hex(0x40a02b);
        let red = hex(0xd20f39);
        Self {
            rgb: true,
            text: hex(0x4c4f69),
            muted: hex(0x6c6f85),
            dim: hex(0x9ca0b0),
            border: hex(0xbcc0cc),
            border_focus: hex(0x7287fd),
            accent: hex(0x8839ef),
            info: hex(0x1e66f5),
            key: hex(0xfe640b),
            hash: hex(0xdf8e1d),
            value: hex(0xfe640b),
            ok: green,
            warn: hex(0xdf8e1d),
            error: red,
            team: hex(0xea76cb),
            selection: hex(0xccd0da),
            base_bg: base,
            popup_bg: hex(0xe6e9ef),
            brand_fg: hex(0xeff1f5),
            brand_bg: hex(0x8839ef),
            diff_add_bg: mix(base, green, 0.12),
            diff_del_bg: mix(base, red, 0.12),
        }
    }

    /// Pick a theme from the config setting. `auto` uses true color when the terminal
    /// advertises it, and Latte only when the background is known to be light.
    pub fn select(
        setting: Option<&str>,
        truecolor: bool,
        dark: impl FnOnce() -> Option<bool>,
    ) -> Self {
        match setting.map(str::to_ascii_lowercase).as_deref() {
            Some("mocha") => Self::mocha(),
            Some("latte") => Self::latte(),
            Some("ansi") => Self::ansi(),
            _ if !truecolor => Self::ansi(),
            _ if dark() == Some(false) => Self::latte(),
            _ => Self::mocha(),
        }
    }

    /// Style for a key name in a shortcut hint.
    pub fn key_hint(&self) -> Style {
        Style::default().fg(self.key).bold()
    }
}

/// Linear blend of two RGB colors. Non-RGB colors switch over at the midpoint.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
        }
        _ if t < 0.5 => a,
        _ => b,
    }
}

/// COLORTERM is how terminals advertise 24-bit color.
pub fn truecolor_env() -> bool {
    std::env::var("COLORTERM")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "truecolor" | "24bit"))
        .unwrap_or(false)
}

/// What the background probe found, and the input it read on the way.
#[derive(Default)]
pub struct Probe {
    pub dark: Option<bool>,
    /// Keys typed while the query waited, to replay into the event loop.
    pub typed: Vec<KeyEvent>,
    /// The terminal did not answer in time, so its reply may still arrive as input.
    pub late_reply: bool,
    /// The OSC reply had begun but not ended when the wait gave up.
    pub mid_reply: bool,
}

/// Whether the terminal background is dark: COLORFGBG first, then an OSC 11 query.
/// Must run in raw mode, before the event loop reads input.
pub fn probe_background() -> Probe {
    if let Ok(v) = std::env::var("COLORFGBG") {
        if let Some(dark) = parse_colorfgbg(&v) {
            return Probe {
                dark: Some(dark),
                ..Probe::default()
            };
        }
    }
    let Some(raw) = query_osc11() else {
        return Probe::default();
    };
    let (reply, typed) = split_reply(&raw);
    Probe {
        dark: parse_osc11(&reply),
        typed: keys_from_bytes(&typed),
        late_reply: !da1_done(&raw),
        mid_reply: osc_open(&raw),
    }
}

/// Whether the last OSC reply in `raw` lacks its BEL or ST terminator.
fn osc_open(raw: &[u8]) -> bool {
    raw.windows(2)
        .rposition(|w| w == b"\x1b]")
        .is_some_and(|start| {
            let rest = &raw[start + 2..];
            !rest.contains(&0x07) && !rest.windows(2).any(|w| w == b"\x1b\\")
        })
}

/// Separate terminal replies (OSC `ESC ] ... BEL|ST` and DA1 `ESC [ ? ... c`) from typed bytes.
/// An unterminated reply counts as reply, so its tail is never read as keys.
pub fn split_reply(raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut reply, mut typed) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < raw.len() {
        let rest = &raw[i..];
        let end = if rest.starts_with(b"\x1b]") {
            let bel = rest.iter().position(|&b| b == 0x07);
            let st = rest.windows(2).position(|w| w == b"\x1b\\").map(|p| p + 1);
            Some(match (bel, st) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(rest.len() - 1),
            })
        } else if rest.starts_with(b"\x1b[?") {
            Some(
                rest.iter()
                    .position(|&b| b == b'c')
                    .unwrap_or(rest.len() - 1),
            )
        } else {
            None
        };
        match end {
            Some(end) => {
                reply.extend_from_slice(&rest[..=end]);
                i += end + 1;
            }
            None => {
                typed.push(raw[i]);
                i += 1;
            }
        }
    }
    (reply, typed)
}

/// Plain keys from raw input bytes. Escape sequences such as arrows are dropped.
pub fn keys_from_bytes(bytes: &[u8]) -> Vec<KeyEvent> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        let key = match b {
            0x1b if i < bytes.len() => {
                // Skip a CSI or SS3 sequence up to its final byte.
                if matches!(bytes[i], b'[' | b'O') {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                continue;
            }
            0x1b => KeyEvent::from(KeyCode::Esc),
            b'\r' | b'\n' => KeyEvent::from(KeyCode::Enter),
            b'\t' => KeyEvent::from(KeyCode::Tab),
            0x7f => KeyEvent::from(KeyCode::Backspace),
            0x01..=0x1a => {
                KeyEvent::new(KeyCode::Char((b - 1 + b'a') as char), KeyModifiers::CONTROL)
            }
            0x20..=0x7e => KeyEvent::from(KeyCode::Char(b as char)),
            _ => continue,
        };
        keys.push(key);
    }
    keys
}

/// How long after a timed-out query a late reply is still expected.
const LATE_REPLY_WINDOW: Duration = Duration::from_secs(3);

/// Drops a late OSC reply from key input. Crossterm reads `ESC ] 11;rgb:...` as Alt+]
/// and then plain keys (`d` would toggle the daemon), ended by Alt+\ or Ctrl+G (BEL).
/// The DA1 reply needs no filter: crossterm parses it as an internal event.
pub struct LateReplyFilter {
    until: Instant,
    in_reply: bool,
}

impl LateReplyFilter {
    /// `in_reply` is set when the reply's start was already read, so its tail comes next.
    pub fn new(now: Instant, in_reply: bool) -> Self {
        Self {
            until: now + LATE_REPLY_WINDOW,
            in_reply,
        }
    }

    /// Whether the key is user input rather than part of a reply.
    pub fn allow(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if now > self.until {
            return true;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.in_reply {
            if (alt && key.code == KeyCode::Char('\\')) || (ctrl && key.code == KeyCode::Char('g'))
            {
                self.in_reply = false;
            }
            return false;
        }
        if alt && key.code == KeyCode::Char(']') {
            self.in_reply = true;
            return false;
        }
        true
    }
}

/// `COLORFGBG="15;0"`: the last field is the background's ANSI index.
pub fn parse_colorfgbg(v: &str) -> Option<bool> {
    let bg: u8 = v.rsplit(';').next()?.trim().parse().ok()?;
    Some(!matches!(bg, 7 | 15))
}

/// Parse `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` and judge darkness by relative luminance.
pub fn parse_osc11(reply: &[u8]) -> Option<bool> {
    let s = String::from_utf8_lossy(reply);
    let rgb = &s[s.find("rgb:")? + 4..];
    let mut chans = rgb.split('/').map(|c| {
        let digits: String = c.chars().take_while(|ch| ch.is_ascii_hexdigit()).collect();
        let max = 16f32.powi(digits.len() as i32) - 1.0;
        u32::from_str_radix(&digits, 16)
            .ok()
            .map(|v| v as f32 / max)
    });
    let (r, g, b) = (chans.next()??, chans.next()??, chans.next()??);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5)
}

/// Ask for the background color, then for device attributes. Every terminal answers
/// the second query, so its reply ends the wait without a timeout on silent terminals.
fn query_osc11() -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    tty.flush().ok()?;

    let deadline = Instant::now() + Duration::from_millis(300);
    let mut reply = Vec::new();
    let mut buf = [0u8; 128];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd: tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let n = tty.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&buf[..n]);
        if da1_done(&reply) {
            break;
        }
    }
    Some(reply)
}

/// The device attributes reply looks like `ESC [ ? ... c`.
fn da1_done(reply: &[u8]) -> bool {
    reply
        .windows(3)
        .position(|w| w == b"\x1b[?")
        .is_some_and(|start| reply[start..].contains(&b'c'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_respects_override_and_terminal() {
        assert_eq!(
            Theme::select(Some("latte"), false, || None).base_bg,
            Theme::latte().base_bg
        );
        assert_eq!(
            Theme::select(Some("ANSI"), true, || None).base_bg,
            Theme::ansi().base_bg
        );
        assert_eq!(
            Theme::select(None, false, || Some(true)).base_bg,
            Theme::ansi().base_bg
        );
        assert_eq!(
            Theme::select(Some("auto"), true, || Some(false)).base_bg,
            Theme::latte().base_bg
        );
        assert_eq!(
            Theme::select(None, true, || Some(true)).base_bg,
            Theme::mocha().base_bg
        );
        assert_eq!(
            Theme::select(None, true, || None).base_bg,
            Theme::mocha().base_bg
        );
    }

    #[test]
    fn auto_skips_detection_without_truecolor() {
        let theme = Theme::select(None, false, || panic!("must not query the terminal"));
        assert_eq!(theme.base_bg, Theme::ansi().base_bg);
    }

    #[test]
    fn parses_background_replies() {
        assert_eq!(parse_osc11(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\"), Some(true));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:efef/f1f1/f5f5\x07"), Some(false));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ff/ff/ff\x07"), Some(false));
        assert_eq!(parse_osc11(b"\x1b[?62;22c"), None);
        assert_eq!(parse_colorfgbg("15;0"), Some(true));
        assert_eq!(parse_colorfgbg("0;default;15"), Some(false));
        assert_eq!(parse_colorfgbg("garbage"), None);
    }

    #[test]
    fn typed_bytes_survive_the_query() {
        let raw = b"q\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x03\x1b[?62;22cs";
        let (reply, typed) = split_reply(raw);
        assert_eq!(parse_osc11(&reply), Some(true));
        assert!(da1_done(&reply));
        assert_eq!(typed, b"q\x03s");
        assert_eq!(
            keys_from_bytes(&typed),
            vec![
                KeyEvent::from(KeyCode::Char('q')),
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                KeyEvent::from(KeyCode::Char('s')),
            ]
        );
        assert_eq!(
            keys_from_bytes(b"\x1b[Aj\x1b"),
            vec![
                KeyEvent::from(KeyCode::Char('j')),
                KeyEvent::from(KeyCode::Esc),
            ]
        );
        // An unterminated reply is never read as keys.
        assert_eq!(split_reply(b"\x1b]11;rgb:dd").1, b"");
    }

    #[test]
    fn late_reply_is_dropped_from_key_input() {
        let start = Instant::now();
        let mut filter = LateReplyFilter::new(start, false);
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let key = |c| KeyEvent::from(KeyCode::Char(c));
        assert!(filter.allow(&key('j'), start));
        assert!(!filter.allow(&alt(']'), start));
        for c in "11;rgb:dddd/0000/0000".chars() {
            assert!(!filter.allow(&key(c), start));
        }
        assert!(!filter.allow(&alt('\\'), start));
        assert!(filter.allow(&key('d'), start));
        assert!(filter.allow(&alt(']'), start + Duration::from_secs(5)));

        // The reply began before the query gave up, so its tail arrives without Alt+].
        assert!(osc_open(b"\x1b]11;rgb:dd"));
        assert!(!osc_open(b"\x1b]11;rgb:dddd/0000/0000\x07"));
        let mut filter = LateReplyFilter::new(start, true);
        assert!(!filter.allow(&key('d'), start));
        assert!(!filter.allow(&alt('\\'), start));
        assert!(filter.allow(&key('d'), start));
    }

    #[test]
    fn da1_reply_ends_query() {
        assert!(da1_done(b"\x1b]11;rgb:0/0/0\x07\x1b[?1;2c"));
        assert!(!da1_done(b"\x1b]11;rgb:0/0/0\x07"));
    }

    #[test]
    fn mix_blends_rgb() {
        assert_eq!(mix(hex(0x000000), hex(0xffffff), 0.5), hex(0x808080));
        assert_eq!(mix(Color::Red, Color::Blue, 0.2), Color::Red);
        assert_eq!(mix(Color::Red, Color::Blue, 0.8), Color::Blue);
    }
}
