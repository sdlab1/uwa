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
        extra += "Есть RED-пруф: поведение, которое сейчас падает, после твоих правок обязано проходить.\n";
    }
    if let Some(rules) = &cfg.rules {
        extra += &format!(
            "Справочник проекта: {} — прочитай целиком. Это база известных граблей и уроков (технические знания: как здесь пишется код). Процессные правила уже вшиты в гейты харнесса. В тексте работы и в summary НЕ упоминай этот файл и todo/.\n",
            root.join(rules).display()
        );
    }
    if cfg.mutate.is_some() {
        let min_kill = cfg.mutate.as_ref().map(|m| m.min_kill).unwrap_or(90);
        extra += &format!(
            "Тесты будут проверены мутационным гейтом: они обязаны УБИВАТЬ мутантов (kill-rate ≥ {min_kill}%). Пиши точные ассерты на реальное поведение; толерантные/тривиальные тесты гейт не пропустит.\n"
        );
    }
    extra += "ON-FLY ANALYSIS: каждый файл, который открываешь/читаешь/правишь, анализируй ПОПУТНО — ни одно чтение в пустую: неоптимальность, избыточность, мёртвый код, дыра в тестах, подозрительный контракт → это ТИКЕТ (НЕ чинить — вне спеки!). Дописывай строкой в {hdir}/observations.md: <файл>:<строка> | <подозрение> | red-тест: <идея>. Тикет без red-теста не считается.\n";
    extra += &format!(
        "## Контракт\n\
- работа сделана только в worktree, git не трогаешь;\n\
- verify ({}) проходит;\n\
- summary написан, первая строка = сообщение коммита.\n",
        cfg.verify
    );
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
