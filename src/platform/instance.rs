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
//! On Windows the lock file is in `%LOCALAPPDATA%\RubyChat` and the socket
//! is a named pipe, which only this user may open, and only from this
//! machine. The key takes the session for the display, since every session
//! sees the same pipes.
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
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

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
/// answers on, on Windows its pipe's first instance.
pub struct Server {
    lock: std::fs::File,
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
    #[cfg(windows)]
    name: String,
    #[cfg(windows)]
    first: std::os::windows::io::OwnedHandle,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Server")
    }
}

/// Becomes RubyChat, or hands `request` to the running one. An error means
/// neither worked; the caller then runs on its own.
#[cfg(unix)]
pub fn claim(request: &Request) -> Result<Claim, String> {
    let (lock, socket) = endpoint()?;
    claim_at(&lock, &socket, request)
}

#[cfg(windows)]
pub fn claim(request: &Request) -> Result<Claim, String> {
    let (lock, name) = pipe::endpoint()?;
    claim_at(&lock, &name, request)
}

#[cfg(not(any(unix, windows)))]
pub fn claim(_: &Request) -> Result<Claim, String> {
    Err(String::from(
        "single instance isn't built for this platform yet",
    ))
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

/// Takes the lock and the pipe, or hands `request` to the RubyChat that
/// has them.
#[cfg(windows)]
fn claim_at(lock: &Path, name: &str, request: &Request) -> Result<Claim, String> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        if let Some(server) = pipe::try_serve(lock, name)? {
            return Ok(Claim::Primary(server));
        }
        match pipe::hand_off(name, request) {
            Ok(()) => return Ok(Claim::HandedOff),
            // A RubyChat that is quitting closes its pipe: try again.
            Err(None) if attempts < 20 => std::thread::sleep(CONNECT_RETRY),
            Err(None) => return Err(String::from("the running RubyChat is quitting")),
            Err(Some(error)) => return Err(error),
        }
    }
}

/// The lock file and socket for this executable, profile and display.
#[cfg(unix)]
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
#[cfg(not(windows))]
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
    let stream = loop {
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
    exchange(stream, request)
}

/// Writes `request` and reads the answer. `Err(None)` means the running
/// RubyChat closed unanswered: it is quitting.
fn exchange(mut stream: impl Read + Write, request: &Request) -> Result<(), Option<String>> {
    let failed = |error: io::Error| Some(format!("handing over to the running RubyChat: {error}"));
    stream
        .write_all(request.to_text().as_bytes())
        .map_err(failed)?;
    let mut answer = String::new();
    BufReader::new(stream.take(64))
        .read_line(&mut answer)
        .map_err(failed)?;
    match answer.trim_end() {
        "ok" => Ok(()),
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

    /// Serves later launches on a thread of its own: each connection is
    /// answered on a short-lived thread, a few at once, so one that never
    /// finishes its request holds up no other.
    #[cfg(windows)]
    pub fn serve(self, sink: impl FnMut(Request) + Send + 'static) -> io::Result<()> {
        std::thread::Builder::new()
            .name(String::from("rubychat-instance"))
            .spawn(move || {
                let _lock = self.lock;
                pipe::serve(&self.name, self.first, sink);
            })
            .map(|_| ())
    }

    #[cfg(not(any(unix, windows)))]
    pub fn serve(self, _sink: impl FnMut(Request) + Send + 'static) -> io::Result<()> {
        Ok(())
    }
}

/// Reads one launch's request and says `ok`.
#[cfg(unix)]
fn answer(mut stream: std::os::unix::net::UnixStream) -> Option<Request> {
    stream.set_read_timeout(Some(HAND_OFF_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HAND_OFF_TIMEOUT)).ok()?;
    let request = read_request(&stream)?;
    stream.write_all(b"ok\n").ok()?;
    Some(request)
}

/// A request's lines, through `end`, at most `MAX_REQUEST` bytes.
fn read_request(stream: impl Read) -> Option<Request> {
    let mut text = String::new();
    let mut reader = BufReader::new(stream.take(MAX_REQUEST));
    loop {
        let before = text.len();
        if reader.read_line(&mut text).ok()? == 0 {
            return None;
        }
        if text[before..].trim_end() == "end" {
            break;
        }
    }
    Request::parse(&text)
}

/// The named pipe (CreateNamedPipeW, "Named Pipe Security and Access
/// Rights").
#[cfg(windows)]
mod pipe {
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, GENERIC_ALL, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    use super::{
        CONNECT_PATIENCE, HAND_OFF_TIMEOUT, Request, Server, exchange, instance_key, read_request,
    };
    use crate::platform::win32::wide;

    /// Connections answered at once, at most; past this a launch waits.
    const ANSWERING: usize = 4;
    /// The only revision there is (InitializeSecurityDescriptor).
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
    const BUFFER: u32 = 4096;

    /// The lock file, `instance-<key>.lock` in `%LOCALAPPDATA%\RubyChat`,
    /// and the pipe, `\\.\pipe\rubychat-<key>`, for this executable,
    /// profile and session.
    pub fn endpoint() -> Result<(std::path::PathBuf, String), String> {
        let executable = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|error| format!("can't locate RubyChat's executable: {error}"))?;
        let profile = crate::platform::paths::config_dir().ok_or("no configuration directory")?;
        let state = crate::platform::paths::state_dir().ok_or("no state directory")?;
        std::fs::create_dir_all(&state)
            .map_err(|error| format!("can't create {}: {error}", state.display()))?;
        let key = instance_key(&executable, &profile, &session());
        Ok((
            state.join(format!("instance-{key}.lock")),
            format!(r"\\.\pipe\rubychat-{key}"),
        ))
    }

    /// Every session sees the same pipes: each runs its own RubyChat.
    fn session() -> String {
        let mut session = 0;
        // SAFETY: plain values; `session` outlives the call.
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
        format!("session:{session}")
    }

    /// Takes the lock and the pipe's first instance; `None` when another
    /// RubyChat holds the lock, which the system releases however the
    /// process ends. A pipe's first instance can't be taken twice either
    /// ("creation of the next instance fails with ERROR_ACCESS_DENIED"),
    /// but Wine doesn't hold to that, so the lock decides.
    pub fn try_serve(lock: &std::path::Path, name: &str) -> Result<Option<Server>, String> {
        let file = File::options()
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
        match create(name, true) {
            Ok(first) => Ok(Some(Server {
                lock: file,
                name: name.to_string(),
                first,
            })),
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                Err(format!("another program has {name}"))
            }
            Err(error) => Err(format!("can't create {name}: {error}")),
        }
    }

    /// An instance of the pipe that only this user may open, and only
    /// from this machine.
    fn create(name: &str, first: bool) -> io::Result<OwnedHandle> {
        let security = OwnerOnly::new()?;
        let name = wide(name);
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: the name and the security attributes outlive the call.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                &security.attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new handle, owned from here.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    }

    /// A security descriptor whose list allows the process's user alone:
    /// the default would let everyone read ("Named Pipe Security and
    /// Access Rights").
    struct OwnerOnly {
        attributes: SECURITY_ATTRIBUTES,
        _descriptor: Box<SECURITY_DESCRIPTOR>,
        _acl: Vec<u64>,
    }

    impl OwnerOnly {
        fn new() -> io::Result<Self> {
            // SAFETY: every buffer is sized as the calls ask and outlives
            // them; the list is built in memory aligned for it, and the
            // descriptor points at it for as long as both live.
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut needed = 0u32;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
                let mut user = vec![0u64; (needed as usize).div_ceil(8)];
                let read = GetTokenInformation(
                    token,
                    TokenUser,
                    user.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                );
                CloseHandle(token);
                if read == 0 {
                    return Err(io::Error::last_os_error());
                }
                let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
                let size = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>()
                    + GetLengthSid(sid) as usize;
                let mut acl = vec![0u64; size.div_ceil(8)];
                let list = acl.as_mut_ptr().cast::<ACL>();
                if InitializeAcl(list, size as u32, ACL_REVISION) == 0
                    || AddAccessAllowedAce(list, ACL_REVISION, GENERIC_ALL, sid) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let mut descriptor: Box<SECURITY_DESCRIPTOR> = Box::default();
                let pointer = (&mut *descriptor as *mut SECURITY_DESCRIPTOR).cast();
                if InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) == 0
                    || SetSecurityDescriptorDacl(pointer, 1, list, 0) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self {
                    attributes: SECURITY_ATTRIBUTES {
                        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                        lpSecurityDescriptor: pointer,
                        bInheritHandle: 0,
                    },
                    _descriptor: descriptor,
                    _acl: acl,
                })
            }
        }
    }

    /// Sends `request` to the RubyChat holding the pipe. `Err(None)` means
    /// it is quitting.
    pub fn hand_off(name: &str, request: &Request) -> Result<(), Option<String>> {
        // The running RubyChat may raise its window: this launch, which
        // the user just started, may let it (AllowSetForegroundWindow).
        // SAFETY: a plain call.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        let started = Instant::now();
        let wide_name = wide(name);
        let file = loop {
            match File::options().read(true).write(true).open(name) {
                Ok(file) => break file,
                Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Err(None);
                }
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && started.elapsed() < CONNECT_PATIENCE =>
                {
                    // SAFETY: the name is NUL-terminated and outlives the
                    // call.
                    unsafe { WaitNamedPipeW(wide_name.as_ptr(), 500) };
                }
                Err(error) => {
                    return Err(Some(format!(
                        "the running RubyChat doesn't answer at {name}: {error}"
                    )));
                }
            }
        };
        // Pipes have no timeouts: the exchange gets a thread, and an answer
        // that doesn't come in time counts as none.
        let (sender, answer) = mpsc::channel();
        let request = request.clone();
        std::thread::spawn(move || {
            let _ = sender.send(exchange(&file, &request));
        });
        answer.recv_timeout(HAND_OFF_TIMEOUT).unwrap_or_else(|_| {
            Err(Some(String::from(
                "the running RubyChat didn't answer in time",
            )))
        })
    }

    /// Waits for launches on `first`, then on new instances.
    pub fn serve(name: &str, first: OwnedHandle, sink: impl FnMut(Request) + Send + 'static) {
        let sink = Arc::new(std::sync::Mutex::new(sink));
        let answering = Arc::new(AtomicUsize::new(0));
        let mut waiting = first;
        loop {
            // SAFETY: the handle is a pipe instance this thread owns.
            let connected = unsafe {
                ConnectNamedPipe(waiting.as_raw_handle_value(), std::ptr::null_mut()) != 0
                    || GetLastError() == ERROR_PIPE_CONNECTED
            };
            // The next launch connects to a fresh instance.
            let next = loop {
                match create(name, false) {
                    Ok(next) => break next,
                    Err(_) => std::thread::sleep(super::CONNECT_RETRY),
                }
            };
            let instance = std::mem::replace(&mut waiting, next);
            if !connected || answering.load(Ordering::SeqCst) >= ANSWERING {
                continue;
            }
            answering.fetch_add(1, Ordering::SeqCst);
            let (sink, finished) = (sink.clone(), answering.clone());
            let spawned = std::thread::Builder::new()
                .name(String::from("rubychat-launch"))
                .spawn(move || {
                    if let Some(request) = answer(File::from(instance))
                        && let Ok(mut sink) = sink.lock()
                    {
                        sink(request);
                    }
                    finished.fetch_sub(1, Ordering::SeqCst);
                });
            if spawned.is_err() {
                answering.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    /// Reads one launch's request and says `ok`, waiting until it is read
    /// before the instance closes.
    fn answer(mut file: File) -> Option<Request> {
        let request = read_request(&file)?;
        file.write_all(b"ok\n").ok()?;
        // SAFETY: the handle is the file's, open through the call.
        unsafe { FlushFileBuffers(file.as_raw_handle_value()) };
        Some(request)
    }

    /// A handle's value as windows-sys takes it.
    trait AsHandleValue {
        fn as_raw_handle_value(&self) -> HANDLE;
    }

    impl<T: std::os::windows::io::AsRawHandle> AsHandleValue for T {
        fn as_raw_handle_value(&self) -> HANDLE {
            self.as_raw_handle() as HANDLE
        }
    }
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

#[cfg(all(test, windows))]
mod windows_tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn a_second_launch_hands_over_through_the_pipe() {
        let id = std::process::id();
        let name = format!(r"\\.\pipe\rubychat-test-{id}");
        let lock = std::env::temp_dir().join(format!("rubychat-test-{id}.lock"));
        let Claim::Primary(server) = claim_at(&lock, &name, &Request::default()).unwrap() else {
            panic!("the first launch is RubyChat");
        };
        let (sender, requests) = mpsc::channel();
        server
            .serve(move |request| {
                let _ = sender.send(request);
            })
            .unwrap();
        for n in 0..3 {
            let launch = Request {
                show: true,
                addresses: vec![format!("irc.example.net/#{n}")],
                token: None,
            };
            assert!(matches!(
                claim_at(&lock, &name, &launch),
                Ok(Claim::HandedOff)
            ));
            assert_eq!(
                requests.recv_timeout(Duration::from_secs(5)).unwrap(),
                launch
            );
        }
    }
}
