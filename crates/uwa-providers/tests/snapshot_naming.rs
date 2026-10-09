//! Unit tests for the file-naming convention used by `uwa-snapshot`.

/// Main fixture file name for a provider.
fn main_path(provider: &str) -> String {
    format!("{provider}.html")
}

/// OOPIF/child-frame fixture file name for a provider at a given index.
fn frame_path(provider: &str, idx: usize) -> String {
    format!("{provider}.frame_{idx}.html")
}

#[test]
fn main_path_is_bare_provider_name() {
    assert_eq!(main_path("chatgpt"), "chatgpt.html");
    assert_eq!(main_path("deepseek"), "deepseek.html");
    assert_eq!(main_path("claude"), "claude.html");
}

#[test]
fn frame_paths_are_zero_indexed() {
    assert_eq!(frame_path("chatgpt", 0), "chatgpt.frame_0.html");
    assert_eq!(frame_path("claude", 3), "claude.frame_3.html");
}

#[test]
fn main_and_frame_do_not_collide() {
    assert_ne!(main_path("chatgpt"), frame_path("chatgpt", 0));
    assert_ne!(main_path("chatgpt"), frame_path("chatgpt", 1));
}

#[test]
fn frame_paths_sort_before_main_in_ascii() {
    // ASCII sort puts '.' (0x2E) before letters, so frame files sort first:
    // `chatgpt.frame_0.html` < `chatgpt.html`. The main fixture is
    // identified by exact name (no `.frame_N` suffix), not by sort order.
    let mut files = [
        main_path("chatgpt"),
        frame_path("chatgpt", 1),
        frame_path("chatgpt", 0),
    ];
    files.sort();
    assert_eq!(files[0], frame_path("chatgpt", 0));
    assert_eq!(files[1], frame_path("chatgpt", 1));
    assert_eq!(files[2], main_path("chatgpt"));
}

#[test]
fn main_path_has_no_frame_suffix() {
    assert!(!main_path("chatgpt").contains(".frame_"));
    assert!(!main_path("deepseek").contains(".frame_"));
}

#[test]
fn providers_do_not_cross_pollinate() {
    assert_ne!(frame_path("chatgpt", 0), frame_path("claude", 0));
    assert_ne!(main_path("chatgpt"), main_path("claude"));
}
