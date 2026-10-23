use crate::core::gates::Gate;
use crate::core::types::{ItemId, RunConfig};
use crate::shell::git;
use crate::shell::store::{item_rel, Store};

pub fn shquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn format_agent_run(cfg: &RunConfig, prompt: &str) -> String {
    format!("{} {}", cfg.agent, shquote(prompt))
}

pub fn agent_prompt(
    store: &Store,
    cfg: &RunConfig,
    root: &std::path::Path,
    wt: &std::path::Path,
    id: &ItemId,
    agent_cmd: &str,
) -> String {
    let mut extra = String::new();
    if store.read(item_rel(id, "red.cmd")).is_some() {
        extra.push_str("Есть RED-пруф: поведение, которое сейчас падает, после твоих правок обязано проходить.\n");
    }
    // C1: контракт задачи целиком (поглотил P8-5/OBSERVE): цель, точки
    // наблюдения, заявленные файлы, ожидаемая подпись RED. Каждый раунд
    // перечитывает АРТЕФАКТ, а не историю — «контракт переживает fix-раунды».
    // Промпт остаётся ускорителем; вратами остаются гейты (advance2 §C1).
    if let Some(text) = store.read(item_rel(id, "contract.toml")) {
        if let Some(block) = crate::core::contract::Contract::parse(&text).block() {
            extra.push_str(&block);
        }
    }
    // Уведомления соседей: непрочитанные письма канала devh msg — агент
    // видит их в промпте (идентификация автоматическая). Письма не съедены:
    // прочитать и подтвердить — `devh msg read`.
    let agent_name = crate::core::identity::current().name;
    let brief = crate::shell::msg::unread_brief(&agent_name);
    if !brief.is_empty() {
        extra.push_str(&brief);
        extra.push('\n');
    }
    if let Some(rules) = &cfg.rules {
        extra.push_str(&format!(
            "Справочник проекта: {} — прочитай целиком. Это база известных граблей и уроков (технические знания: как здесь пишется код). Процессные правила уже вшиты в гейты харнесса. В тексте работы и в summary НЕ упоминай этот файл и todo/.\n",
            root.join(rules).display()
        ));
    }
    if cfg.mutate.is_some() {
        let min_kill = cfg.mutate.as_ref().map(|m| m.min_kill).unwrap_or(90);
        extra.push_str(&format!(
            "Тесты будут проверены мутационным гейтом: они обязаны УБИВАТЬ мутантов (kill-rate ≥ {min_kill}%). Пиши точные ассерты на реальное поведение; толерантные/тривиальные тесты гейт не пропустит.\n"
        ));
    }
    // P8-3: пул исторически слабых зон из .harness/killmap.json (store'а
    // главного репо, не worktree) — прицел агенту. На вердикт гейта и
    // MIN_KILL не влияет (§8.5): killmap не карает.
    if let Some(block) = crate::core::killmap::Killmap::load(&store.dir).hotspot_block() {
        extra.push_str(block.as_str());
    }
    extra.push_str("ON-FLY ANALYSIS: каждый файл, который открываешь/читаешь/правишь, анализируй ПОПУТНО — ни одно чтение в пустую: неоптимальность, избыточность, мёртвый код, дыра в тестах, подозрительный контракт → это ТИКЕТ (НЕ чинить — вне спеки!). Дописывай строкой в {hdir}/observations.md: <файл>:<строка> | <подозрение> | red-тест: <идея>. Тикет без red-теста не считается.\n");
    extra.push_str(&format!(
        "## Контракт\n\
- работа сделана только в worktree, git не трогаешь;\n\
- verify ({}) проходит;\n\
- summary написан, первая строка = сообщение коммита.\n",
        cfg.verify
    ));
    extra = extra.replace("{hdir}", &store.dir.display().to_string());

    let ctx = git::log_oneline(root, 3);
    let ctx: String = ctx.lines().map(|l| format!("  {l}\n")).collect();

    let spec = store.item_dir(id).join("spec.md");
    let plan = store.item_dir(id).join("plan.md");
    let sum = store.summary_file(id);

    format!(
        "Ты работаешь в изолированном git-worktree: {wt}. Все правки — только здесь.\n\n\
ЗАДАЧА: реализуй спеку {spec} (прочитай её полностью).\n\
Заметки прошлых попыток (если есть): {plan} — если не закончишь, допиши туда кратко: что сделано / что дальше.\n\n\
Контекст репо (HEAD на момент старта задачи):\n{ctx}\n\
ЖЕЛЕЗНЫЕ ПРАВИЛА:\n\
1. НИКОГДА не запускай git (add/commit/push/checkout/stash...) — харнесс владеет git.\n\
2. Реализуй спеку целиком: код + тесты, по стандартам проекта.\n\
3. Перед завершением прогони проверку сам: `{verify}` (из корня worktree). Она обязана проходить.\n\
{extra}\
4. В конце напиши summary по-русски в файл {sum}:\n\
   первая строка — готовое описание коммита одним предложением (про изменения в проекте, без упоминания харнесса/спеки),\n\
   дальше — детали и что проверено.\n\
   Если застрял: первая строка 'BLOCKED: <причина>'.\n\n\
Окружение: HARNESS_WORKTREE, HARNESS_SPEC, HARNESS_PLAN, HARNESS_SUMMARY — абсолютные пути.\n\
Следуй инструкции строго: агент — {agent_cmd}\n",
        wt = wt.display(),
        spec = spec.display(),
        plan = plan.display(),
        ctx = ctx,
        verify = cfg.verify,
        extra = extra,
        sum = sum.display(),
        agent_cmd = agent_cmd,
    )
}

pub fn red_author_prompt(
    store: &Store,
    cfg: &RunConfig,
    root: &std::path::Path,
    wt: &std::path::Path,
    id: &ItemId,
    agent_cmd: &str,
) -> String {
    let red = store.item_dir(id).join("red.cmd");
    let spec = store.item_dir(id).join("spec.md");

    let ctx = git::log_oneline(root, 3);
    let ctx: String = ctx.lines().map(|l| format!("  {l}\n")).collect();

    format!(
        "Ты работаешь в изолированном git-worktree: {wt}. Это ФАЗА RED: твоя ПЕРВАЯ задача — написать RED-ТЕСТ. Саму спеку НЕ реализуй.\n\n\
         ЗАДАЧА: создай файл {red} — ГОТОВЫЙ К ЗАПУСКУ ТЕСТ, который СЕЙЧАС падает на чистом HEAD и зеленеет после реализации спеки {spec}.\n\n\
         Что здесь считается тестом:\n\
         - прогон настоящего тестового входа проекта: той же команды/раннера, что у тебя в VERIFY ({verify}), только под нужный тест или сюит;\n\
         - запускается как `bash <этот файл>` из корня worktree: exit 0 = тест прошёл, exit ≠ 0 = тест упал;\n\
         - на чистом HEAD тест ОБЯЗАН упасть (exit ≠ 0), иначе гейт RED ответит «уже зелёный»;\n\
         - проверяет поведение из спеки и позеленеет ТОЛЬКО после её реализации.\n\n\
         ЗАПРЕЩЕНО вместо теста: grep/rg по файлам, test -f/-e и прочие проверки существования, «найди строку в файле», одиночные вызовы без тестового раннера. Это не тест — гейт RED такому не обязан верить; настоящий RED — упавший тест.\n\n\
         red.expect (P7): если знаешь, ПОЧЕМУ тест обязан упасть (какую ошибку он покажет) — первой строкой red.cmd поставь маркер `# red-expect: <regex/text ошибки>`. Харнес проверит, что тест упал ИМЕННО по этой причине (и что упал именно тест): упал по другой — вернётся БЛОКЕР СПЕКИ (чинишь ты эту спеку), а не фикс-раунд.\n\n\
         Правила фазы:\n\
         - в worktree ничего не пиши: исходники и git не трогай — можно только читать код, чтобы написать осмысленный тест;\n\
         - файл ОДИН: {red} (лежит вне worktree, в .harness);\n\
         - summary в этой фазе не нужен — его напишет фаза реализации.\n\n\
         Контекст репо (HEAD на момент старта задачи):\n{ctx}\n\
         Окружение: HARNESS_WORKTREE, HARNESS_SPEC, HARNESS_PLAN, HARNESS_SUMMARY — абсолютные пути.\n\
         Следуй инструкции строго: агент — {agent_cmd}\n",
        wt = wt.display(),
        red = red.display(),
        spec = spec.display(),
        verify = cfg.verify,
        ctx = ctx,
        agent_cmd = agent_cmd,
    )
}

pub fn review_prompt(diff: &str) -> String {
    format!(
        "Ты — независимый ревьюер, отдельный запрос. Ниже дифф сгенерированной работы. \
Review code for inconsistencies and bugs: логика, edge-cases, контракт спеки, качество тестов. \
Ответ — РОВНО одна строка: VERDICT: OK  — или —  VERDICT: ISSUES: <кратко, через ;>.\n\n{diff}"
    )
}

pub fn fix_prompt(gate: &Gate, tail: &str, cfg: &RunConfig, store: &Store, id: &ItemId) -> String {
    let sum = store.summary_file(id);
    let sum = sum.display();
    match gate {
        Gate::Verify => format!(
            "Verify не прошёл. Хвост лога:\n\n{tail}\n\n\
Почини по логу. Правила те же: git не трогать, после фикса сам прогони: `{verify}`. Обнови summary: {sum}",
            verify = cfg.verify
        ),
        Gate::Lint => format!(
            "Линтер нашёл проблемы в изменённых файлах. Хвост лога:\n\n{tail}\n\n\
Почини так, чтобы линтер был чист (стиль — по замечаниям линтера, не глуши его). git не трогай. Обнови summary: {sum}"
        ),
        Gate::Crap => {
            let max = cfg.crap.as_ref().map(|c| c.max).unwrap_or(12);
            format!(
                "CRAP-гейт не прошёл: сложность функций хуже порога (score = C²·(1−cov)³ + C, нарушение = score > {max}). Лог:\n\n{tail}\n\n\
Снизь сложность изменённых функций (разбей длинные цепочки условий, убери дублирование) или покрой их тестами (coverage ↑). Ослаблять/удалять проверки и «чинить» порог запрещено. git не трогай. Обнови summary: {sum}"
            )
        }
        Gate::Review => format!(
            "Независимый ревьюер нашёл замечания (второй запрос той же модели):\n\n{tail}\n\n\
Почини эти проблемы в рамках спеки, не расширяя скоуп. git не трогай. Обнови summary: {sum}"
        ),
        Gate::NoIgnored => format!(
            "Verify прошёл, но в выводе есть ignored-тесты (>0). Железное правило: ни одного ignored — ignored = скрытый красный. Хвост лога:\n\n{tail}\n\n\
Разбери причину игнора: почини тест и включи его; удаление проверки или новый ignore без причины запрещены. git не трогай. Обнови summary: {sum}"
        ),
        Gate::Mutate => {
            let min_kill = cfg.mutate.as_ref().map(|m| m.min_kill).unwrap_or(90);
            format!(
                "Mutation-гейт не прошёл: твои тесты НЕ УБИВАЮТ мутантов на изменённом коде (kill-rate < {min_kill}%). Лог:\n\n{tail}\n\n\
УСИЛЬ тесты изменённых модулей: точные ассерты на реальное поведение, граничные случаи, никаких ослаблений ассертов и удаления проверок. Ослабить тест, чтобы «позеленело», запрещено. git не трогай. Обнови summary: {sum}"
            )
        }
        Gate::Bench => format!(
            "Bench-гейт не прошёл (перф-регрессия против baseline). Лог:\n\n{tail}\n\n\
Разберись с производительностью изменённого кода (алгоритм, а не микромагия). git не трогай. Обнови summary: {sum}"
        ),
        // A6 FREEZE: агент вышел за заявленные файлы. Лечение — вернуть
        // лишнее в scope (или попросить человека записать обход allow-test-edit
        // для tests/**); ослаблять/удалять проверки здесь бессмысленно.
        Gate::Freeze => format!(
            "FREEZE-гейт не прошёл: правки вышли за заявленные в спеке файлы (FILES:/FREEZE_ALLOW). Лог:\n\n{tail}\n\n\
Убери лишние файлы из диффа (они не в scope сессии) или, если правка ОБОСНОВАННА, остановись и попроси человека: он запишет обход — items/{id}/allow-test-edit (только для tests/**) или дополнит FILES: в спеке. Сам ослаблять/удалять проверки и трогать tests/** без записанного обхода запрещено. git не трогай. Обнови summary: {sum}",
            id = id.dir_name()
        ),
        Gate::Tripwire => format!(
            "TRIPWIRE: агент изменил защищённый вход гейтов (конфиг/тесты/refs/HEAD). Правки откатаны. Лог:\n\n{tail}\n\nНе трогай защищённые файлы и git. Если изменение ОБОСНОВАННО, остановись и попроси человека записать обход: items/{id}/allow-protected (путь | причина). git не трогай. Обнови summary: {sum}",
            id = id.dir_name()
        ),
        Gate::Red { .. } => format!(
            "RED-гейт не прошёл. Хвост лога:\n\n{tail}\n\nПочини. git не трогай. Обнови summary: {sum}"
        ),
    }
}

pub fn commit_msg_from_summary(summary: &str, id: &ItemId, title: &str) -> String {
    let first = summary.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        let t: String = title.chars().take(60).collect();
        format!("{id}: {t}")
    } else {
        first.chars().take(72).collect()
    }
}

pub fn is_blocked(summary: &str) -> bool {
    summary
        .lines()
        .next()
        .map(|l| l.trim_start().starts_with("BLOCKED"))
        .unwrap_or(false)
}

pub fn msg_hygiene_violation(msg: &str, deny: &str) -> bool {
    match regex::Regex::new(&format!("(?i){deny}")) {
        Ok(re) => re.is_match(msg),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gates::RedPhase;
    use crate::core::types::MutateCfg;

    #[test]
    fn shquote_escapes_quotes() {
        assert_eq!(shquote("plain"), "'plain'");
        assert_eq!(shquote("it's"), r"'it'\''s'");
        assert_eq!(shquote(""), "''");
    }

    #[test]
    fn format_agent_run_composes_command() {
        let cfg = RunConfig::default();
        assert_eq!(
            format_agent_run(&cfg, "do it"),
            format!("{} 'do it'", cfg.agent)
        );
        let cfg = RunConfig {
            agent: "stub.sh".into(),
            ..Default::default()
        };
        assert_eq!(format_agent_run(&cfg, "x"), "stub.sh 'x'");
    }

    #[test]
    fn review_prompt_embeds_diff() {
        assert!(review_prompt("diff --git a/x").contains("diff --git a/x"));
        assert!(review_prompt("").contains("VERDICT: OK"));
    }

    #[test]
    fn fix_prompt_names_every_gate_and_tail() {
        let cfg = RunConfig::default();
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(dir.path());
        let id = ItemId(7);
        let cases = [
            (Gate::Verify, "Verify не прошёл"),
            (Gate::NoIgnored, "ignored"),
            (Gate::Lint, "Линтер нашёл"),
            (Gate::Crap, "CRAP-гейт"),
            (Gate::Review, "Независимый ревьюер нашёл"),
            (Gate::Mutate, "Mutation-гейт"),
            (Gate::Bench, "Bench-гейт"),
            (Gate::Freeze, "FREEZE-гейт не прошёл"),
            (Gate::Tripwire, "TRIPWIRE"),
            (
                Gate::Red {
                    cmd: "true".into(),
                    phase: RedPhase::Pre,
                    expect: None,
                },
                "RED-гейт не прошёл",
            ),
        ];
        for (gate, needle) in cases {
            let p = fix_prompt(&gate, "TAIL-МЯРКА", &cfg, &store, &id);
            assert!(p.contains(needle), "{gate:?}: {p}");
            assert!(p.contains("TAIL-МЯРКА"), "{gate:?}: {p}");
            assert!(p.contains("W-007"), "{gate:?}: {p}");
        }
    }

    #[test]
    fn fix_prompt_mutate_carries_min_kill() {
        let cfg = RunConfig {
            mutate: Some(MutateCfg {
                tool: "muex".into(),
                scope: ".".into(),
                min_kill: 80,
                early: false,
                first_problem: false,
                cov: false,
                survivors: false,
            }),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(dir.path());
        let p = fix_prompt(&Gate::Mutate, "t", &cfg, &store, &ItemId(1));
        assert!(p.contains("80%"), "{p}");
    }

    #[test]
    fn commit_msg_prefers_summary_and_truncates() {
        let id = ItemId(3);
        assert_eq!(
            commit_msg_from_summary("fix: то-то\nдетали", &id, "титул"),
            "fix: то-то"
        );
        let long = "x".repeat(100);
        assert_eq!(commit_msg_from_summary(&long, &id, "титул").len(), 72);
        assert_eq!(commit_msg_from_summary("", &id, "титул"), "W-003: титул");
        let long_title = "я".repeat(100);
        assert_eq!(
            commit_msg_from_summary("  \n", &id, &long_title),
            format!("W-003: {}", "я".repeat(60))
        );
    }

    #[test]
    fn blocked_detection() {
        assert!(is_blocked("BLOCKED: нет доступа\nдетали"));
        assert!(is_blocked("  BLOCKED: пробел слева"));
        assert!(!is_blocked("fix: блокированно-выглядящее"));
        assert!(!is_blocked(""));
    }

    #[test]
    fn hygiene_deny() {
        assert!(msg_hygiene_violation("todo/файл поправить", "todo/"));
        assert!(msg_hygiene_violation("Session 12 done", "Session [0-9]+"));
        assert!(msg_hygiene_violation("см. TODO/report.md", "todo/"));
        assert!(!msg_hygiene_violation("feat: чисто", "todo/"));
        assert!(!msg_hygiene_violation("любое", "(["));
    }
}
