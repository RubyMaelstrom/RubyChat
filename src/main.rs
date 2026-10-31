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
      --show      Show the running RubyChat's window
      --tray      Start in the tray, without a window
      --demo      Open placeholder networks to try the window chrome
  -h, --help      Print this help
  -V, --version   Print the version

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
    let mut show = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
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
