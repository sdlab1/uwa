use clap::Command;
use devh::Cmd;

fn main() {
    // `devh msg ...` — канал агентов: обрабатывается ДО clap и без варианта Cmd,
    // чтобы не спорить с правкой диспетчера (commands/mod.rs) из других сессий.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("msg") {
        let rest: Vec<String> = argv.into_iter().skip(2).collect();
        std::process::exit(devh::shell::msg::run(&rest));
    }

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
            // Канал сообщений не привязан к репозиторию (хранилище в $DEVH_MSG_HOME).
            if matches!(&cmd, devh::Cmd::Msg { .. }) {
                std::path::PathBuf::from(".")
            } else {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    };
    if let Err(e) = devh::commands::dispatch(&cmd, &root) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
