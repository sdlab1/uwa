pub mod commands;
pub mod core;
pub mod prompt;
pub mod shell;
pub mod timing;

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum Cmd {
    Init,
    New {
        arg: String,
        #[arg(long)]
        no_red: Option<String>,
        #[arg(long)]
        no_crap: Option<String>,
    },
    Auto {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        max: Option<u32>,
    },
    #[command(alias = "status")]
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
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        max: Option<u32>,
    },
    Ps,
    Tickets {
        #[arg(long)]
        import: bool,
    },
}
