//! A pinned private connection card and bounded logs. Inline PNG needs a positive Kitty probe.
use crate::dashboard_graphics::{self, GraphicsProbe};
use anyhow::Result;
use crossterm::{
    ExecutableCommand, cursor,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use qrcode::{EcLevel, QrCode, render::unicode};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Relay,
}
impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Local => "LAN URL",
            Self::Relay => "Pairing URL",
        }
    }
    fn ready(self) -> &'static str {
        match self {
            Self::Local => "Server ready. Open the private sign-in URL or scan the QR.",
            Self::Relay => {
                "Pairing code ready. Connecting to the relay; this is not a local HTTP server."
            }
        }
    }
}
struct ConnectionCard {
    login: String,
    qr: String,
    png: Option<Vec<u8>>,
    unavailable: Option<String>,
}
impl ConnectionCard {
    fn new(login: &str, interactive: bool) -> Result<Self> {
        let login = safe(login);
        if login.is_empty() {
            return Ok(Self {
                login,
                qr: String::new(),
                png: None,
                unavailable: Some("Pairing code unavailable. Waiting for connector status.".into()),
            });
        }
        let qr = text_qr(&login)?;
        let png = interactive
            .then(|| {
                QrCode::new(login.as_bytes())
                    .map_err(anyhow::Error::from)
                    .and_then(|code| dashboard_graphics::png_bytes(&code))
            })
            .transpose()?;
        Ok(Self {
            login,
            qr,
            png,
            unavailable: None,
        })
    }
}
struct Inner {
    card: Mutex<Arc<ConnectionCard>>,
    lines: Mutex<VecDeque<String>>,
    stop: AtomicBool,
    interactive: bool,
}
pub struct Dashboard {
    inner: Arc<Inner>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Dashboard {
    pub fn start(login: &str, socket: &str, plain: bool, mode: Mode) -> Result<Self> {
        let interactive =
            !plain && io::stdout().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb");
        // One immutable card keeps the URL, text QR and native PNG in sync.
        let card = Arc::new(ConnectionCard::new(login, interactive)?);
        let inner = Arc::new(Inner {
            card: Mutex::new(card.clone()),
            lines: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            interactive,
        });
        let mut this = Self {
            inner: inner.clone(),
            thread: None,
        };
        if interactive {
            io::stdout()
                .execute(EnterAlternateScreen)?
                .execute(cursor::Hide)?;
            let socket = safe(socket);
            this.thread = Some(std::thread::spawn(move || {
                run(&inner, &socket, mode);
            }));
        } else if let Some(status) = &card.unavailable {
            println!(
                "Herdr Web · {}\n{status}\nCore socket: {socket}\n--- Logs ---",
                crate::build_info::VERSION
            );
        } else {
            println!(
                "Herdr Web · {}\n{}: {login}\nPrivate URL and QR grant FULL SHELL ACCESS.\nCore socket: {socket}\n{qr}\n{}\n--- Logs ---",
                crate::build_info::VERSION,
                mode.label(),
                transport_notice(login),
                qr = card.qr
            );
        }
        this.log(card.unavailable.as_deref().unwrap_or(mode.ready()));
        Ok(this)
    }
    /// Swap all representations together. Never append fresh secrets to logs.
    pub fn update_link(&self, login: &str) -> Result<()> {
        let card = Arc::new(ConnectionCard::new(login, self.inner.interactive)?);
        *self.inner.card.lock().unwrap() = card;
        self.log("Pairing link and QR renewed; existing trusted browsers are unchanged.");
        Ok(())
    }
    pub fn unavailable(&self, message: &str) {
        *self.inner.card.lock().unwrap() = Arc::new(ConnectionCard {
            login: String::new(),
            qr: String::new(),
            png: None,
            unavailable: Some(safe(message)),
        });
        self.log(message);
    }
    pub fn log(&self, message: &str) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            % 86400;
        let line = format!(
            "{:02}:{:02}:{:02}  {}",
            stamp / 3600,
            (stamp / 60) % 60,
            stamp % 60,
            safe(message)
        );
        if !self.inner.interactive {
            println!("{line}");
            return;
        }
        let mut lines = self.inner.lines.lock().unwrap();
        lines.push_back(line);
        if lines.len() > 500 {
            lines.pop_front();
        }
    }
}
fn safe(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}
fn crop(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}
fn transport_notice(login: &str) -> &'static str {
    if login.starts_with("https://") {
        "HTTPS protects this link. Private URL and QR grant FULL SHELL ACCESS. Ctrl+C stops."
    } else {
        "HTTP is unencrypted. Use a trusted network, VPN or HTTPS. Ctrl+C stops."
    }
}
fn connection_rows(login: &str, width: usize, mode: Mode) -> Vec<String> {
    let mut rows = vec![
        format!("HERDR / WEB   ·   {}", crate::build_info::VERSION),
        format!("{} (private · FULL SHELL ACCESS):", mode.label()),
    ];
    let chars: Vec<_> = safe(login).chars().collect();
    for chunk in chars.chunks(width.max(1)) {
        rows.push(chunk.iter().collect());
    }
    rows
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placement {
    row: u16,
    columns: u16,
    rows: u16,
}
struct Frame {
    rows: Vec<String>,
    links: Range<usize>,
    image: Option<Placement>,
    log_start: usize,
    width: u16,
    height: u16,
}
impl Frame {
    fn new(
        qr: &str,
        login: &str,
        socket: &str,
        mode: Mode,
        size: (u16, u16),
        supported: bool,
        pending: bool,
    ) -> Self {
        let (width, height) = size;
        let usable = usize::from(width).saturating_sub(1);
        let mut rows = connection_rows(login, usable, mode);
        let links = 2..rows.len();
        let available = usize::from(height).saturating_sub(rows.len() + 7);
        let columns = usable
            .saturating_sub(1)
            .min(48)
            .min(available.saturating_mul(2));
        let image = if supported && columns >= 20 {
            let placement = Placement {
                row: (rows.len() + 1) as u16,
                columns: columns as u16,
                rows: columns.div_ceil(2) as u16,
            };
            rows.extend(std::iter::repeat_n(
                String::new(),
                usize::from(placement.rows),
            ));
            Some(placement)
        } else {
            let qrrows: Vec<_> = qr.lines().collect();
            let needed_columns = qrrows.iter().map(|r| r.chars().count()).max().unwrap_or(0) + 2;
            let needed_rows = rows.len() + qrrows.len() + 5;
            if usize::from(width) >= needed_columns && usize::from(height) >= needed_rows {
                rows.extend(qrrows.into_iter().map(|r| format!(" {r}")));
            } else if pending {
                rows.push("Checking terminal inline PNG support…".into());
            } else {
                rows.push(format!("Text QR needs {needed_columns}×{needed_rows} cells; terminal reports {width}×{height}."));
                rows.push("Inline PNG not confirmed. Open the private link above, or reduce the font size.".into());
            }
            None
        };
        rows.push("Private URL and QR grant FULL SHELL ACCESS · Do not share either.".into());
        rows.push(transport_notice(login).into());
        rows.push(format!("Core  {socket}"));
        rows.push("─ Logs (UTC) ─────────────────────────────────────────────────────────".into());
        let log_start = rows.len();
        Self {
            rows,
            links,
            image,
            log_start,
            width,
            height,
        }
    }
    fn unavailable(message: &str, socket: &str, mode: Mode, size: (u16, u16)) -> Self {
        let rows = vec![
            format!("HERDR / WEB   ·   {}", crate::build_info::VERSION),
            format!("{} unavailable", mode.label()),
            safe(message),
            "Existing trusted browsers can still reconnect. Ctrl+C stops.".into(),
            format!("Core  {socket}"),
            "─ Logs (UTC) ─────────────────────────────────────────────────────────".into(),
        ];
        let log_start = rows.len();
        Self {
            rows,
            links: 0..0,
            image: None,
            log_start,
            width: size.0,
            height: size.1,
        }
    }
    fn logs(&self, inner: &Inner) -> Vec<String> {
        let lines = inner.lines.lock().unwrap();
        let count = usize::from(self.height).saturating_sub(self.log_start);
        lines
            .iter()
            .skip(lines.len().saturating_sub(count))
            .cloned()
            .collect()
    }
    fn full_text(&self, login: &str) -> Vec<u8> {
        let mut output = String::from("\x1b[H");
        for i in 0..usize::from(self.height) {
            output.push_str("\x1b[2K");
            if let Some(row) = self.rows.get(i) {
                if self.links.contains(&i) {
                    output.push_str(&format!("\x1b]8;;{login}\x1b\\"));
                }
                output.push_str(&crop(row, usize::from(self.width).saturating_sub(1)));
                if self.links.contains(&i) {
                    output.push_str("\x1b]8;;\x1b\\");
                }
            }
            if i + 1 < usize::from(self.height) {
                output.push_str("\r\n");
            }
        }
        output.into_bytes()
    }
    fn log_text(&self, logs: &[String]) -> Vec<u8> {
        let mut output = String::new();
        for i in self.log_start..usize::from(self.height) {
            output.push_str(&format!("\x1b[{};1H\x1b[2K", i + 1));
            if let Some(line) = logs.get(i - self.log_start) {
                output.push_str(&crop(line, usize::from(self.width).saturating_sub(1)));
            }
        }
        output.into_bytes()
    }
}
fn run(inner: &Inner, socket: &str, mode: Mode) {
    // This dashboard owns terminal input. Never probe in plain/non-TTY mode;
    // retain a single input owner throughout to drain late/duplicate responses.
    let mut probe = GraphicsProbe::start();
    let mut frame: Option<Frame> = None;
    let mut layout = None;
    let mut previous_logs = Vec::new();
    let mut placed = false;
    let mut displayed: Option<Arc<ConnectionCard>> = None;
    while !inner.stop.load(Ordering::Relaxed) {
        if let Some(probe) = &mut probe {
            if probe.query_due() {
                let _ = write_output(&probe.query());
                probe.query_sent();
            }
            probe.poll();
        }
        let supported = probe.as_ref().is_some_and(GraphicsProbe::supported);
        let pending = probe.as_ref().is_some_and(|p| !p.finished());
        if let Ok(size) = terminal::size() {
            let card = inner.card.lock().unwrap().clone();
            let key = (size, supported, pending);
            let changed = displayed
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &card));
            if layout != Some(key) || changed {
                if placed && let Some(probe) = &probe {
                    let _ = write_output(&dashboard_graphics::delete(probe.image_id()));
                }
                placed = false;
                let next = if let Some(message) = &card.unavailable {
                    Frame::unavailable(message, socket, mode, size)
                } else {
                    Frame::new(
                        &card.qr,
                        &card.login,
                        socket,
                        mode,
                        size,
                        supported,
                        pending,
                    )
                };
                let mut output = next.full_text(&card.login);
                if let (Some(at), Some(png), Some(probe)) =
                    (next.image, card.png.as_deref(), probe.as_ref())
                {
                    output.extend_from_slice(format!("\x1b[{};2H", at.row).as_bytes());
                    output.extend_from_slice(&dashboard_graphics::transmit(
                        png,
                        probe.image_id(),
                        at.columns,
                        at.rows,
                    ));
                    placed = true;
                }
                let logs = next.logs(inner);
                output.extend_from_slice(&next.log_text(&logs));
                let _ = write_output(&output);
                previous_logs = logs;
                frame = Some(next);
                layout = Some(key);
                displayed = Some(card);
            } else if let Some(frame) = &frame {
                let logs = frame.logs(inner);
                if logs != previous_logs {
                    // Never clear/retransmit the QR region during a log update.
                    let _ = write_output(&frame.log_text(&logs));
                    previous_logs = logs;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(if pending { 25 } else { 100 }));
    }
    if placed && let Some(probe) = &probe {
        let _ = write_output(&dashboard_graphics::delete(probe.image_id()));
    }
    // Probe drop restores input modes before Dashboard leaves the alternate screen.
}
fn text_qr(login: &str) -> Result<String> {
    Ok(
        QrCode::with_error_correction_level(login.as_bytes(), EcLevel::L)?
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}
fn write_output(bytes: &[u8]) -> io::Result<()> {
    let mut output = io::stdout().lock();
    output.write_all(bytes)?;
    output.flush()
}
impl Drop for Dashboard {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.inner.interactive {
            let _ = io::stdout()
                .execute(cursor::Show)
                .and_then(|s| s.execute(LeaveAlternateScreen));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renewed_card_keeps_url_text_and_png_together() {
        let old =
            ConnectionCard::new("https://qr-preview.invalid/#pair=synthetic-old", true).unwrap();
        let new =
            ConnectionCard::new("https://qr-preview.invalid/#pair=synthetic-new", true).unwrap();
        assert_ne!(old.qr, new.qr);
        assert_ne!(old.png, new.png);
        let frame = Frame::new(
            &new.qr,
            &new.login,
            "synthetic.sock",
            Mode::Relay,
            (120, 60),
            false,
            false,
        );
        let text = String::from_utf8(frame.full_text(&new.login)).unwrap();
        assert!(text.contains(&new.login));
        assert!(!text.contains(&old.login));
        assert_eq!(frame.rows[frame.links.clone()].concat(), new.login);
    }
    #[test]
    fn unavailable_card_removes_hyperlink_and_qr() {
        let frame = Frame::unavailable(
            "Pairing temporarily unavailable.",
            "synthetic.sock",
            Mode::Relay,
            (100, 30),
        );
        let text = String::from_utf8(frame.full_text("")).unwrap();
        assert!(frame.image.is_none() && frame.links.is_empty());
        assert!(text.contains("unavailable"));
        assert!(!text.contains("#pair=") && !text.contains("\x1b]8;;"));
    }
    #[test]
    fn pinned_url_contains_the_entire_token_even_when_wrapped() {
        let login = format!("http://192.168.1.234:8001/#token={}", "a".repeat(64));
        for width in [1, 40, 79, 199] {
            let rows = connection_rows(&login, width, Mode::Local);
            assert!(rows[1].contains("private"));
            assert_eq!(rows[2..].concat(), login);
            assert!(rows[2..].iter().all(|row| row.chars().count() <= width));
        }
    }
    #[test]
    fn log_text_cannot_inject_terminal_controls() {
        assert_eq!(safe("hi\x1b[31m\n"), "hi[31m");
    }
    #[test]
    fn relay_labels_do_not_claim_lan_or_unencrypted_https() {
        let login = "https://relay.example/#pair=fake";
        let frame = Frame::new(
            "██\n██",
            login,
            "private.sock",
            Mode::Relay,
            (100, 30),
            false,
            false,
        );
        let text = String::from_utf8(frame.full_text(login)).unwrap();
        assert!(text.contains("Pairing URL") && text.contains("HTTPS protects"));
        assert!(!text.contains("LAN URL") && !text.contains("HTTP is unencrypted"));
        assert!(!Mode::Relay.ready().contains("Server ready"));
        assert!(transport_notice("http://127.0.0.1:9001").contains("unencrypted"));
    }
    #[test]
    fn image_fits_normal_terminal_and_log_updates_leave_it_alone() {
        let qr = (0..43)
            .map(|_| "█".repeat(85))
            .collect::<Vec<_>>()
            .join("\n");
        let frame = Frame::new(
            &qr,
            &format!("https://relay.example/#pair={}", "x".repeat(340)),
            "core.sock",
            Mode::Relay,
            (120, 40),
            true,
            false,
        );
        let at = frame.image.unwrap();
        assert!(at.rows <= 24 && frame.log_start + 3 <= 40);
        let logs = String::from_utf8(frame.log_text(&["ready".into()])).unwrap();
        assert!(!logs.contains("\x1b_G"));
        assert!(logs.starts_with(&format!("\x1b[{};1H", frame.log_start + 1)));
    }
    #[test]
    #[ignore = "interactive synthetic fixture; run only in an owned isolated terminal"]
    fn synthetic_native_preview() {
        assert_eq!(std::env::var("HERDR_WEBAPP_QR_PREVIEW").as_deref(), Ok("1"));
        let login = format!(
            "https://qr-preview.invalid/#pair={}",
            "synthetic-not-an-access-credential-".repeat(10)
        );
        let dashboard =
            Dashboard::start(&login, "synthetic-core.sock", false, Mode::Relay).unwrap();
        dashboard.log("SYNTHETIC PREVIEW ONLY — QR does not grant access to any Core.");
        let seconds = std::env::var("HERDR_WEBAPP_QR_PREVIEW_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(45)
            .clamp(1, 120);
        std::thread::sleep(Duration::from_secs(seconds));
        drop(dashboard);
    }
    #[test]
    #[ignore = "interactive synthetic rotation fixture; owned isolated TTY only"]
    fn synthetic_native_rotation_preview() {
        assert_eq!(
            std::env::var("HERDR_WEBAPP_QR_ROTATION_PREVIEW").as_deref(),
            Ok("1")
        );
        let dashboard = Dashboard::start(
            "https://qr-preview.invalid/#pair=synthetic-first",
            "synthetic.sock",
            false,
            Mode::Relay,
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(1));
        dashboard
            .update_link("https://qr-preview.invalid/#pair=synthetic-second")
            .unwrap();
        std::thread::sleep(Duration::from_secs(1));
        dashboard.unavailable("SYNTHETIC pairing in progress; no access credentials.");
        std::thread::sleep(Duration::from_secs(1));
        dashboard
            .update_link("https://qr-preview.invalid/#pair=synthetic-third")
            .unwrap();
        std::thread::sleep(Duration::from_secs(1));
    }
    #[test]
    fn compact_text_qr_preserves_url_and_quiet_zone_with_fewer_cells() {
        let prefix = "https://qr-preview.invalid/#pair=";
        let login = format!("{prefix}{}", "a".repeat(365 - prefix.len()));
        let original = QrCode::new(login.as_bytes()).unwrap();
        let compact = QrCode::with_error_correction_level(login.as_bytes(), EcLevel::L).unwrap();
        assert!(compact.width() < original.width());
        assert_eq!(original.width() + 8, 85);
        assert_eq!(compact.width() + 8, 73);
        let qr = text_qr(&login).unwrap();
        let rows: Vec<_> = qr.lines().collect();
        assert_eq!(rows.len(), (compact.width() + 8).div_ceil(2));
        assert!(
            rows.iter()
                .all(|line| line.chars().count() == compact.width() + 8)
        );
        // Native PNG keeps its original M matrix; nothing is clipped or rescaled
        // in text mode, and the containing frame still shows the entire URL.
        let frame = Frame::new(
            &qr,
            &login,
            "core.sock",
            Mode::Relay,
            (120, 60),
            false,
            false,
        );
        assert_eq!(frame.rows[frame.links.clone()].concat(), login);
    }
    #[test]
    fn fallback_reports_actual_cell_requirements() {
        let qr = (0..43)
            .map(|_| "█".repeat(85))
            .collect::<Vec<_>>()
            .join("\n");
        let frame = Frame::new(
            &qr,
            "https://relay.example/#pair=fake",
            "core.sock",
            Mode::Relay,
            (120, 40),
            false,
            false,
        );
        assert!(frame.image.is_none());
        let text = frame.rows.join("\n");
        assert!(text.contains("87×51 cells") && text.contains("120×40"));
        assert!(!text.contains("--no-dashboard"));
    }
}
