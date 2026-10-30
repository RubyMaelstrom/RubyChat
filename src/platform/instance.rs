//! One RubyChat per profile and display (DESIGN.md §12.4). A launch while
//! RubyChat runs hands its addresses (`irc://` links, say) and activation
//! token to the running one, which opens them and shows its window, and
//! ends.
//!
//! As in trust-desktop, the first launch holds an exclusive lock on a file
//! beside a Unix socket in `$XDG_RUNTIME_DIR` for its whole life (XDG Base
//! Directory Specification: "Applications should use this directory for
//! communication and synchronization purposes"). The operating system
//! releases the lock however the process ends, so a stale socket never
//! stops a later launch from taking over. The instance is keyed by the
//! executable, the configuration directory and the display, so a
//! development build, another profile or another display each run their
//! own.
//!
//! A launch writes a few lines and waits for `ok`:
//!
//! ```text
//! RUBYCHAT 1
//! show
//! token <activation token>
//! open <address>
//! end
//! ```

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The first line of a request; a running RubyChat refuses others.
const GREETING: &str = "RUBYCHAT 1";
/// The longest request read.
const MAX_REQUEST: u64 = 64 * 1024;
/// Addresses taken from one launch.
const MAX_ADDRESSES: usize = 32;
/// How long a launch waits for the running RubyChat's answer, and how long
/// the running one waits for a launch to finish its request.
const HAND_OFF_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a launch that found the lock taken keeps trying the socket,
/// which the running RubyChat opens just after taking the lock.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(50);

/// What a launch asks of the running RubyChat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// Show the window: a plain launch or `--show`, not `--tray`.
    pub show: bool,
    /// Addresses to open, as the command line gave them.
    pub addresses: Vec<String>,
    /// The launch's activation token, which lets the running window take
    /// focus on Wayland (xdg-activation-v1 allows passing it "through
    /// means not described in this protocol").
    pub token: Option<String>,
}

impl Request {
    fn to_text(&self) -> String {
        let mut text = format!("{GREETING}\n");
        if self.show {
            text.push_str("show\n");
        }
        if let Some(token) = &self.token {
            text.push_str(&format!("token {token}\n"));
        }
        for address in &self.addresses {
            text.push_str(&format!("open {address}\n"));
        }
        text.push_str("end\n");
        text
    }

    /// Reads a request; `None` if it isn't one. Lines with control
    /// characters are refused, so nothing smuggles a line break further.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != GREETING {
            return None;
        }
        let mut request = Request::default();
        for line in lines {
            if line.chars().any(char::is_control) {
                return None;
            }
            match line.split_once(' ').unwrap_or((line, "")) {
                ("show", "") => request.show = true,
                ("token", token) if !token.is_empty() => request.token = Some(token.to_string()),
                ("open", address) if !address.is_empty() => {
                    if request.addresses.len() < MAX_ADDRESSES {
                        request.addresses.push(address.to_string());
                    }
                }
                ("end", "") => return Some(request),
                // Lines a newer RubyChat might send are skipped.
                _ => {}
            }
        }
        None
    }
}

/// The outcome of `claim`.
pub enum Claim {
    /// This process is RubyChat; it serves later launches.
    Primary(Server),
    /// The running RubyChat took this launch's request.
    HandedOff,
}

/// The running RubyChat's side: the lock it holds and the socket it
/// answers on.
pub struct Server {
    #[cfg(unix)]
    lock: std::fs::File,
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Server")
    }
}

/// Becomes RubyChat, or hands `request` to the running one. An error means
/// neither worked; the caller then runs on its own.
pub fn claim(request: &Request) -> Result<Claim, String> {
    let (lock, socket) = endpoint()?;
    claim_at(&lock, &socket, request)
}

#[cfg(unix)]
fn claim_at(lock: &Path, socket: &Path, request: &Request) -> Result<Claim, String> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        if let Some(server) = try_serve(lock, socket)? {
            return Ok(Claim::Primary(server));
        }
        match hand_off(socket, request) {
            Ok(()) => return Ok(Claim::HandedOff),
            // A RubyChat that is quitting gives up its lock: try again.
            Err(None) if attempts < 20 => std::thread::sleep(CONNECT_RETRY),
            Err(None) => return Err(String::from("the running RubyChat is quitting")),
            Err(Some(error)) => return Err(error),
        }
    }
}

#[cfg(not(unix))]
fn claim_at(_: &Path, _: &Path, _: &Request) -> Result<Claim, String> {
    Err(String::from(
        "single instance isn't built for this platform yet",
    ))
}

/// The lock file and socket for this executable, profile and display.
fn endpoint() -> Result<(PathBuf, PathBuf), String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("can't locate RubyChat's executable: {error}"))?;
    let profile = super::paths::config_dir().ok_or("no configuration directory")?;
    let key = instance_key(&executable, &profile, &display_name());
    let directory = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(runtime) if runtime.is_absolute() => runtime.join("rubychat"),
        // "Applications should fall back to a replacement directory with
        // similar capabilities and print a warning message."
        _ => {
            let state = super::paths::state_dir().ok_or("no state directory")?;
            eprintln!(
                "rubychat: XDG_RUNTIME_DIR isn't set; keeping RubyChat's socket in {}",
                state.display()
            );
            state
        }
    };
    create_private_dir(&directory)?;
    Ok((
        directory.join(format!("instance-{key}.lock")),
        directory.join(format!("instance-{key}.sock")),
    ))
}

#[cfg(unix)]
fn create_private_dir(directory: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|error| format!("can't create {}: {error}", directory.display()))
}

#[cfg(not(unix))]
fn create_private_dir(directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("can't create {}: {error}", directory.display()))
}

/// A short, stable name: SHA-256 of the parts, each prefixed with its
/// length.
fn instance_key(executable: &Path, profile: &Path, display: &str) -> String {
    let mut bytes = Vec::new();
    for part in [
        executable.as_os_str().as_encoded_bytes(),
        profile.as_os_str().as_encoded_bytes(),
        display.as_bytes(),
    ] {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part);
    }
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &bytes);
    digest.as_ref()[..10]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The display windows open on: launches on another display run their
/// own RubyChat there.
fn display_name() -> String {
    match (std::env::var("WAYLAND_DISPLAY"), std::env::var("DISPLAY")) {
        (Ok(wayland), _) if !wayland.is_empty() => format!("wayland:{wayland}"),
        (_, Ok(x11)) if !x11.is_empty() => format!("x11:{x11}"),
        _ => String::new(),
    }
}

/// Takes the lock and opens the socket; `None` when another process holds
/// the lock.
#[cfg(unix)]
fn try_serve(lock: &Path, socket: &Path) -> Result<Option<Server>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .map_err(|error| format!("can't open {}: {error}", lock.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(format!("can't lock {}: {error}", lock.display()));
        }
    }
    // The lock proves no RubyChat answers here: a socket left over is from
    // one that ended.
    let _ = std::fs::remove_file(socket);
    let listener = std::os::unix::net::UnixListener::bind(socket)
        .map_err(|error| format!("can't listen on {}: {error}", socket.display()))?;
    Ok(Some(Server {
        lock: file,
        listener,
    }))
}

/// Sends `request` to the RubyChat holding the lock. `Err(None)` means it
/// is quitting.
#[cfg(unix)]
fn hand_off(socket: &Path, request: &Request) -> Result<(), Option<String>> {
    let started = Instant::now();
    let mut stream = loop {
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if started.elapsed() < CONNECT_PATIENCE => std::thread::sleep(CONNECT_RETRY),
            Err(error) => {
                return Err(Some(format!(
                    "the running RubyChat doesn't answer at {}: {error}",
                    socket.display()
                )));
            }
        }
    };
    let failed = |error: io::Error| Some(format!("handing over to the running RubyChat: {error}"));
    stream
        .set_read_timeout(Some(HAND_OFF_TIMEOUT))
        .map_err(failed)?;
    stream
        .set_write_timeout(Some(HAND_OFF_TIMEOUT))
        .map_err(failed)?;
    stream
        .write_all(request.to_text().as_bytes())
        .map_err(failed)?;
    let mut answer = String::new();
    BufReader::new(stream.take(64))
        .read_line(&mut answer)
        .map_err(failed)?;
    match answer.trim_end() {
        "ok" => Ok(()),
        // Closed unanswered: it is quitting.
        "" => Err(None),
        other => Err(Some(format!("the running RubyChat said {other:?}"))),
    }
}

impl Server {
    /// Serves later launches on a thread of its own, which `sink` hears
    /// from; the thread and the lock last as long as the process.
    #[cfg(unix)]
    pub fn serve(self, mut sink: impl FnMut(Request) + Send + 'static) -> io::Result<()> {
        std::thread::Builder::new()
            .name(String::from("rubychat-instance"))
            .spawn(move || {
                let _lock = self.lock;
                for stream in self.listener.incoming() {
                    let Ok(stream) = stream else {
                        std::thread::sleep(CONNECT_RETRY);
                        continue;
                    };
                    if let Some(request) = answer(stream) {
                        sink(request);
                    }
                }
            })
            .map(|_| ())
    }

    #[cfg(not(unix))]
    pub fn serve(self, _sink: impl FnMut(Request) + Send + 'static) -> io::Result<()> {
        Ok(())
    }
}

/// Reads one launch's request and says `ok`.
#[cfg(unix)]
fn answer(mut stream: std::os::unix::net::UnixStream) -> Option<Request> {
    stream.set_read_timeout(Some(HAND_OFF_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HAND_OFF_TIMEOUT)).ok()?;
    let mut text = String::new();
    let mut reader = BufReader::new((&stream).take(MAX_REQUEST));
    loop {
        let before = text.len();
        if reader.read_line(&mut text).ok()? == 0 {
            return None;
        }
        if text[before..].trim_end() == "end" {
            break;
        }
    }
    let request = Request::parse(&text)?;
    stream.write_all(b"ok\n").ok()?;
    Some(request)
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn requests_round_trip_and_refuse_smuggling() {
        let request = Request {
            show: true,
            addresses: vec![String::from("ircs://irc.libera.chat/#rust")],
            token: Some(String::from("token-1")),
        };
        assert_eq!(Request::parse(&request.to_text()), Some(request));
        assert_eq!(Request::parse("HELLO\nend\n"), None);
        assert_eq!(Request::parse("RUBYCHAT 1\nopen a\u{1b}b\nend\n"), None);
        assert_eq!(Request::parse("RUBYCHAT 1\nshow\n"), None, "no end");
        let newer = Request::parse("RUBYCHAT 1\nfuture thing\nshow\nend\n").unwrap();
        assert!(newer.show);
    }

    #[test]
    fn a_second_launch_hands_over_and_a_stale_socket_is_taken_over() {
        let directory =
            std::env::temp_dir().join(format!("rubychat-instance-test-{}", std::process::id()));
        create_private_dir(&directory).unwrap();
        let (lock, socket) = (directory.join("i.lock"), directory.join("i.sock"));
        // A socket left behind by a RubyChat that ended.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let Claim::Primary(server) = claim_at(&lock, &socket, &Request::default()).unwrap() else {
            panic!("the first launch is RubyChat");
        };
        let (sender, requests) = mpsc::channel();
        server
            .serve(move |request| {
                let _ = sender.send(request);
            })
            .unwrap();
        let launch = Request {
            show: true,
            addresses: vec![String::from("irc.oftc.net")],
            token: None,
        };
        assert!(matches!(
            claim_at(&lock, &socket, &launch),
            Ok(Claim::HandedOff)
        ));
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(5)).unwrap(),
            launch
        );
        assert_eq!(
            instance_key(Path::new("/a"), Path::new("/b"), "wayland:wayland-0").len(),
            20
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
