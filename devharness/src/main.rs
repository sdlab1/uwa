use clap::Command;
use devh::Cmd;

fn main() {
    let matches = Command::new("devh")
        .about("KISS dev harness: harness owns git, agent never runs it")
        .subcommands(Cmd::clap_subcommands())
        .get_matches();

    let cmd = Cmd::from_matches(&matches).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    let root = match devh::shell::git::root_from_cwd() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = devh::commands::dispatch(&cmd, &root) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
