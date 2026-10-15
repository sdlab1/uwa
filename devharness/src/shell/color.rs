#[derive(Clone, Copy)]
pub struct Colors {
    pub g: &'static str,
    pub r: &'static str,
    pub y: &'static str,
    pub c: &'static str,
    pub b: &'static str,
    pub d: &'static str,
    pub z: &'static str,
}

const ON: Colors = Colors {
    g: "\x1b[32m",
    r: "\x1b[31m",
    y: "\x1b[33m",
    c: "\x1b[36m",
    b: "\x1b[1m",
    d: "\x1b[2m",
    z: "\x1b[0m",
};

const OFF: Colors = Colors {
    g: "",
    r: "",
    y: "",
    c: "",
    b: "",
    d: "",
    z: "",
};

pub fn colors() -> Colors {
    let force = match std::env::var("HARNESS_COLOR").ok().as_deref() {
        Some("always") => Some(true),
        Some("never") => Some(false),
        _ => None,
    };
    let no_color = std::env::var_os("NO_COLOR")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let enabled = match force {
        Some(true) => true,
        Some(false) => false,
        None => !no_color && is_tty_stdout(),
    };
    if enabled {
        ON
    } else {
        OFF
    }
}

fn is_tty_stdout() -> bool {
    unsafe { libc::isatty(1) == 1 }
}
