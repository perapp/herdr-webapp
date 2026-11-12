//! A fixed connection card above a bounded live log. No child programs or browser runtime.
use anyhow::Result;
use crossterm::{
    ExecutableCommand, cursor,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use qrcode::{QrCode, render::unicode};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    pub fn start(login: &str, socket: &str, plain: bool) -> Result<Self> {
        let interactive =
            !plain && io::stdout().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb");
        let qr = QrCode::new(login.as_bytes())?
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build();
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
            let login = login.to_owned();
            let socket = socket.to_owned();
            this.thread = Some(std::thread::spawn(move || {
                while !inner.stop.load(Ordering::Relaxed) {
                    let _ = render(&inner, &qr, &login, &socket);
                    std::thread::sleep(Duration::from_millis(250));
                }
            }));
        } else {
            println!(
                "Herdr Web · {}\nLAN URL: {login}\nPrivate URL and QR grant FULL SHELL ACCESS.\nCore socket: {socket}\n{qr}\nHTTP is unencrypted. Use a trusted network, VPN or HTTPS. Never share this QR or sign-in URL.\n--- Logs ---",
                env!("CARGO_PKG_VERSION")
            );
        }
        this.log("Server ready. Open the private LAN URL or scan the QR to sign in automatically.");
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
fn connection_rows(login: &str, width: usize) -> Vec<String> {
    let mut rows = vec![
        format!("HERDR / WEB   ·   {}", env!("CARGO_PKG_VERSION")),
        "LAN URL (private · FULL SHELL ACCESS):".into(),
    ];
    let chars: Vec<_> = safe(login).chars().collect();
    for chunk in chars.chunks(width.max(1)) {
        rows.push(chunk.iter().collect());
    }
    rows
}
fn render(inner: &Inner, qr: &str, login: &str, socket: &str) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let width = usize::from(width);
    let height = usize::from(height);
    let login = safe(login);
    let mut rows = connection_rows(&login, width.saturating_sub(1));
    let link_rows = 2..rows.len();
    let qrrows: Vec<_> = qr.lines().collect();
    if height >= rows.len() + qrrows.len() + 5
        && width > qrrows.iter().map(|s| s.chars().count()).max().unwrap_or(0) + 1
    {
        for line in qrrows {
            rows.push(format!(" {line}"));
        }
    } else {
        rows.push("QR needs a larger terminal. Enlarge this pane or use --no-dashboard.".into());
    }
    rows.push("Private URL and QR grant FULL SHELL ACCESS · Do not share either.".into());
    rows.push("HTTP is unencrypted · Use a trusted network, VPN or HTTPS. Ctrl+C stops.".into());
    rows.push(format!("Core  {}", safe(socket)));
    rows.push("─ Logs (UTC) ─────────────────────────────────────────────────────────".into());
    let remaining = height.saturating_sub(rows.len());
    let lines = inner.lines.lock().unwrap();
    rows.extend(
        lines
            .iter()
            .skip(lines.len().saturating_sub(remaining))
            .cloned(),
    );
    let mut output = String::from("\x1b[H");
    for i in 0..height {
        output.push_str("\x1b[2K");
        if let Some(row) = rows.get(i) {
            if link_rows.contains(&i) {
                // Keep wrapped URL rows clickable as one complete sign-in link.
                output.push_str(&format!("\x1b]8;;{login}\x1b\\"));
            }
            output.push_str(&crop(row, width.saturating_sub(1)));
            if link_rows.contains(&i) {
                output.push_str("\x1b]8;;\x1b\\");
            }
        }
        if i + 1 < height {
            output.push_str("\r\n");
        }
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
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
    #[test]
    fn pinned_url_contains_the_entire_token_even_when_wrapped() {
        let login = format!("http://192.168.1.234:8001/#token={}", "a".repeat(64));
        for width in [1, 40, 79, 199] {
            let rows = super::connection_rows(&login, width);
            assert!(rows[1].contains("private"));
            assert_eq!(rows[2..].concat(), login);
            assert!(rows[2..].iter().all(|row| row.chars().count() <= width));
        }
    }
    #[test]
    fn log_text_cannot_inject_terminal_controls() {
        assert_eq!(super::safe("hi\x1b[31m\n"), "hi[31m");
    }
}
