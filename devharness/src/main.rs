use clap::Parser;
use devh::Cmd;

#[derive(Parser)]
#[command(
    name = "devh",
    about = "KISS dev harness: harness owns git, agent never runs it"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

fn main() {
    let cli = Cli::parse();
    let root = match devh::shell::git::root_from_cwd() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = devh::commands::dispatch(&cli.cmd, &root) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
