//! Структурная модель доски оркестратора.

use std::collections::BTreeMap;

/// Предупреждение на доске.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardWarning {
    pub agent: String,
    pub message: String,
    pub severity: WarningSeverity,
}

/// Уровень серьезности предупреждения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WarningSeverity {
    Info,
    Warning,
    Critical,
}

/// Задача в представлении доски.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardTask {
    pub id: String,
    pub agent: String,
    pub priority: u8,
    pub complexity: u8,
    pub status: TaskStatus,
    pub zone: String,
}

/// Статус задачи на доске.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Active,
    Done,
    Failed,
    Revoked,
}

/// Очередь агента.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentQueue {
    pub agent: String,
    pub tasks: Vec<BoardTask>,
}

/// Модель доски оркестратора.
#[derive(Debug, Clone, Default)]
pub struct BoardView {
    pub leader_buffer: Vec<BoardTask>, // БУФЕР ВЕДУЩЕГО (O1: >=10)
    pub agent_queues: BTreeMap<String, AgentQueue>,
    pub in_progress: Vec<BoardTask>,
    pub warnings: Vec<BoardWarning>,
}

/// Проверить буфер ведущего (O1: >=10 задач).
pub fn check_lead_buffer(board: &BoardView) -> Vec<BoardWarning> {
    let mut warnings = Vec::new();
    if board.leader_buffer.len() < 10 {
        warnings.push(BoardWarning {
            agent: "lead".to_string(),
            message: format!(
                "БУФЕР ВЕДУЩЕГО: всего {} задач, минимум 10 (O1)",
                board.leader_buffer.len()
            ),
            severity: WarningSeverity::Critical,
        });
    }
    warnings
}

/// Проверить очереди ведомых (O1: >=2 задачи у каждого).
pub fn check_agent_queues(board: &BoardView) -> Vec<BoardWarning> {
    let mut warnings = Vec::new();
    for (agent, queue) in &board.agent_queues {
        if queue.tasks.len() < 2 {
            warnings.push(BoardWarning {
                agent: agent.clone(),
                message: format!(
                    "ОЧЕРЕДЬ {}: всего {} задач, минимум 2 (O1)",
                    agent,
                    queue.tasks.len()
                ),
                severity: WarningSeverity::Warning,
            });
        }
    }
    warnings
}

/// Собрать все предупреждения доски.
pub fn collect_warnings(board: &BoardView) -> Vec<BoardWarning> {
    let mut warnings = Vec::new();
    warnings.extend(check_lead_buffer(board));
    warnings.extend(check_agent_queues(board));
    warnings.extend(board.warnings.clone());
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(id: &str, agent: &str, status: TaskStatus) -> BoardTask {
        BoardTask {
            id: id.to_string(),
            agent: agent.to_string(),
            priority: 2,
            complexity: 0,
            status,
            zone: "core/test".to_string(),
        }
    }

    #[test]
    fn lead_buffer_ok() {
        let mut board = BoardView::default();
        for i in 0..10 {
            board.leader_buffer.push(sample_task(&format!("T-{}", i), "lead", TaskStatus::Queued));
        }
        let warnings = check_lead_buffer(&board);
        assert!(warnings.is_empty());
    }

    #[test]
    fn lead_buffer_warning() {
        let mut board = BoardView::default();
        for i in 0..5 {
            board.leader_buffer.push(sample_task(&format!("T-{}", i), "lead", TaskStatus::Queued));
        }
        let warnings = check_lead_buffer(&board);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("5 задач"));
    }

    #[test]
    fn agent_queue_ok() {
        let mut board = BoardView::default();
        let mut queue = AgentQueue::default();
        queue.agent = "beam2".to_string();
        queue.tasks.push(sample_task("T-001", "beam2", TaskStatus::Queued));
        queue.tasks.push(sample_task("T-002", "beam2", TaskStatus::Queued));
        board.agent_queues.insert("beam2".to_string(), queue);
        let warnings = check_agent_queues(&board);
        assert!(warnings.is_empty());
    }

    #[test]
    fn agent_queue_warning() {
        let mut board = BoardView::default();
        let mut queue = AgentQueue::default();
        queue.agent = "beam2".to_string();
        queue.tasks.push(sample_task("T-001", "beam2", TaskStatus::Queued));
        board.agent_queues.insert("beam2".to_string(), queue);
        let warnings = check_agent_queues(&board);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("1 задач"));
    }

    #[test]
    fn collect_all_warnings() {
        let mut board = BoardView::default();
        // lead buffer empty -> critical (1 warning)
        // no agent queues -> no additional warning (only existing queues checked)
        let warnings = collect_warnings(&board);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn warning_severity_order() {
        assert!(WarningSeverity::Critical > WarningSeverity::Warning);
        assert!(WarningSeverity::Warning > WarningSeverity::Info);
    }
}
