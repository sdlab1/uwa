# Orchestrator Guards

Автоматические машинные проверки инвариантов оркестратора. Guard — это
маленькая stateless проверка, которая проходит (Ok), предупреждает (Warn)
или блокирует (Problem) действие.

## Архитектура

```
core/guards/
  mod.rs              — фреймворк (Guard trait, GuardVerdict, GuardReport, run_all)
  deps_guard.rs       — граф зависимостей задач (циклы, несуществующие ID)
  module_list_guard.rs — список модулей core/mod.rs (дубли, missing file, malformed)
  task_state_guard.rs  — конечный автомат статусов задачи
  worktree_guard.rs    — worktree agent: путь, ветка, чистота, не в /tmp
  zone_overlap_guard.rs — пересечение зон активных задач разных агентов
```

## Как это работает

1. `devh orch guard` вызывает `run_all` из `core/guards/mod.rs`.
2. Каждый Guard реализует trait `Guard { fn id(); fn run(ctx) -> GuardVerdict }`.
3. `GuardReport` собирает вердикты: `is_clean()` = true, если нет Problem.
4. Warn не блокирует, а выводит подсказку.
5. Problem → действие отклоняется, очередь замедляется.

## Каталог существующих guards

| guard | id | инвариант | verdict | файл |
| --- | --- | --- | --- | --- |
| deps | `deps` | граф задач: нет циклов, все `dep` существуют | Problem | `core/guards/deps_guard.rs` |
| module_list | `module_list` | `pub mod X;` в `core/mod.rs` ↔ файл `X.rs` существует, нет дублей | Problem | `core/guards/module_list_guard.rs` |
| task_state | `task_state` | статусы задачи: queued→active→done\|failed\|revoked; нет `done→active` | Problem | `core/guards/task_state_guard.rs` |
| worktree | `worktree` | worktree существует, не в `/tmp`, на своей ветке, чистый | Warn/Problem | `core/guards/worktree_guard.rs` |
| zone_overlap | `zone_overlap` | две активные задачи разных агентов не пересекаются по зонам | Problem | `core/guards/zone_overlap_guard.rs` |

## Шаблон нового guard

```rust
//! Doc: что проверяем и почему.
use super::{Guard, GuardContext, GuardVerdict};

pub struct MyGuard;

impl Guard for MyGuard {
    fn id(&self) -> &'static str { "my_guard" }

    fn run(&self, ctx: &GuardContext) -> GuardVerdict {
        // 1. Сбор данных (без побочных эффектов).
        // 2. Проверка инварианта.
        // 3. Возврат Ok | Warn(msg) | Problem(msg).
        GuardVerdict::Ok
    }
}
```

### Как подключить

1. Создать `devharness/src/core/guards/<name>.rs`.
2. Добавить `pub mod <name>;` в `devharness/src/core/guards/mod.rs`.
3. Зарегистрировать в реестре guards (в `orch.rs` или `guard_cmd`).
4. Добавить `#[test]` в модуль — guard проверяется юнит-тестом.
5. `devh orch guard --all` должен включать новый guard.

## Selfcheck

```bash
# Прогнать все guards:
devh orch guard

# Проверить конкретный guard:
devh test --case guard::module_list

# Добавить guard в selfcheck:
# в core/guards/mod.rs → в реестр в orch.rs
```
