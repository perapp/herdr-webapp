//! A pinned private connection card and bounded logs. Inline PNG needs a positive Kitty probe.
use crate::dashboard_graphics::{self, GraphicsProbe};
use anyhow::Result;
use crossterm::{
    ExecutableCommand, cursor,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use qrcode::{QrCode, render::unicode};
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
struct Inner {
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
        let code = QrCode::new(login.as_bytes())?;
        let qr = code
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build();
        let png = interactive
            .then(|| dashboard_graphics::png_bytes(&code))
            .transpose()?;
        let inner = Arc::new(Inner {
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
            let login = safe(login);
            let socket = safe(socket);
            this.thread = Some(std::thread::spawn(move || {
                run(&inner, &qr, png.as_deref(), &login, &socket, mode);
            }));
        } else {
            println!(
                "Herdr Web · {}\n{}: {login}\nPrivate URL and QR grant FULL SHELL ACCESS.\nCore socket: {socket}\n{qr}\n{}\n--- Logs ---",
                env!("CARGO_PKG_VERSION"),
                mode.label(),
                transport_notice(login)
            );
        }
        this.log(mode.ready());
        Ok(this)
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
        format!("HERDR / WEB   ·   {}", env!("CARGO_PKG_VERSION")),
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
fn run(inner: &Inner, qr: &str, png: Option<&[u8]>, login: &str, socket: &str, mode: Mode) {
    // This dashboard owns terminal input. Never probe in plain/non-TTY mode;
    // retain a single input owner throughout to drain late/duplicate responses.
    let mut probe = png.and_then(|_| GraphicsProbe::start());
    let mut frame: Option<Frame> = None;
    let mut layout = None;
    let mut previous_logs = Vec::new();
    let mut placed = false;
    if let Some(probe) = &probe {
        let _ = write_output(&probe.query());
    }
    while !inner.stop.load(Ordering::Relaxed) {
        if let Some(probe) = &mut probe {
            probe.poll();
        }
        let supported = probe.as_ref().is_some_and(GraphicsProbe::supported);
        let pending = probe.as_ref().is_some_and(|p| !p.finished());
        if let Ok(size) = terminal::size() {
            let key = (size, supported, pending);
            if layout != Some(key) {
                if placed && let Some(probe) = &probe {
                    let _ = write_output(&dashboard_graphics::delete(probe.image_id()));
                }
                placed = false;
                let next = Frame::new(qr, login, socket, mode, size, supported, pending);
                let mut output = next.full_text(login);
                if let (Some(at), Some(png), Some(probe)) = (next.image, png, probe.as_ref()) {
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
