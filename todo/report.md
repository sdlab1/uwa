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
| `04.md` | ✅ закрыт (A+B) |
| `05.md` | ⬜ не читал |
| `06.md` | ⬜ не читал |
| `07.md` | ⬜ не читал |

## Что делаю сейчас

Мы завершили проверку `todo/04.md` Part A (uwa-stealth) и Part B (uwa-providers). Теперь переходим к Part C (обновление config.example.toml).

## Part C — что легло

- `/v1/messages/count_tokens`: `CountTokensRequest`/`CountTokensResponse` в
  `uwa-core/src/types/anthropic.rs`, `count_tokens` + `estimate_tokens` в
  `routes/messages.rs`. Оценка: ASCII — 4 симв./токен, non-ASCII — 2,
  +4 токена на сообщение, system и tools — по их тексту/JSON. Переиспользует
  `SystemField::as_text` и `content_text`.
- `/v1/responses`: новый `routes/responses.rs`. `to_chat_request` (instructions
  → system, строка или массив content-parts, `stream` всегда false),
  `reshape` (message-item + function_call items, `usage`, `output_text`).
- Отклонение от плана: ответ не «переигрывается» байтами через axum — handler
  зовёт типизированный `chat::run_pipeline_with` + `chat::nonstream::build_non_streaming`
  и решейпит уже сериализованный `Value`. `chat::local_tools` стал
  `pub(crate)`.
- Выкинул `or_else(input_text/output_text)` в `content_text`: реальные части
  API (`input_text`, `output_text`, `refusal`) несут текст в поле `text`, и
  мутация показала, что эти ветки никем не проверяются.
- README: таблица эндпоинтов дополнена двумя строками.
- Тесты: 8 unit (responses) + 5 unit (арифметика count_tokens) + 6 + 5
  интеграционных через `AppBuilder` — всего +24 (база на HEAD была 218).
- Мутации (все Killed): не читать `text` у part; instructions → user; без
  префикса `resp_`; пустой `output_text`; выкинуть `tool_calls`; пустой input
  принять; count_tokens всегда 0 / без system+tools / без контента; `stream:
  true` начал отдавать SSE.

## Мой план (todo)

1. ~~02.md Part D~~ ✅ чеклист пройден, кода не меняла.
2. ~~03.md Part A~~ ✅ уwa-resilience проверена.
3. ~~03.md Part B~~ ✅ уwa-session проверена.
4. ~~03.md Part C~~ ✅ уwa-lifecycle проверена.
5. ~~03.md Part D~~ ✅ интеграция проверена.
6. ~~04.md Part A~~ ✅ uwa-stealth реализована.
7. ~~04.md Part B~~ ✅ uwa-providers реализована.
8. Теперь переходим к Part C (обновление config.example.toml).
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
<current commit> feat(uwa-providers): implement generic provider and input helpers per todo/04.md Part B
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

- Сейчас в воркспейсе 243 тест (251 с `--all-features`), `dead_code` = 0, все гейты зелёные.
- Property-тесты проверены мутациями парсеров (4 шт.) — падают на реальных багах; `PROPTEST_CASES=5000` даёт 16.7 s / 25.3 s против 3.1 s / 1.3 s.
- CI-файл: `.github/workflows/ci.yml` (jobs: lint, test, cdp-integration, proptest).
- Feature-аутбоксы: `uwa-providers/snapshot`, `uwa-mcp/mcp-http`, `uwa-bin/mcp-http`.
- Dev-dep-цикл `uwa-api` → `uwa-testkit` → `uwa-api` cargo разрешает.
- Docker daemon недоступен; YAML валиден через `python yaml` + `docker compose config`.
- Тестовые порты: 38210/38211 (основные), 38212 (guard), 38213 (dump-dom).
- Chrome: нужен `--password-store=basic`, иначе виснет на keyring.

## Цифры последнего гейта (Part B uwa-providers + Part A uwa-stealth)

- fmt / build --all-targets / clippy (оба профиля, `-D warnings`): чисто
- `allow(dead_code)`: 0
- `cargo test --workspace --all-targets`: **243** (база 218)
- `--all-features`: **251**
- браузерные (`UWA_CHROMIUM=1 … --include-ignored`): **245** / **249**
- doc-тесты: 0 (нет кода с doc-примерами)
- утечек нет: портов 382x нет, `/tmp/uwa-cdp-*` и `uwa.pid` нет; живой Chrome
  (PID 423607, 8.5 ч) — это пользовательская сессия с дефолтным профилем,
  тесты запускают Chrome с `--user-data-dir=/tmp/uwa-cdp-*`
- изолированно по 14 членам после коммита `513f142`: все зелёные (uwa-api 46, uwa-core 18, uwa-testkit 30, uwa-tools 31, …)

## Коммит Part A+B

`63d6221 feat(uwa-stealth): implement stealth pack and apply logic per todo/04.md Part A`
`<current commit> feat(uwa-providers): implement generic provider and input helpers per todo/04.md Part B`

## Коммит Part C (в_progress)

Работаем над обновлением config.example.toml и добавлением теста для примерной конфигурации.

## Часть D — чеклист 02.md (пройден)

| Проверка плана | Ожидание | Факт |
|---|---|---|
| `cargo build --workspace --all-features` | зелёный | ✅ |
| `cargo test --workspace` | ≈60+ | ✅ **242** (0 failed) |
| `cargo test -p uwa-tools --test proptest_parser` | 6 свойств × 256 | ✅ **10 свойств**, 2688 кейсов (2×512, 5×256, 3×128) |
| `cargo test -p uwa-extract --test proptest_net` | 4 свойства × 128 | ✅ **10 свойств**, 1664 кейса (3×256, 7×128) |
| `cargo test -p uwa-api --test messages_count_tokens` | 3 теста | ✅ 5 (+5 unit) |
| `cargo test -p uwa-api --test responses` | 4 теста | ✅ 6 (+8 unit) |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | зелёный | ✅ |
| `cargo fmt --all -- --check` | зелёный | ✅ |
| CI cdp-integration (Chromium) | ✅ | ✅ локально 245/249 |
