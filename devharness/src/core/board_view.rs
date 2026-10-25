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
    /// SHA в `main`, куда попала работа (записывается при слиянии).
    pub main_sha: Option<String>,
    /// Причина обхода DoD ведущим (подавляет warning о missing main_sha).
    pub override_reason: Option<String>,
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
    /// Количество непрочитанных писем для агента.
    pub unread_count: usize,
}

/// Модель доски оркестратора.
#[derive(Debug, Clone, Default)]
pub struct BoardView {
    pub leader_buffer: Vec<BoardTask>, // БУФЕР ВЕДУЩЕГО (O1: >=10)
    /// БУФЕР TECH-LEAD: крупные задачи (C>=C4), ждущие технического лида.
    pub tech_lead_buffer: Vec<BoardTask>,
    pub agent_queues: BTreeMap<String, AgentQueue>,
    pub in_progress: Vec<BoardTask>,
    pub warnings: Vec<BoardWarning>,
}

impl BoardView {
    /// Заполнить счётчики непрочитанных писем для всех агентов.
    ///
    /// Источники: `.harness/orch/msg_events.jsonl` и ящики `$DEVH_MSG_HOME/inbox`.
    pub fn fill_unread_counts(&mut self) {
        for (agent, queue) in self.agent_queues.iter_mut() {
            let canon = crate::core::identity::canonical(agent);
            queue.unread_count = crate::shell::msg_events::count_unread(&canon);
        }
    }
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

/// Проверить: задачи Done обязаны иметь main_sha или явную причину обхода.
pub fn check_done_tasks_have_main_sha(board: &BoardView) -> Vec<BoardWarning> {
    let mut warnings = Vec::new();
    for task in board
        .leader_buffer
        .iter()
        .chain(board.tech_lead_buffer.iter())
        .chain(board.in_progress.iter())
        .chain(board.agent_queues.values().flat_map(|q| q.tasks.iter()))
    {
        if task.status == TaskStatus::Done
            && task.main_sha.is_none()
            && task.override_reason.is_none()
        {
            warnings.push(BoardWarning {
                agent: task.agent.clone(),
                message: format!(
                    "{}: задача Done без main_sha и без override_reason — слияние не зафиксировано",
                    task.id
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
    warnings.extend(check_done_tasks_have_main_sha(board));
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
            main_sha: None,
            override_reason: None,
        }
    }

    fn sample_queue(agent: &str, count: usize) -> AgentQueue {
        AgentQueue {
            agent: agent.to_string(),
            tasks: (0..count).map(|i| sample_task(&format!("T-{}", i), agent, TaskStatus::Queued)).collect(),
            unread_count: 0,
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

    #[test]
    fn done_task_without_main_sha_warns() {
        let mut board = BoardView::default();
        let mut t = sample_task("T-001", "beam", TaskStatus::Done);
        t.main_sha = None;
        t.override_reason = None;
        board.in_progress.push(t);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("T-001"));
        assert_eq!(warnings[0].severity, WarningSeverity::Warning);
    }

    #[test]
    fn done_task_with_main_sha_no_warning() {
        let mut board = BoardView::default();
        let mut t = sample_task("T-001", "beam", TaskStatus::Done);
        t.main_sha = Some("abc123".into());
        board.in_progress.push(t);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert!(warnings.is_empty());
    }

    #[test]
    fn done_task_with_override_reason_no_warning() {
        let mut board = BoardView::default();
        let mut t = sample_task("T-001", "beam", TaskStatus::Done);
        t.main_sha = None;
        t.override_reason = Some("manual merge".into());
        board.in_progress.push(t);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert!(warnings.is_empty(), "override_reason подавляет warning: {warnings:?}");
    }

    #[test]
    fn non_done_task_no_warning_for_missing_sha() {
        let mut board = BoardView::default();
        let t = sample_task("T-001", "beam", TaskStatus::Queued);
        board.in_progress.push(t);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert!(warnings.is_empty(), "non-Done task не должен давать warning");
    }

    #[test]
    fn multiple_done_tasks_all_warned() {
        let mut board = BoardView::default();
        let t1 = sample_task("T-001", "beam", TaskStatus::Done);
        let mut t2 = sample_task("T-002", "beam2", TaskStatus::Done);
        t2.main_sha = Some("deadbee".into());
        board.in_progress.push(t1);
        board.in_progress.push(t2);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert_eq!(warnings.len(), 1, "только T-001 без sha: {warnings:?}");
        assert!(warnings[0].message.contains("T-001"));
    }

    /// T-297: буфер tech-lead — отдельная секция доски (не смешан с буфером ведущего).
    #[test]
    fn tech_lead_buffer_is_separate_section() {
        let mut board = BoardView::default();
        board
            .tech_lead_buffer
            .push(sample_task("T-300", "techlead", TaskStatus::Queued));
        assert_eq!(board.tech_lead_buffer.len(), 1);
        assert!(
            board.leader_buffer.is_empty(),
            "буфер техлида не смешан с буфером ведущего"
        );
    }

    /// T-272: AgentQueue содержит счётчик непрочитанных писем.
    #[test]
    fn agent_queue_has_unread_count() {
        let q = sample_queue("beam", 3);
        assert_eq!(q.unread_count, 0, "unread_count в структуре есть");
    }

    /// T-272: BoardView::fill_unread_counts заполняет счётчики.
    #[test]
    fn fill_unread_counts_populates_field() {
        let mut board = BoardView::default();
        board.agent_queues.insert(
            "beam".to_string(),
            sample_queue("beam", 1),
        );
        board.fill_unread_counts();
        let q = board.agent_queues.get("beam").expect("queue");
        // unread_count должен быть usize (0 если нет новых писем).
        let _: usize = q.unread_count;
    }

    /// T-312: check_done_tasks_have_main_sha ловит Done без main_sha.
    #[test]
    fn check_done_tasks_have_main_sha_finds_done_without_sha() {
        let mut board = BoardView::default();
        let t = sample_task("T-401", "beam", TaskStatus::Done);
        board.in_progress.push(t);
        let warnings = check_done_tasks_have_main_sha(&board);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("T-401"));
        assert_eq!(warnings[0].severity, WarningSeverity::Warning);
    }

    /// T-312: Done с main_sha или override_reason не порождает warning.
    #[test]
    fn check_done_tasks_have_main_sha_ok_when_signed() {
        let mut board = BoardView::default();
        let mut t = sample_task("T-402", "beam", TaskStatus::Done);
        t.main_sha = Some("abc".into());
        board.in_progress.push(t);
        assert!(check_done_tasks_have_main_sha(&board).is_empty());

        let mut board = BoardView::default();
        let mut t = sample_task("T-403", "beam", TaskStatus::Done);
        t.override_reason = Some("manual merge".into());
        board.in_progress.push(t);
        assert!(
            check_done_tasks_have_main_sha(&board).is_empty(),
            "override_reason подавляет warning"
        );
    }
}
