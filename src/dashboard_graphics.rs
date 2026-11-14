//! Native inline QR images, enabled only by a positive Kitty graphics reply.
//! No terminal names, environment variables, or core FACTS imply support.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{Color, QrCode};

const PROBE_TIMEOUT: Duration = Duration::from_millis(750);
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
    deadline: Instant,
    supported: bool,
    finished: bool,
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
            deadline: Instant::now() + PROBE_TIMEOUT,
            supported: false,
            finished: false,
        })
    }

    /// Verified one-RGB-pixel Kitty query. A positive reply establishes native
    /// graphics support; Herdr's native implementation also supports PNG f=100.
    /// Call once and flush stdout before polling; the deadline starts at start().
    pub fn query(&self) -> Vec<u8> {
        format!(
            "\x1b_Gi={},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",
            self.image_id()
        )
        .into_bytes()
    }

    /// Bounded nonblocking drain, without changing shared file status flags.
    pub fn poll(&mut self) {
        if Instant::now() >= self.deadline {
            self.finished = true;
        }
        let fd = self.terminal.fd.as_raw_fd();
        if !is_foreground(fd) {
            return; // Reading a background tty could otherwise raise SIGTTIN.
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
            let matched = self.parser.feed(&bytes[..n as usize]);
            if matched && !self.finished {
                self.supported = true;
                self.finished = true;
            }
        }
    }

    pub fn supported(&self) -> bool {
        self.supported
    }
    pub fn finished(&self) -> bool {
        self.finished || Instant::now() >= self.deadline
    }
    pub fn image_id(&self) -> u32 {
        self.parser.id
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
