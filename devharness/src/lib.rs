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
}

impl Cmd {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        vec![
            Command::new("init").about("Initialize a new dev harness project"),
            Command::new("new")
                .about("Create a new entry")
                .arg(Arg::new("arg").index(1).action(ArgAction::Set))
                .arg(
                    Arg::new("no_red")
                        .long("no-red")
                        .action(ArgAction::Set),
                )
                .arg(
                    Arg::new("no_crap")
                        .long("no-crap")
                        .action(ArgAction::Set),
                ),
            Command::new("auto")
                .about("Run automatic mode")
                .arg(Arg::new("from").long("from").action(ArgAction::Set))
                .arg(Arg::new("max").long("max").action(ArgAction::Set)),
            Command::new("list")
                .alias("status")
                .about("List entries"),
            Command::new("hint")
                .about("Show hint for an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("run")
                .about("Run an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("commit")
                .about("Commit an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("merge")
                .about("Merge an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("abort")
                .about("Abort an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("clean").about("Clean up"),
            Command::new("doctor").about("Run diagnostics"),
            Command::new("logs")
                .about("Show logs for an entry")
                .arg(Arg::new("id").long("id").action(ArgAction::Set)),
            Command::new("bg")
                .about("Run in background")
                .arg(Arg::new("id").index(1).action(ArgAction::Set))
                .arg(Arg::new("from").long("from").action(ArgAction::Set))
                .arg(Arg::new("max").long("max").action(ArgAction::Set)),
            Command::new("ps").about("Show process status"),
            Command::new("tickets")
                .about("Manage tickets")
                .arg(
                    Arg::new("import")
                        .long("import")
                        .action(ArgAction::SetTrue),
                ),
        ]
    }

    pub fn from_matches(matches: &clap::ArgMatches) -> Result<Self, String> {
        let name = matches.subcommand_name().unwrap_or("");
        let sub_matches = matches
            .subcommand_matches(name)
            .ok_or_else(|| format!("missing subcommand matches for '{name}'"))?;

        match name {
            "init" => Ok(Cmd::Init),
            "new" => Ok(Cmd::New {
                arg: sub_matches
                    .get_one::<String>("arg")
                    .ok_or_else(|| "missing required argument 'arg'".to_string())?
                    .clone(),
                no_red: sub_matches.get_one::<String>("no_red").cloned(),
                no_crap: sub_matches.get_one::<String>("no_crap").cloned(),
            }),
            "auto" => Ok(Cmd::Auto {
                from: sub_matches.get_one::<String>("from").cloned(),
                max: sub_matches
                    .get_one::<String>("max")
                    .and_then(|s| s.parse().ok()),
            }),
            "list" => Ok(Cmd::List),
            "hint" => Ok(Cmd::Hint {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "run" => Ok(Cmd::Run {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "commit" => Ok(Cmd::Commit {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "merge" => Ok(Cmd::Merge {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "abort" => Ok(Cmd::Abort {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "clean" => Ok(Cmd::Clean),
            "doctor" => Ok(Cmd::Doctor),
            "logs" => Ok(Cmd::Logs {
                id: sub_matches.get_one::<String>("id").cloned(),
            }),
            "bg" => Ok(Cmd::Bg {
                id: Some(
                    sub_matches
                        .get_one::<String>("id")
                        .ok_or_else(|| "missing required argument 'id'".to_string())?
                        .clone(),
                ),
                from: sub_matches.get_one::<String>("from").cloned(),
                max: sub_matches
                    .get_one::<String>("max")
                    .and_then(|s| s.parse().ok()),
            }),
            "ps" => Ok(Cmd::Ps),
            "tickets" => Ok(Cmd::Tickets {
                import: sub_matches.get_flag("import"),
            }),
            _ => Err(format!("unknown subcommand: {name}")),
        }
    }
}
