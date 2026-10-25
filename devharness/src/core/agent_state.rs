//! Чистая модель состояния исполнителя.
//!
//! Инкапсулирует состояние агента (роль, модель, подозрение, fail_streak,
//! pending_welcome) и переходы, правила которых живут в каноне
//! (`devh workflow` §ОРКЕСТРАЦИЯ O11–O13):
//!
//! - `record_fail`   — запись результта задачи: fail_streak + триггер suspicion (O12)
//! - `degrade_p`     — понижение P-уровня очереди на 1 (не ниже P0) (O13)
//! - `on_model_change` — смена модели/рантайма: анонимная → suspicion; --confirm → снять (O13)
//! - `reset_welcome`  — сброс после простоя: pending_welcome + suspicion (R23/watchdog)
//! - `welcome_done`   — завершение welcome: очистка pending_welcome и suspicion

/// Роль агента в оркестрации (R22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentRole {
    /// Ведущий: управляет очередью, worktree'ами, мутационным аудитом.
    #[default]
    Lead,
    /// Ведомый: исполняет задачи в своём worktree.
    Worker,
    /// Технический лид: ревью коммитов, уточнение моделей агентов и
    /// управление их потолком сложности (C). Без прав ведущего:
    /// без merge/push/mutate/passwd.
    TechLead,
}

impl AgentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRole::Lead => "lead",
            AgentRole::Worker => "worker",
            AgentRole::TechLead => "tech_lead",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lead" => Some(AgentRole::Lead),
            "worker" => Some(AgentRole::Worker),
            "tech_lead" | "tech-lead" | "techlead" => Some(AgentRole::TechLead),
            _ => None,
        }
    }
}

/// Состояние исполнителя: чистая модель без IO и без serde.
///
/// `Default` обязателен: cargo-mutants подставляет `Default::default()`
/// вместо тела функции (P9: unviable = дыра).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentState {
    /// Канонический адрес агента.
    pub name: String,
    /// Роль: lead / worker.
    pub role: AgentRole,
    /// Модель (фиксируется ведущим; в адрес не входит).
    pub model: Option<String>,
    /// Последний фактический уровень отказа (0=F0 … 5=F5).
    pub last_fail: Option<u8>,
    /// Сколько раз подряд F>=F2.
    pub fail_streak: u32,
    /// Причина подозрения (F3+, два F>=F2 подряд, смена модели, простой).
    pub suspicion: Option<String>,
    /// Ждёт повторного `orch welcome` после простоя (R23).
    pub pending_welcome: bool,
}

impl AgentState {
    /// Создать состояние для нового агента.
    pub fn new(name: impl Into<String>, role: AgentRole) -> Self {
        Self {
            name: name.into(),
            role,
            model: None,
            last_fail: None,
            fail_streak: 0,
            suspicion: None,
            pending_welcome: false,
        }
    }

    /// Записать результат задачи и применить правила деградации (O12).
    ///
    /// Правила:
    /// - F>=F2 → fail_streak += 1; иначе fail_streak = 0
    /// - F>=F3 → suspicion ("F{f} в задаче {id}")
    /// - fail_streak >= 2 → suspicion ("два подряд F>=F2 ...")
    ///
    /// Возвращает причину подозрения, если триггер сработал.
    pub fn record_fail(&mut self, fail_level: u8, task_id: &str) -> Option<String> {
        self.last_fail = Some(fail_level);
        self.fail_streak = if fail_level >= 2 {
            self.fail_streak.saturating_add(1)
        } else {
            0
        };

        if fail_level >= 3 {
            let reason = format!("F{} в задаче {}", fail_level, task_id);
            self.suspicion = Some(reason.clone());
            Some(reason)
        } else if self.fail_streak >= 2 {
            let reason = format!("два подряд F>=F2 (последняя задача {})", task_id);
            self.suspicion = Some(reason.clone());
            Some(reason)
        } else {
            None
        }
    }

    /// Сменить модель/рантайм (O13).
    ///
    /// `confirmed` = false (без `--confirm`): подозрение + деградация очереди.
    /// `confirmed` = true (ведущий подтвердил): снять подозрение, очередь нет.
    ///
    /// Возвращает причину подозрения, если смена анонимная.
    pub fn on_model_change(&mut self, new_model: &str, confirmed: bool) -> Option<String> {
        let old_model = self.model.clone();
        self.model = Some(new_model.to_string());

        if confirmed {
            self.suspicion = None;
            None
        } else {
            let reason = if let Some(old) = &old_model {
                format!("смена модели/рантайма ({} → {})", old, new_model)
            } else {
                format!("смена модели/рантайма (неизвестно → {})", new_model)
            };
            self.suspicion = Some(reason.clone());
            Some(reason)
        }
    }

    /// Снять подозрение (например, после `--confirm`).
    pub fn clear_suspicion(&mut self) {
        self.suspicion = None;
    }

    /// Сброс после простоя (R23/watchdog): pending_welcome + suspicion.
    pub fn reset_welcome(&mut self, reason: &str) {
        self.pending_welcome = true;
        self.suspicion = Some(reason.to_string());
    }

    /// Выполнить `orch welcome` — очистить флаг повторного посвящения.
    pub fn welcome_done(&mut self) {
        self.pending_welcome = false;
        self.suspicion = None;
    }

    /// Активное подозрение?
    pub fn suspicious(&self) -> bool {
        self.suspicion.is_some()
    }

    /// Ожидает повторного welcome?
    pub fn needs_welcome(&self) -> bool {
        self.pending_welcome
    }
}

/// Понизить P-уровень задачи на 1 (не ниже P0 / floor) (O13).
///
/// `floor` — верхняя граница (не опустить ниже); `None` = просто -1.
pub fn degrade_p(current: u8, floor: Option<u8>) -> u8 {
    let target = match floor {
        Some(f) => current.min(f),
        None => current.saturating_sub(1),
    };
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str) -> AgentState {
        AgentState::new(name, AgentRole::Worker)
    }

    #[test]
    fn role_as_str_and_parse_roundtrip() {
        assert_eq!(AgentRole::Lead.as_str(), "lead");
        assert_eq!(AgentRole::Worker.as_str(), "worker");
        assert_eq!(AgentRole::parse("lead"), Some(AgentRole::Lead));
        assert_eq!(AgentRole::parse("worker"), Some(AgentRole::Worker));
        assert_eq!(AgentRole::parse("boss"), None);
        assert_eq!(AgentRole::TechLead.as_str(), "tech_lead");
        assert_eq!(AgentRole::parse("tech_lead"), Some(AgentRole::TechLead));
        assert_eq!(AgentRole::parse("tech-lead"), Some(AgentRole::TechLead));
        assert_eq!(AgentRole::parse(" LEAD "), Some(AgentRole::Lead));
    }

    #[test]
    fn record_fail_f0_no_suspicion() {
        let mut a = worker("beam");
        assert!(a.record_fail(0, "T-001").is_none());
        assert!(!a.suspicious());
        assert_eq!(a.fail_streak, 0);
        assert_eq!(a.last_fail, Some(0));
    }

    #[test]
    fn record_fail_f1_resets_streak() {
        let mut a = worker("beam");
        a.record_fail(2, "T-001");
        assert_eq!(a.fail_streak, 1);
        // F1 после F2: streak сбрасывается
        assert!(a.record_fail(1, "T-002").is_none());
        assert_eq!(a.fail_streak, 0);
        assert!(!a.suspicious(), "F1 не триггерит suspicion");
    }

    #[test]
    fn record_fail_f3_triggers_suspicion() {
        let mut a = worker("beam");
        let reason = a.record_fail(3, "T-005");
        assert!(reason.is_some(), "F3 должен триггерить suspicion");
        assert!(a.suspicious());
        assert_eq!(a.last_fail, Some(3));
        let r = reason.unwrap();
        assert!(r.contains("F3"));
        assert!(r.contains("T-005"));
    }

    #[test]
    fn record_fail_f2_once_no_suspicion() {
        let mut a = worker("beam");
        let reason = a.record_fail(2, "T-001");
        assert!(reason.is_none(), "одиночный F2 не триггерит suspicion");
        assert_eq!(a.fail_streak, 1);
        assert!(!a.suspicious());
    }

    #[test]
    fn record_fail_two_consecutive_f2_triggers_suspicion() {
        let mut a = worker("beam");
        // первая F2: streak=1, suspicion — нет
        a.record_fail(2, "T-001");
        assert!(!a.suspicious());
        // вторая подряд F2: streak=2 → suspicion
        let reason = a.record_fail(2, "T-002");
        assert!(reason.is_some());
        assert!(a.suspicious());
        assert_eq!(a.fail_streak, 2);
        assert!(reason.unwrap().contains("два подряд"));
    }

    #[test]
    fn model_change_unconfirmed_sets_suspicion() {
        let mut a = worker("beam");
        let reason = a.on_model_change("mimo", false);
        assert!(reason.is_some());
        assert!(a.suspicious());
        assert_eq!(a.model, Some("mimo".to_string()));
        assert!(reason.unwrap().contains("неизвестно"));
    }

    #[test]
    fn model_change_unconfirmed_after_first_is_suspicious() {
        let mut a = worker("beam");
        // первая фиксация — тоже смена (было None)
        a.on_model_change("mimo", false);
        // вторая смена без --confirm
        let reason = a.on_model_change("beam", false);
        assert!(reason.is_some());
        assert!(a.suspicious());
        assert!(reason.unwrap().contains("mimo → beam"));
    }

    #[test]
    fn model_change_confirmed_clears_suspicion() {
        let mut a = worker("beam");
        a.on_model_change("mimo", false);
        assert!(a.suspicious());
        // подтверждение ведущиком
        a.on_model_change("mimo", true);
        assert!(!a.suspicious());
        assert_eq!(a.model, Some("mimo".to_string()));
    }

    #[test]
    fn model_change_confirmed_no_suspicion() {
        let mut a = worker("beam");
        let reason = a.on_model_change("mimo", true);
        assert!(reason.is_none());
        assert!(!a.suspicious());
    }

    #[test]
    fn reset_welcome_sets_pending_and_suspicion() {
        let mut a = worker("beam");
        a.welcome_done(); // начальное состояние
        a.reset_welcome("простой 20 мин — при возврате нужен повторный welcome");
        assert!(a.pending_welcome);
        assert!(a.suspicious());
    }

    #[test]
    fn welcome_done_clears_flags() {
        let mut a = worker("beam");
        a.reset_welcome("простой");
        assert!(a.pending_welcome);
        assert!(a.suspicious());
        a.welcome_done();
        assert!(!a.pending_welcome);
        assert!(!a.suspicious());
    }

    #[test]
    fn degrade_p_decreases_by_one() {
        assert_eq!(degrade_p(3, None), 2);
        assert_eq!(degrade_p(1, None), 0);
    }

    #[test]
    fn degrade_p_respects_floor() {
        assert_eq!(degrade_p(3, Some(2)), 2);
        assert_eq!(degrade_p(5, Some(4)), 4);
        // floor выше current — не меняет
        assert_eq!(degrade_p(2, Some(4)), 2);
    }

    #[test]
    fn degrade_p_never_below_zero() {
        assert_eq!(degrade_p(0, None), 0);
        assert_eq!(degrade_p(1, Some(0)), 0);
    }
}
