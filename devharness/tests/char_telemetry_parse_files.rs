//! T-405: char_telemetry_parse_files — вывод cargo check разбирается по файлам (человеческий и JSON)

use devh::core::check_telemetry::CheckStats;

#[test]
fn parses_human_readable_multiple_files() {
    let text = "\
Checking devh v0.1.0 (/tmp/x)
warning: unused variable: `x`
  --> src/lib.rs:3:9
error[E0308]: mismatched types
  --> src/main.rs:10:5
warning: unused import: `std::fmt`
  --> src/utils.rs:1:1
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s
";
    let s = CheckStats::parse(text);
    assert_eq!(s.errors, 1, "одна ошибка");
    assert_eq!(s.warnings, 2, "два предупреждения");
    assert!(!s.build_ok, "ошибка есть — сборка не успешна");
    assert_eq!(s.runs, 1);
}

#[test]
fn parses_json_multiple_files() {
    let text = "\
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"mismatched types\",\"spans\":[{\"file_name\":\"src/main.rs\",\"byte_start\":10,\"byte_end\":15,\"line_start\":10,\"line_end\":10,\"column_start\":5,\"column_end\":10}]}}
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"unused variable\",\"spans\":[{\"file_name\":\"src/lib.rs\",\"byte_start\":3,\"byte_end\":9,\"line_start\":3,\"line_end\":3,\"column_start\":9,\"column_end\":9}]}}
{\"reason\":\"build-finished\",\"success\":false}
";
    let s = CheckStats::parse(text);
    assert_eq!(s.errors, 1);
    assert_eq!(s.warnings, 1);
    assert!(!s.build_ok);
    assert_eq!(s.runs, 1);
}

#[test]
fn from_output_merges_stdout_stderr_by_file() {
    // Simulated stdout with error in main.rs
    let stdout = "\
error[E0308]: mismatched types
  --> src/main.rs:10:5
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s
";
    // Simulated stderr with warning in lib.rs
    let stderr = "\
warning: unused import: `std::fmt`
  --> src/lib.rs:1:1
";
    let s = CheckStats::from_output(stdout, stderr);
    assert_eq!(s.errors, 1);
    assert_eq!(s.warnings, 1);
    assert_eq!(s.runs, 1);
    assert!(!s.build_ok);
}

#[test]
fn multiple_files_in_json_are_parsed() {
    let text = "\
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"E0308\",\"spans\":[{\"file_name\":\"src/a.rs\",\"line_start\":1,\"line_end\":1}]}}
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"unused\",\"spans\":[{\"file_name\":\"src/b.rs\",\"line_start\":2,\"line_end\":2}]}}
{\"reason\":\"build-finished\",\"success\":false}
";
    let s = CheckStats::parse(text);
    assert_eq!(s.errors, 1);
    assert_eq!(s.warnings, 1);
    assert!(!s.build_ok);
    assert_eq!(s.runs, 1);
}