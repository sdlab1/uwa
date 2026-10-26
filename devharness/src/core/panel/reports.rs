//! Отчёты lead и tech_lead: письма из `$DEVH_MSG_HOME/outbox`.
//!
//! Отчёт агента — это письмо ведущему; для панели важны отчёты РОЛЕЙ
//! (`lead`, `tech_lead`), поэтому читается их исходящий архив.

use crate::commands::orch::Registry;

/// Письмо-отчёт.
#[derive(Debug, Clone, Default)]
pub struct ReportPanel {
    /// Автор (канонический адрес).
    pub author: String,
    /// Роль автора.
    pub role: String,
    /// Время письма (строка как в письме).
    pub stamp: String,
    /// Тема.
    pub subject: String,
    /// Тред.
    pub thread: String,
    /// Тело письма (обрезано).
    pub body: String,
    /// Это отчёт техлида.
    pub is_tech_lead: bool,
    /// Отчёт по канону (в теле есть строка «P/F:»).
    pub is_report: bool,
}

/// Максимальная длина тела отчёта в панели.
const BODY_LIMIT: usize = 2_000;

/// Собрать отчёты lead/tech_lead (свежие первыми).
pub fn collect(reg: &Registry, limit: usize) -> Vec<ReportPanel> {
    let mut authors: Vec<(String, String)> = Vec::new();
    if let Some(lead) = reg.lead.clone() {
        authors.push((lead, "lead".to_string()));
    }
    for a in &reg.agents {
        let role = a.role.clone().unwrap_or_default();
        if (role == "lead" || role == "tech_lead")
            && !authors.iter().any(|(n, _)| n == &a.name)
        {
            authors.push((a.name.clone(), role));
        }
    }
    let mut out: Vec<ReportPanel> = Vec::new();
    for (name, role) in authors {
        for block in sent_blocks(&name) {
            let view = crate::shell::msg::parse_view(&block);
            if view.subject.trim().is_empty() && view.body.trim().is_empty() {
                continue;
            }
            let body: String = view.body.chars().take(BODY_LIMIT).collect();
            out.push(ReportPanel {
                author: name.clone(),
                role: role.clone(),
                stamp: view.stamp.clone(),
                subject: view.subject.clone(),
                thread: view.thread.clone(),
                body,
                is_tech_lead: role == "tech_lead",
                is_report: view.body.contains("P/F:") || view.subject.contains("P/F:"),
            });
        }
    }
    out.reverse();
    out.truncate(limit);
    out
}

/// Разобрать исходящий архив агента на отдельные письма.
fn sent_blocks(agent: &str) -> Vec<String> {
    let path = crate::shell::msg::home()
        .join("outbox")
        .join(format!("{}.log", crate::shell::msg::safe_name(agent)));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    split_blocks(&text)
}

/// Разделить журнал писем на блоки (новое письмо начинается со строки `[ЧЧ:ММ] ОТ:`).
pub fn split_blocks(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        let is_header = line.starts_with('[') && line.contains(" ОТ:");
        if is_header && !cur.is_empty() {
            blocks.push(cur.join("\n"));
            cur.clear();
        }
        cur.push(line);
    }
    if !cur.is_empty() {
        blocks.push(cur.join("\n"));
    }
    blocks
        .into_iter()
        .filter(|b| !b.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_split_on_headers() {
        let text = "[10:00] ОТ: minimax — КОМУ: beam\nТЕМА: A\nТЕЛО:\nтело1\n\n[11:00] ОТ: minimax — КОМУ: beam\nТЕМА: B\nТЕЛО:\nтело2\n";
        let blocks = split_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("ТЕМА: A"));
        assert!(blocks[1].contains("ТЕМА: B"));
    }

    #[test]
    fn empty_journal_yields_no_blocks() {
        assert!(split_blocks("").is_empty());
    }
}
