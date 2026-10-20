pub mod commands;
pub mod core;
pub mod prompt;
pub mod shell;
pub mod timing;

use clap::{Arg, ArgAction, Command};

#[derive(Debug)]
pub enum Cmd {
    Init,
    New {
        arg: String,
        no_red: Option<String>,
        no_crap: Option<String>,
    },
    Auto {
        from: Option<String>,
        max: Option<u32>,
    },
    List,
    Hint {
        id: Option<String>,
    },
    Run {
        id: Option<String>,
    },
    Commit {
        id: Option<String>,
    },
    Merge {
        id: Option<String>,
    },
    Abort {
        id: Option<String>,
    },
    Clean,
    Doctor,
    Sweep {
        maxsize: Option<String>,
    },
    Logs {
        id: Option<String>,
    },
    Bg {
        id: Option<String>,
        from: Option<String>,
        max: Option<u32>,
    },
    Ps,
    Tickets {
        import: bool,
    },
    Survivors {
        /// режим классов RULES (`blind:*`): выстрелы, тишина, пенсия.
        rules: bool,
        /// порог пенсии K (сессий тишины); дефолт 10.
        sessions: Option<u32>,
        /// очистить `.harness/survivors-seen.json` (ручной инструмент §6).
        reset: bool,
    },
    /// B1: агрегация ledger'а побегов (только чтение — файл правится руками).
    Escapes,
}

impl Cmd {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        vec![
            Command::new("init"),
            Command::new("new")
                .arg(Arg::new("arg").index(1).required(true))
                .arg(Arg::new("no_red").long("no-red").action(ArgAction::Set))
                .arg(Arg::new("no_crap").long("no-crap").action(ArgAction::Set)),
            Command::new("auto")
                .arg(Arg::new("from").long("from").action(ArgAction::Set))
                .arg(
                    Arg::new("max")
                        .long("max")
                        .action(ArgAction::Set)
                        // clap stores Set-values as String by default, but
                        // the handlers read this as u32 (get_one::<u32>) —
                        // a type mismatch panics at runtime. Pin the parser.
                        .value_parser(clap::value_parser!(u32)),
                ),
            Command::new("list").alias("status"),
            Command::new("hint").arg(Arg::new("id").index(1)),
            Command::new("run").arg(Arg::new("id").index(1)),
            Command::new("commit").arg(Arg::new("id").index(1)),
            Command::new("merge").arg(Arg::new("id").index(1)),
            Command::new("abort").arg(Arg::new("id").index(1)),
            Command::new("clean"),
            Command::new("doctor"),
            Command::new("sweep").arg(
                Arg::new("maxsize")
                    .long("maxsize")
                    .action(ArgAction::Set)
                    .help("лимит размера target/ для cargo sweep (напр. 20G)"),
            ),
            Command::new("logs").arg(Arg::new("id").index(1)),
            Command::new("bg")
                .arg(Arg::new("id").index(1))
                .arg(Arg::new("from").long("from").action(ArgAction::Set))
                .arg(
                    Arg::new("max")
                        .long("max")
                        .action(ArgAction::Set)
                        // clap stores Set-values as String by default, but
                        // the handlers read this as u32 (get_one::<u32>) —
                        // a type mismatch panics at runtime. Pin the parser.
                        .value_parser(clap::value_parser!(u32)),
                ),
            Command::new("ps"),
            Command::new("tickets")
                .arg(Arg::new("import").long("import").action(ArgAction::SetTrue)),
            Command::new("survivors")
                .about("фактура выживших мутантов для дистилляции классов слепоты")
                .arg(
                    Arg::new("rules")
                        .long("rules")
                        .action(ArgAction::SetTrue)
                        .help(
                            "режим классов RULES (`blind:*`): жанры тега, выстрелы, тишина, пенсия",
                        ),
                )
                .arg(
                    Arg::new("sessions")
                        .long("sessions")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u32))
                        .help("K сессий тишины для пенсии (дефолт 10)"),
                )
                .arg(
                    Arg::new("reset")
                        .long("reset")
                        .action(ArgAction::SetTrue)
                        .help("очистить .harness/survivors-seen.json (ручной инструмент)"),
                ),
            Command::new("escapes")
                .about("ledger побегов: классы, streak, корпус, уровень автономии (чтение)"),
        ]
    }

    pub fn from_matches(matches: &clap::ArgMatches) -> Result<Self, String> {
        match matches.subcommand_name() {
            Some("init") => Ok(Cmd::Init),
            Some("new") => {
                let m = matches
                    .subcommand_matches("new")
                    .unwrap_or_else(|| unreachable!());
                let arg = m
                    .get_one::<String>("arg")
                    .unwrap_or_else(|| unreachable!())
                    .clone();
                let no_red = m.get_one::<String>("no_red").cloned();
                let no_crap = m.get_one::<String>("no_crap").cloned();
                Ok(Cmd::New {
                    arg,
                    no_red,
                    no_crap,
                })
            }
            Some("auto") => {
                let m = matches
                    .subcommand_matches("auto")
                    .unwrap_or_else(|| unreachable!());
                let from = m.get_one::<String>("from").cloned();
                let max = m.get_one::<u32>("max").copied();
                Ok(Cmd::Auto { from, max })
            }
            Some("list") => Ok(Cmd::List),
            Some("hint") => {
                let m = matches
                    .subcommand_matches("hint")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Hint { id })
            }
            Some("run") => {
                let m = matches
                    .subcommand_matches("run")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Run { id })
            }
            Some("commit") => {
                let m = matches
                    .subcommand_matches("commit")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Commit { id })
            }
            Some("merge") => {
                let m = matches
                    .subcommand_matches("merge")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Merge { id })
            }
            Some("abort") => {
                let m = matches
                    .subcommand_matches("abort")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Abort { id })
            }
            Some("clean") => Ok(Cmd::Clean),
            Some("doctor") => Ok(Cmd::Doctor),
            Some("sweep") => {
                let m = matches
                    .subcommand_matches("sweep")
                    .unwrap_or_else(|| unreachable!());
                let maxsize = m.get_one::<String>("maxsize").cloned();
                Ok(Cmd::Sweep { maxsize })
            }
            Some("logs") => {
                let m = matches
                    .subcommand_matches("logs")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                Ok(Cmd::Logs { id })
            }
            Some("bg") => {
                let m = matches
                    .subcommand_matches("bg")
                    .unwrap_or_else(|| unreachable!());
                let id = m.get_one::<String>("id").cloned();
                let from = m.get_one::<String>("from").cloned();
                let max = m.get_one::<u32>("max").copied();
                Ok(Cmd::Bg { id, from, max })
            }
            Some("ps") => Ok(Cmd::Ps),
            Some("tickets") => {
                let m = matches
                    .subcommand_matches("tickets")
                    .unwrap_or_else(|| unreachable!());
                let import = m.get_one::<bool>("import").copied().unwrap_or(false);
                Ok(Cmd::Tickets { import })
            }
            Some("survivors") => {
                let m = matches
                    .subcommand_matches("survivors")
                    .unwrap_or_else(|| unreachable!());
                let rules = m.get_one::<bool>("rules").copied().unwrap_or(false);
                let sessions = m.get_one::<u32>("sessions").copied();
                let reset = m.get_one::<bool>("reset").copied().unwrap_or(false);
                Ok(Cmd::Survivors {
                    rules,
                    sessions,
                    reset,
                })
            }
            Some("escapes") => Ok(Cmd::Escapes),
            _ => Err("unknown subcommand".to_string()),
        }
    }
}
