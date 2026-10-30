use std::process::ExitCode;

use rubychat::app::{self, Options};

const USAGE: &str = "\
Usage: rubychat [OPTIONS] [SERVER]...

Connects to each SERVER at startup: host, host:port, ircs://host[:port] or
irc://host[:port]. TLS unless the address says irc:// or port 6667.

Options:
      --demo      Open placeholder networks to try the window chrome
  -h, --help      Print this help
  -V, --version   Print the version

Environment:
  RUBYCHAT_TRACE            Log each frame's timings and every event-loop wake
  RUBYCHAT_EXIT_AFTER=SECS  Quit cleanly after SECS seconds
  RUBYCHAT_SNAPSHOT=FILE    Save the window's last frame as a PNG on exit
";

fn main() -> ExitCode {
    let mut options = Options::default();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--demo" => options.demo = true,
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
    match app::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rubychat: {error}");
            ExitCode::FAILURE
        }
    }
}
