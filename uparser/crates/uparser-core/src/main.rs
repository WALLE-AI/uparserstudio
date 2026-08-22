use clap::Parser;
use clap::error::ErrorKind;
use std::process::ExitCode;
use uparser_core::cli::{self, Cli, EXIT_USAGE};

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => ExitCode::from(cli::run(cli) as u8),
        Err(e) => {
            let code = match e.kind() {
                // --help/--version are not usage errors — let clap print to
                // stdout and exit 0 as it normally would.
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => EXIT_USAGE,
            };
            let _ = e.print();
            ExitCode::from(code as u8)
        }
    }
}
