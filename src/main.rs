// A window, not a console program: Explorer opens no console beside it.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use rubychat::app::{self, Options};
use rubychat::platform::instance::{self, Claim, Request};

const USAGE: &str = "\
Usage: rubychat [OPTIONS] [SERVER]...

Connects to each SERVER at startup: host, host:port, ircs://host[:port] or
irc://host[:port], with an optional channel. TLS unless the address says
irc:// or port 6667. While RubyChat runs, a later launch hands its servers
to it and shows its window.

Options:
      --show        Show the running RubyChat's window
      --tray        Start in the tray, without a window
      --demo        Open placeholder networks to try the window chrome
      --register    Windows: open irc:// and ircs:// links in this RubyChat
      --unregister  Windows: stop opening them here
  -h, --help        Print this help
  -V, --version     Print the version
      --            Treat what follows as servers, even if it starts with -

Environment:
  RUBYCHAT_TRACE            Log each frame's timings and every event-loop wake
  RUBYCHAT_EXIT_AFTER=SECS  Quit cleanly after SECS seconds
  RUBYCHAT_SNAPSHOT=FILE    Save the window's last frame as a PNG on exit
";

fn main() -> ExitCode {
    let mut options = Options {
        launched: Some(std::time::Instant::now()),
        ..Options::default()
    };
    #[cfg(windows)]
    attach_console();
    let mut show = false;
    let mut servers_only = false;
    for argument in std::env::args().skip(1) {
        if servers_only {
            options.connect.push(argument);
            continue;
        }
        match argument.as_str() {
            "--" => servers_only = true,
            "--register" | "--unregister" => return register(argument == "--register"),
            "--demo" => options.demo = true,
            "--show" => show = true,
            "--tray" => options.tray = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("rubychat {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("rubychat: unknown option '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
            server => options.connect.push(server.to_string()),
        }
    }
    options.token = activation_token();
    // The placeholder networks of --demo stand apart from the real ones.
    if !options.demo {
        let request = Request {
            show: show || !options.tray,
            addresses: options.connect.clone(),
            token: options.token.clone(),
        };
        match instance::claim(&request) {
            Ok(Claim::HandedOff) => return ExitCode::SUCCESS,
            Ok(Claim::Primary(server)) => options.instance = Some(server),
            Err(error) => eprintln!("rubychat: {error}; running on its own"),
        }
    }
    match app::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rubychat: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `--register` and `--unregister`: irc:// links on Windows.
#[cfg(windows)]
fn register(register: bool) -> ExitCode {
    use rubychat::platform::win32::register;
    let result = if register {
        std::env::current_exe()
            .map_err(|error| format!("can't locate RubyChat's executable: {error}"))
            .and_then(|executable| register::register(&executable))
    } else {
        register::unregister()
    };
    match result {
        Ok(()) if register => {
            println!("irc:// and ircs:// links now open in this RubyChat.");
            ExitCode::SUCCESS
        }
        Ok(()) => {
            println!("irc:// and ircs:// links no longer open in RubyChat.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rubychat: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn register(_: bool) -> ExitCode {
    eprintln!(
        "rubychat: --register and --unregister are for Windows; on Linux, \
         rubychat.desktop registers irc:// links"
    );
    ExitCode::from(2)
}

/// Started from a console, RubyChat writes there (help, the version,
/// errors and traces); started from Explorer there is none to attach.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: a plain call, before any output.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// The token whatever launched RubyChat left for its window
/// (`XDG_ACTIVATION_TOKEN` on Wayland, `DESKTOP_STARTUP_ID` on X11), taken
/// out of the environment so the programs RubyChat starts don't reuse it.
fn activation_token() -> Option<String> {
    let mut token = None;
    for variable in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
        if let Ok(value) = std::env::var(variable) {
            token = token.or(Some(value).filter(|value| !value.is_empty()));
            // SAFETY: no other thread runs yet.
            unsafe { std::env::remove_var(variable) };
        }
    }
    token
}
