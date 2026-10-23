//! Разбор unified diff -> список изменённых файлов и строк.

use std::collections::BTreeSet;

/// Информация об изменённом файле.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangedFile {
    pub path: String,
}

/// Разобрать unified diff и вернуть множество изменённых файлов.
///
/// Пустой/битый дифф -> пустое множество (не «всё подряд»).
pub fn parse_diff(text: &str) -> BTreeSet<ChangedFile> {
    let mut files = BTreeSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            let path = rest.trim().to_string();
            if !path.is_empty() && path != "/dev/null" {
                files.insert(ChangedFile { path });
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest.trim().trim_start_matches("b/").to_string();
            if !path.is_empty() && path != "/dev/null" {
                files.insert(ChangedFile { path });
            }
        }
    }
    files
}

/// Разобрать unified diff и вернуть изменённые строки по файлам.
pub fn changed_lines(text: &str) -> Vec<(String, u32)> {
    let mut result = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            current_file = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            // @@ -old_start,old_count +new_start,new_count @@
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 2 {
                let old_part = parts[0].trim_start_matches('-');
                let new_part = parts[1].trim_start_matches('+');
                let old_start: u32 = old_part
                    .split(',')
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                let new_start: u32 = new_part
                    .split(',')
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                old_line = old_start;
                new_line = new_start;
            }
            continue;
        }
        if let Some(content) = line.strip_prefix('+') {
            if !content.starts_with('+') && current_file.is_some() {
                // Добавленная строка
                if let Some(ref f) = current_file {
                    result.push((f.clone(), new_line));
                }
            }
            new_line += 1;
        } else if let Some(_content) = line.strip_prefix('-') {
            if !_content.starts_with('-') {
                old_line += 1;
            }
        } else if line.starts_with(' ') {
            old_line += 1;
            new_line += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff() {
        let files = parse_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn broken_diff() {
        let files = parse_diff("not a diff");
        assert!(files.is_empty());
    }

    #[test]
    fn single_file() {
        let diff = "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@\n-old\n+new\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert!(files.contains(&ChangedFile {
            path: "foo.rs".to_string()
        }));
    }

    #[test]
    fn multiple_files() {
        let diff = "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn dev_null_ignored() {
        let diff = "--- a/foo.rs\n+++ /dev/null\n";
        let files = parse_diff(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn changed_lines_added() {
        let diff = "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,4 @@\n line1\n+added\n line2\n";
        let lines = changed_lines(diff);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "foo.rs");
        assert_eq!(lines[0].1, 2);
    }

    #[test]
    fn changed_lines_multiple() {
        let diff =
            "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,4 @@\n line1\n+added1\n+added2\n line2\n";
        let lines = changed_lines(diff);
        assert_eq!(lines.len(), 2);
    }
}
