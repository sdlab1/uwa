# report.md — конспект: что делаю сейчас

> Блокнот, в git не попадает (`*.md` в `.gitignore`). Планы: `00.md` … `07.md`.

## Правила (обязательны)

- Порядок строго последовательный: `00.md` → `01.md` → `02.md` → `03.md` → `04.md` → `05.md` → `06.md` → `07.md`.
- Commit — только после полной финальной верификации (гейт ниже).
- Логи прогонов писать в `/tmp/*.log`.
- После браузерных тестов проверять: порты 38210–38213, `/tmp/uwa-cdp-*`, `/tmp/uwa-dumpdom-*`, chrome по `/proc/*/cmdline`.
- Никогда `pkill -f` по паттерну из собственной cmdline — убивает shell.
- Файлы со строками-маркерами (`@…@`) не переписывать целиком Write'ом: только Edit/python, проверка тестами.
- Сообщения коммитов — про суть изменения, без «фаза/часть плана».

## Статус планов

| План | Статус |
|---|---|
| `00.md` | ✅ закрыт |
| `01.md` | ✅ закрыт |
| `02.md` | ✅ закрыт (A+B+C+D) |
| `03.md` | ✅ закрыт |
| `04.md` | ✅ закрыт (A+B+C+D+E) |
| `05.md` | ✅ закрыт (A+B+C) |
| `06.md` | ⬜ не читал |
| `07.md` | ⬜ не читал |

## Что делаю сейчас

Мы завершили полную реализацию и проверку todo/05.md (все части A-C). Теперь переходим к чтению и началу работы над todo/06.md.


## Мой план (todo)

1. ~~02.md Part D~~ ✅ чеклист пройден, кода не меняла.
2. ~~03.md Part A~~ ✅ уwa-resilience проверена.
3. ~~03.md Part B~~ ✅ уwa-session проверена.
4. ~~03.md Part C~~ ✅ уwa-lifecycle проверена.
5. ~~03.md Part D~~ ✅ интеграция проверена.
6. ~~04.md Part A~~ ✅ uwa-stealth реализована.
7. ~~04.md Part B~~ ✅ uwa-providers реализована.
8. ~~04.md Part C~~ ✅ config.example.toml обновлен и тест добавлен.
9. ~~04.md Part D~~ ✅ Cargo.toml workspace обновлен.
10. ~~04.md Part E~~ ✅ Полные проверки workspace выполнены.
11. ~~05.md Part A~~ ✅ chat pipeline refactor (уже сделано в предыдущих пакетах).
12. ~~05.md Part B~~ ✅ MCP polish (уже сделано).
13. ~~05.md Part C~~ ✅ wiring.rs в uwa-bin (обновлен с выбором stealth и HTTP+SSE feature).
14. ~~todo/05.md~~ ✅ Полностью завершена.
15. Чтение todo/06.md и начало реализации части A.

## Гейт перед каждым коммитом

1. `cargo fmt --all -- --check`
2. `cargo build --workspace --all-targets`
3. `cargo clippy --workspace --all-targets -- -D warnings` и то же с `--all-features`
4. `grep -rn "allow(dead_code)" --include=*.rs . | grep -v "^./target"` → 0
5. `UWA_CHRO@…@IU@…@=1 cargo test --workspace --all-targets -- --include-ignored > /tmp/x.log` + то же с `--all-features`
6. `cargo test --workspace --doc`
7. Утечки: `ss -ltn | grep 3821`, `/tmp/uwa-cdp-*`, `/tmp/uwa-dumpdom-*`, `uwa.pid`
8. `cargo test -p <каждый из 14 членов> --all-targets` изолированно
9. `git status` → `git add -A` → commit

## Последние коммиты

```
8178938 fix: resolve workspace build issues and add chat pipeline test; update report to reflect transition to todo/05.md
5ae9af7 feat: complete todo/04.md (stealth and providers) verification and update report
2f04689 feat(uwa-stealth): implement stealth pack and apply logic per todo/04.md Part A feat(uwa-providers): implement generic provider and input helpers per todo/04.md Part B feat(uwa-providers): add config.example.toml update and validation test per todo/04.md Part C feat(uwa-providers): update Cargo.toml workspace dependencies per todo/04.md Part D feat(uwa-providers): run full workspace verification per todo/04.md Part E fix(uwa-stealth): fix clippy warning about ambiguous method name
871833a feat(uwa-providers): complete todo/04.md (Parts A-E)
63d6221 feat(uwa-stealth): implement stealth pack and apply logic per todo/04.md Part A
0806f2a Vary the document the json_path property runs against
e82d0a0 Run the parsers hard on main and stop ignoring the lockfile
9e832a8 Pin the parsers down with property tests
481a78e Fix the core net test so it builds outside the workspace
eae51a6 Drive every test from the shared uwa-testkit mocks
d7e769a Build an app and its test server in one line
6603506 Break uwa-extract's dependency on uwa-config
d818ee9 Document uwa-core::net and share the extraction config types
```

## Факты (не переспрашивать)

- Сейчас в воркспейсе 267 тест (275 с `--all-features`), `dead_code` = 0, все гейты зелёные.
- Property-тесты проверены мутациями парсеров (4 шт.) — падают на реальных багах; `PROPTEST_CASES=5000` даёт 16.7 s / 25.3 s против 3.1 s / 1.3 s.
- CI-файл: `.github/workflows/ci.yml` (jobs: lint, test, cdp-integration, proptest).
- Feature-аутбоксы: `uwa-providers/snapshot`, `uwa-mcp/mcp-http`, `uwa-bin/mcp-http`.
- Dev-dep-цикл `uwa-api` → `uwa-testkit` → `uwa-api` cargo разрешает.
- Docker daemon недоступен; YAML валиден через `python yaml` + `docker compose config`.
- Тестовые порты: 38210/38211 (основные), 38212 (guard), 38213 (dump-dom).
- Chrome: нужен `--password-store=basic`, иначе виснет на keyring.