# ⛔ MUTATION WORKFLOW — ЖЕЛЕЗНЫЙ ПРАВИЛО

**ЛЮБАЯ работа с мутациями = ТОЛЬКО через watcher. РУКАМИ cargo-mutants НЕ ЗАПУСКАТЬ.**

```
/home/user/project_uwa/devharness/tools/mutants-watch.sh
```

Это не просьба. Это граница, которая проверяется механически (см. `devharness/shell/guard-mutation.sh`).

## Как запускать мутационный аудит

1. Запустить `cargo-mutants` **в фоне**, отвязанным от shell-tool (`setsid`), иначе
   tool-таймаут убивает процесс группой (exit 127) и прогон сбивается:

```bash
cd /home/user/project_uwa
rm -rf mutants.out
setsid bash -c 'cargo mutants -p devh --config devharness/cargo-mutants.toml \
  --in-place --in-diff /tmp/f03.diff --test-tool nextest --baseline=skip \
  --timeout-multiplier 2 </dev/null >/tmp/mut_full.log 2>&1' &
```

2. Смотреть прогресс **только** watcher'ом:

```bash
env LIVE=1 HARNESS_COLOR=always STEP=1 INTERVAL=2 devharness/tools/mutants-watch.sh
```

Watcher даёт: легенду значков, цветную пилюлю процента, счётчики с числами
(✓ caught / ✗ missed / ⏱ timeout / ⊘ unviable), avg, ETA в минутах, текущий файл.
Один кадр = один turn. Никаких `sleep`-петель.

3. Итог — только по `--once` после смерти `cargo-mutants` (`pgrep -x cargo-mutants` пусто).

## Канонические конфиги

| Что | Команда |
| --- | --- |
| Только ядро (`core/`) | `--config devharness/cargo-mutants.toml` (исключает `commands/**`, `shell/**`, `tests/**`) |
| Дельта по диффу | `--in-diff /tmp/<slice>.diff` **без** конфига (иначе `commands/`,`shell/` не аудируется) |

Kill-rate считается по **тестируемым**: `testable = total - unviable`.

## Почему именно watcher

- `sleep`-петли в сессии слепы (не видно остатка) и жгут turn'ы;
- watcher ждёт **событие** (`inotify` по `outcomes.json`), а не таймер;
- watcher синглтон — второй запуск сам гасит старые, `pkill -f mutants-watch` не нужен;
- оператор видит живую строку прямо в выводе tool'а, без лог-файлов.

## Капкан

`devharness/shell/guard-mutation.sh` — вызывается обёрткой; при попытке ручного
`cargo mutants|mutants ...` без watcher: отказ + эта команда на видном месте.

Правило дублируется в глобальной памяти `devh-mutation-rule`
(`~/.config/goose/memory/devh-mutation-rule.txt`), но память — это только подсказка;
**источник истины — этот файл и капкан.**
