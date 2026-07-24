use clap::Parser;
use clap::error::ErrorKind;
use uparser_core::cli::{self, Cli, EXIT_USAGE};

fn main() {
    match Cli::try_parse() {
        Ok(cli) => std::process::exit(cli::run(cli)),
        Err(e) => match e.kind() {
            // --help/--version are not usage errors — let clap print to
            // stdout and exit 0 as it normally would.
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => e.exit(),
            _ => {
                eprintln!("{e}");
                std::process::exit(EXIT_USAGE);
            }
        },
    }
}
