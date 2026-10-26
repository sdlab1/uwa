//! План оркестратора из папки `docs/` — файлы `.md` с заголовками.

use std::path::Path;

/// Файл плана.
#[derive(Debug, Clone, Default)]
pub struct PlanFile {
    /// Относительный путь (`docs/devh_advance2.md`).
    pub path: String,
    /// Размер в байтах.
    pub bytes: u64,
    /// Unix-время последнего изменения.
    pub modified_ts: u64,
    /// Первый заголовок markdown.
    pub heading: String,
}

/// Сводка по плану.
#[derive(Debug, Clone, Default)]
pub struct PlanPanel {
    /// Каталог с планом.
    pub dir: String,
    /// Файлы плана (новые первыми).
    pub files: Vec<PlanFile>,
}

/// Собрать список файлов плана.
pub fn collect(root: &Path) -> PlanPanel {
    let dir = root.join("docs");
    let mut panel = PlanPanel {
        dir: dir.display().to_string(),
        files: Vec::new(),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return panel;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let meta = entry.metadata().ok();
        let bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified_ts = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let heading = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| first_heading(&text))
            .unwrap_or_default();
        panel.files.push(PlanFile {
            path: format!("docs/{}", path.file_name().unwrap_or_default().to_string_lossy()),
            bytes,
            modified_ts,
            heading,
        });
    }
    panel
        .files
        .sort_by(|a, b| b.modified_ts.cmp(&a.modified_ts).then(a.path.cmp(&b.path)));
    panel
}

/// Первый заголовок markdown (`# …`).
pub fn first_heading(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Прочитать документ плана (с ограничением размера).
pub fn read_doc(root: &Path, rel: &str) -> Option<String> {
    let safe = rel.replace("..", "");
    let path = root.join(safe.trim_start_matches('/'));
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.chars().take(200_000).collect())
}

/// Минимальный markdown → HTML: заголовки, списки, код, абзацы.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut in_list = false;
    for line in md.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            if in_code {
                out.push_str("</code></pre>\n");
                in_code = false;
            } else {
                if in_list {
                    out.push_str("</ul>\n");
                    in_list = false;
                }
                out.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            out.push_str(&escape(trimmed));
            out.push('\n');
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes <= 6 && trimmed[hashes..].starts_with(' ') {
            if in_list {
                out.push_str("</ul>\n");
                in_list = false;
            }
            let level = hashes + 2;
            let text = trimmed[hashes + 1..].trim();
            out.push_str(&format!("<h{level}>{}</h{level}>\n", inline(text)));
            continue;
        }
        let bullet = trimmed.trim_start();
        if bullet.starts_with("- ") || bullet.starts_with("* ") {
            if !in_list {
                out.push_str("<ul>\n");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>\n", inline(&bullet[2..])));
            continue;
        }
        if in_list {
            out.push_str("</ul>\n");
            in_list = false;
        }
        if trimmed.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("<p>{}</p>\n", inline(trimmed)));
    }
    if in_code {
        out.push_str("</code></pre>\n");
    }
    if in_list {
        out.push_str("</ul>\n");
    }
    out
}

/// Экранирование HTML.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Инлайновая разметка: `code` и **bold**.
fn inline(text: &str) -> String {
    let escaped = escape(text);
    let mut out = String::new();
    let mut rest = escaped.as_str();
    while let Some(pos) = rest.find('`') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        match after.find('`') {
            Some(end) => {
                out.push_str(&format!("<code>{}</code>", &after[..end]));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[pos..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "<strong>").replace("**", "</strong>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_detected() {
        assert_eq!(first_heading("# Title\ntext").as_deref(), Some("Title"));
        assert!(first_heading("no heading").is_none());
    }

    #[test]
    fn markdown_rendered_safely() {
        let html = markdown_to_html("# H\n\n- item `code`\n\n<script>alert(1)</script>");
        assert!(html.contains("<h3>H</h3>"));
        assert!(html.contains("<li>item <code>code</code></li>"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
