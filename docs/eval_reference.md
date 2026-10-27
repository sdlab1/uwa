# Эталон оценки (calibration.jsonl) — сводка по уровням

Публикуемый отчёт по эталонам оценки агентов. Источник истины — `.harness/orch/calibration.jsonl`
(эталоны, которые пишет `devh orch calibrate`) + `.harness/orch/quality.jsonl` (замеры, их пишет
`devh orch verify`). Документ обновляется из этих файлов; команда обновления снимка оценки —
`devh orch calibrate --export` (единый снимок quality/model_ledger/calibration).

Свежесть эталона контролирует guard `calibration_guard` (T-512): эталон не старше 30 суток
(порог среды `HARNESS_REFERENCE_FRESH_DAYS`), иначе — Problem с уровнем и возрастом.

## Веса оценки (eval_config, значения по умолчанию)

| веса | категории | значения |
| --- | --- | --- |
| error_severity_weights | fatality / high / medium / low | 5.0 / 3.0 / 1.5 / 1.0 |
| warning_severity_weights | fatality / high / medium / low | 3.0 / 2.0 / 1.0 / 0.75 |
| autotune | вес ошибок в штрафе автотюна | 1.2 |

Взвешенные метрики эталона (`avg_errors_weighted`, `avg_warnings_weighted`) — это суммы
по категориям с этими множителями; `score` — сводный балл эталона (0..1), относительно
которого считается отклонение работы агента (`devh orch calibrate --dry-run`).

## Уровни

| уровень | замеров (требуется 4) | эталон (последний) | возраст, сут | score | build_ok | err_w | warn_w | comment | runs | rework |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| C1 | 12/4 | 2026-10-26 22:52 (minimax) | 0 | 0.8314 | 1.00 | 0.00 | 1.14 | 0.201 | 1.29 | 0.00 |
| C2 | 9/4 | 2026-10-26 22:52 (minimax) | 0 | 0.8539 | 1.00 | 0.00 | 3.43 | 0.222 | 1.00 | 0.00 |
| C3 | 8/4 | 2026-10-26 22:52 (minimax) | 0 | 0.9142 | 1.00 | 0.00 | 2.00 | 0.151 | 1.00 | 0.00 |
| C4 | 6/4 | 2026-10-26 22:52 (minimax) | 0 | 0.9139 | 1.00 | 0.00 | 1.50 | 0.499 | 1.00 | 0.00 |
| C5 | 8/4 | 2026-10-27 00:22 (minimax) | 0 | 0.8812 | 1.00 | 0.00 | 0.00 | 0.124 | 1.00 | 0.00 |

## Как читать

* **замеров**: сколько закрытых задач уровня участвовало (порог эталона — 4, `calibration::MIN_REFERENCE_TASKS`).
* **score**: сводный балл эталона; отклонение агента сверх эталона понижает его потолок C (`devh orch models`).
* **build_ok / err_w / warn_w / runs / rework**: чем меньше ошибки, предупреждения и повторные прогоны — тем выше балл.
* **возраст**: эталон старше 30 суток требует пересборки (`devh orch calibrate --c N`) — guard покажет Problem.

## Пересборка

```bash
devh orch calibrate            # все уровни C1..C4
devh orch calibrate --c 3      # один уровень
devh orch calibrate --export   # единый снимок оценки (quality/model_ledger/calibration)
```

Документ сгенерирован из calibration.jsonl (14 записей) + quality.jsonl (96 записей) 2026-10-27.