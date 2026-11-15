//! Native inline QR images, enabled only by a positive Kitty graphics reply.
//! No terminal names, environment variables, or core FACTS imply support.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{Color, QrCode};

// This deadline selects the initial text fallback, not the validity of a reply.
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_WINDOW: Duration = Duration::from_secs(30);
const MAX_REPLY: usize = 4096;
const PIXELS_PER_MODULE: usize = 4;
const QUIET_ZONE: usize = 4;

/// A deliberately unfiltered, integer-scaled grayscale QR with a four-module border.
pub fn png_bytes(code: &QrCode) -> anyhow::Result<Vec<u8>> {
    let modules = code.width();
    let side = (modules + 2 * QUIET_ZONE) * PIXELS_PER_MODULE;
    let mut pixels = vec![255; side * side];
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] == Color::Dark {
                let left = (x + QUIET_ZONE) * PIXELS_PER_MODULE;
                let top = (y + QUIET_ZONE) * PIXELS_PER_MODULE;
                for row in top..top + PIXELS_PER_MODULE {
                    pixels[row * side + left..row * side + left + PIXELS_PER_MODULE].fill(0);
                }
            }
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, side.try_into()?, side.try_into()?);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
        writer.finish()?;
    }
    Ok(bytes)
}

/// An exclusive, threadless stdin reader. Keep alive (and poll) for the dashboard's
/// entire lifetime to consume late/duplicate replies from multiple viewers.
/// The dashboard must be the only component reading stdin during this lifetime.
pub struct GraphicsProbe {
    terminal: TerminalMode,
    parser: ReplyParser,
    keys: PrivateLinkKeys,
    timing: ProbeTiming,
    supported: bool,
}

impl GraphicsProbe {
    /// Only the process group's controlling, foreground terminal is eligible.
    /// `/dev/tty` and the actual PTY need not have equal device numbers.
    pub fn start() -> Option<Self> {
        if !same_foreground_terminal(libc::STDIN_FILENO, libc::STDOUT_FILENO) {
            return None;
        }
        // Own a descriptor so restoring cannot accidentally target a replaced stdin.
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let terminal = TerminalMode::new(fd)?;
        let mut id = rand::random::<u32>();
        while id <= 31 {
            id = rand::random::<u32>();
        }
        Some(Self {
            terminal,
            parser: ReplyParser::new(id),
            keys: PrivateLinkKeys::default(),
            timing: ProbeTiming::default(),
            supported: false,
        })
    }

    /// Verified one-RGB-pixel Kitty query. A positive reply establishes native
    /// graphics support; Herdr's native implementation also supports PNG f=100.
    /// Retry only when query_due(), flush stdout, then call query_sent().
    /// Reusing this run's unpredictable ID lets delayed/duplicate viewer replies
    /// prove support without accepting replies from another dashboard lifetime.
    pub fn query(&self) -> Vec<u8> {
        format!(
            "\x1b_Gi={},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",
            self.image_id()
        )
        .into_bytes()
    }

    /// Queries are sparse and stop after 30 seconds or the first positive reply.
    /// A viewer can attach after the dashboard starts, so one query is insufficient.
    pub fn query_due(&self) -> bool {
        !self.supported
            && is_foreground(self.terminal.fd.as_raw_fd())
            && self.timing.query_due(Instant::now())
    }

    /// Start the fallback/retry clocks only after the query has been flushed.
    pub fn query_sent(&mut self) {
        self.timing.sent(Instant::now());
    }

    /// Bounded nonblocking drain, without changing shared file status flags.
    /// Return intentional plain L/l presses only; protocol replies and encoded
    /// key/paste data are drained, never interpreted as dashboard commands.
    pub fn poll(&mut self) -> usize {
        let mut toggles = 0;
        let fd = self.terminal.fd.as_raw_fd();
        if !is_foreground(fd) {
            return 0; // Reading a background tty could otherwise raise SIGTTIN.
        }
        let mut bytes = [0u8; 1024];
        for _ in 0..8 {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0
                || pfd.revents & libc::POLLIN == 0
                || !is_foreground(fd)
            {
                break;
            }
            // ICANON off and VMIN/VTIME zero: never blocks, even if another
            // consumer were to race poll (the caller must still exclude readers).
            let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n <= 0 {
                break;
            }
            toggles += self.keys.feed(&bytes[..n as usize]);
            let matched = self.parser.feed(&bytes[..n as usize]);
            // Timeout means "show text for now", never "discard valid proof".
            // Continue draining for our entire input-ownership lifetime.
            if matched {
                self.supported = true;
            }
        }
        toggles
    }

    pub fn supported(&self) -> bool {
        self.supported
    }
    pub fn finished(&self) -> bool {
        self.supported || self.timing.initial_finished(Instant::now())
    }
    pub fn image_id(&self) -> u32 {
        self.parser.id
    }
}

/// Streaming input classifier, independent of read boundaries. Be conservative:
/// incomplete escape strings remain suppressed rather than timing out into keys.
#[derive(Default)]
struct PrivateLinkKeys {
    state: u8,
    csi: Vec<u8>,
    paste: bool,
    osc: bool,
}
impl PrivateLinkKeys {
    fn feed(&mut self, bytes: &[u8]) -> usize {
        let mut toggles = 0;
        for &byte in bytes {
            match self.state {
                0 => match byte {
                    0x1b => self.state = 1,
                    b'L' | b'l' if !self.paste => toggles += 1,
                    _ => {}
                },
                1 => {
                    self.state = match byte {
                        b'[' => {
                            self.csi.clear();
                            2
                        }
                        b'O' => 3, // SS3 encoded key
                        b']' | b'P' | b'_' | b'^' | b'X' => {
                            self.osc = byte == b']';
                            4
                        }
                        0x1b => 1,
                        _ => 0, // Includes Alt+L: not a plain key.
                    };
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        if byte == b'~' {
                            if self.csi == b"200" {
                                self.paste = true;
                            }
                            if self.csi == b"201" {
                                self.paste = false;
                            }
                        }
                        self.state = 0;
                    } else if byte == 0x1b {
                        self.state = 1;
                    } else if self.csi.len() < 32 {
                        self.csi.push(byte);
                    }
                }
                3 => {
                    if byte == 0x1b {
                        self.state = 1;
                    } else if (0x40..=0x7e).contains(&byte) {
                        self.state = 0;
                    }
                }
                4 => {
                    if byte == 0x1b {
                        self.state = 5;
                    } else if byte == 7 && self.osc {
                        self.state = 0;
                    }
                }
                5 => {
                    self.state = match byte {
                        b'\\' => 0,
                        0x1b => 5,
                        _ => 4,
                    };
                }
                _ => unreachable!(),
            }
        }
        toggles
    }
}

#[derive(Default)]
struct ProbeTiming {
    first_sent: Option<Instant>,
    last_sent: Option<Instant>,
}
impl ProbeTiming {
    fn query_due(&self, now: Instant) -> bool {
        match (self.first_sent, self.last_sent) {
            (Some(first), Some(last)) => {
                now.saturating_duration_since(first) < RETRY_WINDOW
                    && now.saturating_duration_since(last) >= RETRY_INTERVAL
            }
            _ => true,
        }
    }
    fn sent(&mut self, now: Instant) {
        self.first_sent.get_or_insert(now);
        self.last_sent = Some(now);
    }
    fn initial_finished(&self, now: Instant) -> bool {
        self.first_sent
            .is_some_and(|first| now.saturating_duration_since(first) >= PROBE_TIMEOUT)
    }
}

fn is_foreground(fd: RawFd) -> bool {
    unsafe { libc::tcgetpgrp(fd) == libc::getpgrp() }
}

fn same_foreground_terminal(input: RawFd, output: RawFd) -> bool {
    unsafe {
        libc::isatty(input) == 1
            && libc::isatty(output) == 1
            && is_foreground(input)
            && is_foreground(output)
            && libc::tcgetsid(input) == libc::getsid(0)
            && libc::tcgetsid(output) == libc::getsid(0)
    }
}

struct TerminalMode {
    fd: OwnedFd,
    original: libc::termios,
}

impl TerminalMode {
    fn new(fd: OwnedFd) -> Option<Self> {
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd.as_raw_fd(), &mut original) } != 0 {
            return None;
        }
        let mut mode = original;
        mode.c_lflag &= !(libc::ICANON | libc::ECHO);
        mode.c_cc[libc::VMIN] = 0;
        mode.c_cc[libc::VTIME] = 0;
        if set_termios(fd.as_raw_fd(), &mode) != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        // Restore even after losing foreground ownership, without SIGTTOU stops.
        set_termios(self.fd.as_raw_fd(), &self.original);
    }
}

fn set_termios(fd: RawFd, mode: &libc::termios) -> libc::c_int {
    unsafe {
        let mut signals = std::mem::zeroed::<libc::sigset_t>();
        let mut previous = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut signals);
        libc::sigaddset(&mut signals, libc::SIGTTOU);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &signals, &mut previous) != 0 {
            return -1;
        }
        let mut result;
        loop {
            result = libc::tcsetattr(fd, libc::TCSANOW, mode);
            if result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        result
    }
}

/// A streaming APC parser with fixed maximum storage. Oversized messages are
/// discarded through ST, never interpreted as a truncated positive response.
struct ReplyParser {
    id: u32,
    state: u8,
    body: Vec<u8>,
    overflow: bool,
}

impl ReplyParser {
    fn new(id: u32) -> Self {
        Self {
            id,
            state: 0,
            body: Vec::new(),
            overflow: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut matched = false;
        for &byte in bytes {
            match self.state {
                0 => {
                    if byte == 0x1b {
                        self.state = 1;
                    }
                }
                1 => {
                    self.state = match byte {
                        b'_' => 2,
                        0x1b => 1,
                        _ => 0,
                    };
                }
                2 => {
                    self.state = if byte == b'G' {
                        3
                    } else if byte == 0x1b {
                        1
                    } else {
                        0
                    };
                    self.body.clear();
                    self.overflow = false;
                }
                3 => {
                    if byte == 0x1b {
                        self.state = 4;
                    } else if self.body.len() < MAX_REPLY {
                        self.body.push(byte);
                    } else {
                        self.overflow = true;
                    }
                }
                4 => {
                    if byte == b'\\' {
                        matched |= !self.overflow && self.positive();
                        self.state = 0;
                        self.body.clear();
                    } else {
                        // ESC inside the payload is invalid; discard until ST.
                        self.overflow = true;
                        self.state = if byte == 0x1b { 4 } else { 3 };
                    }
                }
                _ => unreachable!(),
            }
        }
        matched
    }

    fn positive(&self) -> bool {
        let Some(header) = self.body.strip_suffix(b";OK") else {
            return false;
        };
        let Some(number) = header.strip_prefix(b"i=") else {
            return false;
        };
        if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
            return false;
        }
        std::str::from_utf8(number)
            .ok()
            .and_then(|n| n.parse::<u32>().ok())
            == Some(self.id)
            && self.id != 0
    }
}

/// Display at the caller's cursor. Kitty's C=1 keeps that cursor stationary;
/// subsequent log redraws do not own or delete this separately placed image.
/// Specify only columns: setting both c/r stretches square QR modules according
/// to each terminal's cell aspect ratio. Kitty derives the height from the PNG.
pub fn transmit(png: &[u8], image_id: u32, columns: u16, _rows: u16) -> Vec<u8> {
    let encoded = STANDARD.encode(png);
    let chunks: Vec<_> = encoded.as_bytes().chunks(4096).collect();
    let mut result = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            result.extend_from_slice(
                format!("\x1b_Ga=T,f=100,t=d,i={image_id},q=2,C=1,c={columns},m={more};")
                    .as_bytes(),
            );
        } else {
            result.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        result.extend_from_slice(chunk);
        result.extend_from_slice(b"\x1b\\");
    }
    result
}

/// Delete only this dashboard's image and placements, never other clients' images.
pub fn delete(image_id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_link_keys_toggle_not_replies_encoded_keys_or_paste() {
        for sequence in [
            &b"\x1b_Gi=123;LATE\x1b\\"[..],
            &b"\x1b]10;rgb:LLLL/llll/0000\x07"[..],
            &b"\x1bPsynthetic L l\x1b\\"[..],
            &b"\x1b[1;2L"[..],
            &b"\x1b[108u"[..],
            &b"\x1bOL"[..],
            &b"\x1bO1;2L"[..],
            &b"\x1b_Gi=123;\x07L\x1b\\"[..],
            &b"\x1bL"[..],
            &b"\x1b[200~L l\x1b[201~"[..],
        ] {
            for split in 0..=sequence.len() {
                let mut keys = PrivateLinkKeys::default();
                assert_eq!(keys.feed(&sequence[..split]), 0);
                assert_eq!(keys.feed(&sequence[split..]), 0);
                assert_eq!(keys.feed(b"Ll"), 2);
            }
        }
        let mut keys = PrivateLinkKeys::default();
        assert_eq!(keys.feed(b"\x1b_G"), 0);
        assert_eq!(keys.feed(&vec![b'L'; 100_000]), 0);
        assert_eq!(keys.feed(b"\x1b\\l"), 1);
        assert_eq!(keys.feed(b"hello"), 2); // Plain l, not an input-forwarding path.
    }

    #[test]
    fn fallback_clock_starts_after_flush_and_retries_are_sparse_and_bounded() {
        let start = Instant::now();
        let mut timing = ProbeTiming::default();
        assert!(timing.query_due(start));
        assert!(!timing.initial_finished(start + Duration::from_secs(5)));
        let first = start + Duration::from_secs(5);
        timing.sent(first);
        assert!(!timing.initial_finished(first + Duration::from_millis(749)));
        assert!(timing.initial_finished(first + PROBE_TIMEOUT));
        assert!(!timing.query_due(first + Duration::from_millis(1999)));
        for seconds in (2..30).step_by(2) {
            let now = first + Duration::from_secs(seconds);
            assert!(timing.query_due(now));
            timing.sent(now);
            assert!(!timing.query_due(now));
        }
        assert!(!timing.query_due(first + RETRY_WINDOW));
        assert!(!timing.query_due(first + Duration::from_secs(300)));
    }

    #[test]
    fn png_is_crisp_with_four_module_quiet_zone() {
        let code = QrCode::new(b"https://example.invalid/fake-access").unwrap();
        let bytes = png_bytes(&code).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        let side = (code.width() + 8) * 4;
        assert_eq!((info.width, info.height), (side as u32, side as u32));
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        for y in 0..side {
            for x in 0..side {
                let mx = x / 4;
                let my = y / 4;
                let dark = mx >= 4
                    && my >= 4
                    && mx < code.width() + 4
                    && my < code.width() + 4
                    && code[(mx - 4, my - 4)] == Color::Dark;
                assert_eq!(pixels[y * side + x], if dark { 0 } else { 255 });
            }
        }
    }

    #[test]
    fn transmission_chunks_reassemble_and_delete_is_scoped() {
        for length in [1, 3072, 3073, 10000] {
            let data: Vec<_> = (0..length).map(|n| n as u8).collect();
            let output = String::from_utf8(transmit(&data, 123456, 20, 10)).unwrap();
            let parts: Vec<_> = output.split("\x1b\\").filter(|p| !p.is_empty()).collect();
            let mut encoded = String::new();
            for (index, part) in parts.iter().enumerate() {
                let (header, payload) = part.split_once(';').unwrap();
                assert!(header.starts_with("\x1b_G"));
                assert!(header.ends_with(if index + 1 == parts.len() {
                    "m=0"
                } else {
                    "m=1"
                }));
                assert!(payload.len() <= 4096);
                if index == 0 {
                    assert!(header.contains("a=T,f=100,t=d,i=123456,q=2,C=1,c=20"));
                } else {
                    assert!(!header.contains("a=T"));
                }
                encoded.push_str(payload);
            }
            assert_eq!(STANDARD.decode(encoded).unwrap(), data);
        }
        assert_eq!(delete(123456), b"\x1b_Ga=d,d=I,i=123456,q=2;\x1b\\");
    }

    #[test]
    fn only_matching_positive_replies_count_even_when_fragmented_or_duplicated() {
        let mut parser = ReplyParser::new(987654);
        for reply in [
            "\x1b_Gi=1;OK\x1b\\",
            "\x1b_Gi=987654;ENOENT\x1b\\",
            "\x1b_Gi=987654;OKextra\x1b\\",
            "\x1b_Gi=0;OK\x1b\\",
            "\x1b_Gi=987654,i=1;OK\x1b\\",
            "\x1b_Gi=42949672960;OK\x1b\\",
        ] {
            assert!(!parser.feed(reply.as_bytes()));
        }
        let reply = b"\x1b_Gi=987654;OK\x1b\\";
        for split in 0..reply.len() {
            let mut parser = ReplyParser::new(987654);
            assert!(!parser.feed(&reply[..split]));
            assert!(parser.feed(&reply[split..]));
            assert!(parser.feed(reply));
        }
        assert!(!ReplyParser::new(0).feed(b"\x1b_Gi=0;OK\x1b\\"));
    }

    #[test]
    fn malformed_and_oversized_replies_are_bounded_and_recover() {
        let mut parser = ReplyParser::new(987654);
        assert!(!parser.feed(b"\x1b_G"));
        assert!(!parser.feed(&vec![b'x'; 100_000]));
        assert!(parser.body.len() <= MAX_REPLY);
        assert!(!parser.feed(b"i=987654;OK\x1b\\"));
        assert!(!parser.feed(b"\x1b_Gi=987654;\x1bXOK\x1b\\"));
        assert!(parser.feed(b"noise\x1b_Gi=987654;OK\x1b\\"));
    }

    #[test]
    fn disposable_pty_reads_hotkeys_without_enter_and_drains_protocol_replies() {
        // An owned PTY only; never reads or changes the test runner's terminal.
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let terminal = TerminalMode::new(unsafe { OwnedFd::from_raw_fd(slave) }).unwrap();
        let mut keys = PrivateLinkKeys::default();
        let mut replies = ReplyParser::new(123456);
        for (input, expected, positive) in [
            (&b"\x1b_Gi=123456;OK\x1b\\"[..], 0, true),
            (&b"\x1b]10;LLLL\x07\x1b[1;2L"[..], 0, false),
            (&b"L"[..], 1, false),
            (&b"l"[..], 1, false),
        ] {
            assert_eq!(
                unsafe { libc::write(master.as_raw_fd(), input.as_ptr().cast(), input.len()) },
                input.len() as isize
            );
            let mut remaining = input.len();
            let mut toggles = 0;
            let mut matched = false;
            while remaining > 0 {
                let mut pfd = libc::pollfd {
                    fd: terminal.fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                assert_eq!(unsafe { libc::poll(&mut pfd, 1, 1000) }, 1);
                let mut bytes = [0u8; 64];
                let n = unsafe {
                    libc::read(
                        terminal.fd.as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                assert!(n > 0);
                remaining -= n as usize;
                toggles += keys.feed(&bytes[..n as usize]);
                matched |= replies.feed(&bytes[..n as usize]);
            }
            assert_eq!(toggles, expected);
            assert_eq!(matched, positive);
        }
    }

    #[test]
    fn disposable_pty_restores_termios_without_changing_shared_flags() {
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let mut before = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut before) },
            0
        );
        let flags = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) };
        let duplicate = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        assert!(duplicate >= 0);
        {
            let mode = TerminalMode::new(unsafe { OwnedFd::from_raw_fd(duplicate) }).unwrap();
            let mut during = before;
            assert_eq!(
                unsafe { libc::tcgetattr(mode.fd.as_raw_fd(), &mut during) },
                0
            );
            assert_eq!(
                during.c_lflag,
                before.c_lflag & !(libc::ICANON | libc::ECHO)
            );
            assert_eq!(during.c_cc[libc::VMIN], 0);
            assert_eq!(during.c_cc[libc::VTIME], 0);
            assert_eq!(during.c_iflag, before.c_iflag);
            assert_eq!(during.c_oflag, before.c_oflag);
            assert_eq!(during.c_cflag, before.c_cflag);
            assert_eq!(
                unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) },
                flags
            );
        }
        let mut after = before;
        assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut after) }, 0);
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        assert_eq!(after.c_cflag, before.c_cflag);
        // Darwin marks queued input for reprocessing when ICANON is restored.
        // PENDIN is kernel-owned transient state, not an unrestored input mode.
        #[cfg(target_os = "macos")]
        let kernel_flags = libc::PENDIN;
        #[cfg(not(target_os = "macos"))]
        let kernel_flags = 0;
        assert_eq!(
            after.c_lflag & !kernel_flags,
            before.c_lflag & !kernel_flags
        );
        assert_eq!(after.c_cc, before.c_cc);
        assert_eq!(unsafe { libc::cfgetispeed(&after) }, unsafe {
            libc::cfgetispeed(&before)
        });
        assert_eq!(unsafe { libc::cfgetospeed(&after) }, unsafe {
            libc::cfgetospeed(&before)
        });
        assert!(!same_foreground_terminal(
            slave.as_raw_fd(),
            slave.as_raw_fd()
        ));
    }
}
