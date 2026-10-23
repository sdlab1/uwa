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
        no_files: Option<String>,
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
    /// Мутационный аудит: ЕДИНСТВЕННЫЙ вход (start/status/watch/verdict/help).
    /// Раньше аудит вёлся внешним bash-скриптом, который «нельзя было
    /// запускать руками» — то есть нельзя было запустить ВООБЩЕ. Теперь
    /// вход — подкоманда самого харнесса: одна команда на весь цикл.
    Mutate {
        /// Подкоманда: start | status | watch | verdict | help.
        sub: String,
        /// Остальные аргументы (для `start` — флаги cargo-mutants).
        args: Vec<String>,
    },
    /// Полный сьют харнесса (authoritative backend — `devharness/test.sh`).
    Test {
        /// Аргументы: `--list`, `--case <N|имя>`.
        args: Vec<String>,
    },
    /// Оракул следующего шага: ровно одна исполняемая команда (всегда rc=0).
    Next,
    /// Мини-описание агента: адрес, модель, рантайм, сводка system prompt.
    About,
    /// Печать канонического workflow из кода (машинно-читаемая проекция).
    Workflow,
    /// Канал сообщений между агентами: координация 2+ агентов (по каналам/темам).
    Msg {
        /// Аргументы после `msg`: send|read|watch|list|prune + флаги.
        args: Vec<String>,
    },
    /// Оркестратор: реестр задач ведомых, шкалы P0-P5/F0-F5, дашборд ведущего.
    Orch {
        /// Подкоманда: board|list|add|start|done|fail|revoke|reassign|degrade|model|zones.
        sub: String,
        /// Остальные аргументы (флаги подкоманды).
        args: Vec<String>,
    },
}

impl Default for Cmd {
    /// Нейтральный дефолт диспетчера — чтение списка (побочных эффектов нет).
    /// Кроме смысла — мутационная гигиена: fn-replace мутант
    /// `replace Cmd::from_matches -> Ok(Default::default())` без `Cmd: Default`
    /// не компилируется (unviable = спрятанная дыра, fast_cycle §P9); с
    /// дефолтом он viable, и его ловит from_matches_dispatches_subcommands.
    fn default() -> Self {
        Cmd::List
    }
}

impl Cmd {
    pub fn clap_subcommands() -> Vec<clap::Command> {
        vec![
            Command::new("init"),
            Command::new("new")
                .arg(Arg::new("arg").index(1).required(true))
                .arg(Arg::new("no_red").long("no-red").action(ArgAction::Set))
                .arg(Arg::new("no_crap").long("no-crap").action(ArgAction::Set))
                .arg(Arg::new("no_files").long("no-files").action(ArgAction::Set)),
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
            Command::new("mutate")
                .about("мутационный аудит: ЕДИНСТВЕННЫЙ вход (start/status/watch/verdict/help)")
                .arg(
                    Arg::new("sub")
                        .index(1)
                        .default_value("help")
                        .help("start | status | watch | verdict | help"),
                )
                .arg(
                    Arg::new("rest")
                        .index(2)
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("аргументы: для start — флаги cargo-mutants (передаются как есть)"),
                ),
            Command::new("test")
                .about("полный сьют харнесса (test.sh): полный лог + PASS/FAIL + код возврата")
                .arg(
                    Arg::new("args")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("--list — список проверок; --case <N|имя> — точечная проверка"),
                ),
            Command::new("next").about("оракул следующего шага: ровно одна команда (всегда rc=0)"),
            Command::new("workflow").about("печать канонического workflow из кода"),
            Command::new("about")
                .about("мини-описание агента: адрес, модель, рантайм, сводка system prompt"),
            Command::new("msg")
                .about("канал сообщений между агентами: send|read|watch|list|prune")
                .arg(
                    Arg::new("args")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("аргументы: send|read|watch|list|prune + флаги"),
                ),
            Command::new("orch")
                .about("оркестратор: реестр задач, шкалы P0-P5/F0-F5, дашборд")
                .arg(
                    Arg::new("sub")
                        .index(1)
                        .default_value("board")
                        .help("board|list|add|start|done|fail|revoke|reassign|degrade|model|zones"),
                )
                .arg(
                    Arg::new("rest")
                        .index(2)
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true)
                        .help("аргументы подкоманды"),
                ),
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
                let no_files = m.get_one::<String>("no_files").cloned();
                Ok(Cmd::New {
                    arg,
                    no_red,
                    no_crap,
                    no_files,
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
            Some("mutate") => {
                let m = matches
                    .subcommand_matches("mutate")
                    .unwrap_or_else(|| unreachable!());
                let sub = m
                    .get_one::<String>("sub")
                    .cloned()
                    .unwrap_or_else(|| "help".to_string());
                let args = m
                    .get_many::<String>("rest")
                    .map(|v| v.cloned().collect::<Vec<String>>())
                    .unwrap_or_default();
                Ok(Cmd::Mutate { sub, args })
            }
            Some("test") => {
                let m = matches
                    .subcommand_matches("test")
                    .unwrap_or_else(|| unreachable!());
                let args = m
                    .get_many::<String>("args")
                    .map(|v| v.cloned().collect::<Vec<String>>())
                    .unwrap_or_default();
                Ok(Cmd::Test { args })
            }
            Some("next") => Ok(Cmd::Next),
            Some("workflow") => Ok(Cmd::Workflow),
            Some("about") => Ok(Cmd::About),
            Some("msg") => {
                let m = matches
                    .subcommand_matches("msg")
                    .unwrap_or_else(|| unreachable!());
                let args = m
                    .get_many::<String>("args")
                    .map(|v| v.cloned().collect::<Vec<String>>())
                    .unwrap_or_default();
                Ok(Cmd::Msg { args })
            }
            Some("orch") => {
                let m = matches
                    .subcommand_matches("orch")
                    .unwrap_or_else(|| unreachable!());
                let sub = m
                    .get_one::<String>("sub")
                    .cloned()
                    .unwrap_or_else(|| "board".to_string());
                let args = m
                    .get_many::<String>("rest")
                    .map(|v| v.cloned().collect::<Vec<String>>())
                    .unwrap_or_default();
                Ok(Cmd::Orch { sub, args })
            }
            _ => Err("unknown subcommand".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch(argv: &[&str]) -> Cmd {
        let mut args = vec!["devh"];
        args.extend_from_slice(argv);
        let matches = clap::Command::new("devh")
            .subcommands(Cmd::clap_subcommands())
            .get_matches_from(args);
        Cmd::from_matches(&matches).unwrap_or_else(|e| unreachable!("dispatch: {e}"))
    }

    #[test]
    fn from_matches_dispatches_subcommands() {
        // Ловит мутанта `from_matches -> Ok(Default::default())`: любая из
        // этих команд отличается от дефолта Cmd::List.
        assert!(matches!(dispatch(&["init"]), Cmd::Init));
        assert!(matches!(dispatch(&["list"]), Cmd::List));
        assert!(matches!(dispatch(&["clean"]), Cmd::Clean));
        assert!(matches!(dispatch(&["escapes"]), Cmd::Escapes));
        assert!(matches!(dispatch(&["next"]), Cmd::Next));
        assert!(matches!(dispatch(&["workflow"]), Cmd::Workflow));
        assert!(matches!(dispatch(&["test"]), Cmd::Test { .. }));
        assert!(matches!(dispatch(&["test", "--list"]), Cmd::Test { .. }));
    }

    #[test]
    fn from_matches_unknown_subcommand_is_err() {
        // Защитная ветка `_ => Err`: clap режет неизвестное на парсинге,
        // но контракт from_matches — Err, а не паника.
        let empty = clap::ArgMatches::default();
        assert!(Cmd::from_matches(&empty).is_err());
    }
}
