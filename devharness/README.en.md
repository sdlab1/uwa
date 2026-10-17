# devharness (Rust) — KISS dev harness

[Русский](README.md)

A single `devh` binary. Same model: **the harness owns git and the cycle;
the agent works in an isolated worktree and never runs git.**
State lives in `.harness/` of the target repo (flock + atomic writes, no DB).

## Install

    cargo install --path devharness        # devh into PATH; in a checkout — ./devharness/devh
                                           # (wrapper: prebuilt target/devh/debug/devh, else cargo run)

Static binary for distribution (flock/fs2/setsid verified by the full oracle — 95/95):

    rustup target add x86_64-unknown-linux-musl
    cargo build --release --target x86_64-unknown-linux-musl -p devh
    # → target/x86_64-unknown-linux-musl/release/devh (static-pie)

`devh` in the checkout is a wrapper over the Rust binary; the bash
reference is kept as `devh-classic`. `test.sh` — 30 sections / 127 asserts,
by default it runs against the Rust `devh` (CI `devharness-rs.yml`),
separately — against bash:

    bash devharness/test.sh                                    # Rust (default)
    DEVH=$PWD/devharness/devh-classic bash devharness/test.sh  # bash reference

## Quick start — first task

Entry point is `devh` on PATH (`cargo install`) or `./devharness/devh` in the
checkout; work from the repo root.

    cd <your-repo>
    devh init                                  # once: VERIFY/RULES/MSG_DENY
    printf 'What to do: …\nRED: cargo test -- <fails right now>\n' \
      | devh new "SHORT TITLE"                 # item + spec.md + red.cmd (from the RED: line)
    devh bg 1 && devh ps                        # detached run + status
    devh logs 1                                 # agent/verify tails
    devh merge 1                                # rebase → ff-push, once gates are green

RED proof is mandatory: a `RED: …` line in the spec writes `red.cmd`
up front; specs without it still enter the cycle — the agent writes
`red.cmd` as its FIRST task (red-author phase; the prompt demands a real
test, not a grep). Bypass: `devh new "…" --no-red "reason"`.

## Daily cycle

    cd <your-repo>
    devh init                              # once: auto-detect VERIFY/RULES
    printf 'Do X.\nRED: mix test …\n' | devh new "task X"
    devh bg 1 && devh ps                   # detached + observation
    tail -f .harness/logs/W-001-agent.log
    devh merge                             # rebase → ff-push → log → next:

Autopilot: `devh auto --from specs/ --max 5` — specs → items (RED
extracted from the `RED:` line, otherwise the agent's red phase) →
run+merge+tickets; a spec without RED does not fall out of the cycle. In
the background: `devh bg auto --from specs/ --max 5`
— detached autopilot (log `.harness/logs/bg-auto.log`); `devh ps` also
shows live autopilot runs (`.harness/auto.pid`).

## Gates (all mandatory except bench)

RED-LINK → RED proof (pre) → verify → no-ignored → lint →
crap(score≤CRAP_MAX) → review → mutation(kill≥MIN_KILL) → [bench] → RED
(post) → RED-MUT; MSG_DENY hygiene last. Failure → fix-round with the tail
of the log (≤ MAX_FIX); a missing tool = an ENV blocker, not a fix-round.

| Gate | Config | Meaning |
|---|---|---|
| VERIFY | `VERIFY` | tests/build before commit |
| NO-IGNORED | `NO_IGNORED=1`, `IGNORED_ALLOWLIST` | allowlist semantics: an ignore is allowed if the test name matches a regex in `IGNORED_ALLOWLIST` (file, default `scripts/ignored-tests.txt`); an undeclared ignore = hidden red |
| MUTATE | `MUTATE`, `MIN_KILL=90` | kill-rate ≥ MIN_KILL% on changed `MUTATE_SCOPE` files (`{module}`/`{file}`); output format `Mutants: N, Killed: M` (muex / cargo-mutants); missing tool = env blocker (rc=2), not a fix-round |
| CRAP | `CRAP`, `CRAP_MAX=12` | guardrail #2 (Uncle Bob): score = C²·(1−cov)³ + C, violation = score > CRAP_MAX; output format `file:line fun C=N cov=N` + summary `CRAP: functions=N`; unrecognized contract = env blocker; bypass only recorded (`--no-crap`) |
| RED | `items/W-NNN/red.cmd` | first of all: fails on a clean HEAD before the work, goes green after; source — the spec's `RED:` line or the agent's red phase (the first task of run/auto); only recorded bypass (`--no-red`) |
| RED-LINK | compared against `VERIFY` | red.cmd must run the very same runner/binary as VERIFY (signature = interpreter+script or the binary; subcommands joined by `;`/`&&`/`||`/`|` are considered). A mismatch → re-authoring with a corrective prompt (≤ MAX_FIX), then `FAIL: RED-LINK`; an empty VERIFY = the check does not apply; bypass `--no-red` |
| RED-MUT | `RED_MUTATE`, `RED_MIN_KILL=90` | mutation control of the red test: `{test}` is substituted with red.cmd, kill-rate ≥ RED_MIN_KILL% on changed `MUTATE_SCOPE` files; no `{test}` = refusal with a hint; log `logs/W-NNN-redmut.log`; an empty `RED_MUTATE` = off |
| LINT | `LINT=1`, `LINT_CMD` | auto-shellcheck of changed *.sh + project linter |
| REVIEW | `REVIEW=1` | self-review of the diff with a second request; `VERDICT: OK` / `VERDICT: ISSUES: …` → fix-round |
| BENCH | `BENCH` | the only optional gate: perf vs baseline |

## Commands

| Command | Syntax | What it does |
|---|---|---|
| init | `devh init` | config with auto-detection (uwa→scripts/verify.sh; gobot→mix test+muex; rust→cargo test), todo/report.md→RULES/RULES_LOG/MSG_DENY; `.harness/` into the local exclude |
| new | `devh new "title" [--no-red "reason"] [--no-crap "reason"]` / `devh new <N>` | item + spec.md; on a TTY stdin — opens an editor (`$EDITOR`, default vi); `<N>` — copy of `todo/N.md` → `W-0NN` |
| list | `devh list` | table: ID / STATUS(open→active→done) / WT / TITLE |
| hint | `devh hint [id]` | spec + plan + ready prompt for the next model |
| run | `devh run [id]` | claim (WIP limit, flock) → worktree → RED-LINK + pre (no `red.cmd` → the agent is tasked to write it as the first task; then the runner is compared with VERIFY and the pre-check runs) → agent → gate cycle (failure → fix-round with the log tail) → RED post → RED-MUT (`RED_MUTATE`) → hygiene → commit → push branch |
| commit | `devh commit [id]` | re-run commit/push for finished work; idempotent |
| merge | `devh merge [id]` | guard (clean tree, has commits) → rebase → ff-push to MAIN → `## Session N` into RULES_LOG → log → cleanup → `next:` |
| abort | `devh abort [id]` | worktree/branch gone, item → open (work discarded) |
| clean | `devh clean` | worktrees/branches of done items + log retention (LOG_DAYS) |
| doctor | `devh doctor` | environment: git/VERIFY (+ a smoke run ≤120s)/agent/flock/timeout/remote/exclude/mutation/CRAP (+ smoke)/shellcheck/bench + crash-recovery for orphans; exit≠0 on fail |
| logs | `devh logs [id]` | item log paths + agent/verify tails; without id — fresh logs of all items |
| bg | `devh bg [id]` / `devh bg auto --from <dir> [--max N]` | detached run: setsid+nohup (own session, immune to HUP), log in bg.log / bg-auto.log; parallel work is safe (run.lock + WIP) |
| ps | `devh ps` | PID, elapsed, current stage from timing.log; live autopilot runs (auto.pid); dead pid files are flagged |
| tickets | `devh tickets [--import]` | OnFlyAnalysis observations: show / turn into items (no red idea — rejected) |

## Observability — where to look

All state and logs live in the target repo under `.harness/` (never committed):

| Path | What's inside |
|---|---|
| `.harness/logs/W-NNN-agent.log` | full agent output for the last round |
| `.harness/logs/W-NNN-verify.log` / `-mutate.log` / `-crap.log` / `-bench.log` | gate outputs for the last round |
| `.harness/logs/W-NNN-bootstrap.log` | worktree preparation (deps) |
| `.harness/logs/W-NNN-timing.log` | stage timings (seconds): worktree / red / agent-rN / verify-rN / crap-rN / mutate-rN / bench-rN / commit — printed at the end of run and appended to log.md; bottleneck visible at a glance |
| `.harness/log.md` | append-only journal of closed sessions: commit, fix-rounds/gates/duration, summary |
| `.harness/summaries/W-NNN.md` | agent summary (first line = commit message) |
| `.harness/items/W-NNN/` | spec.md, plan.md, red.cmd, status, agent (override) |
| `.harness/observations.md` | OnFlyAnalysis queue: `<file>:<line> | <suspicion> | red-test: <idea>`; `devh tickets --import` turns them into items |

- **Live run**: `tail -f .harness/logs/W-001-agent.log` (or `devh logs W-001`).
- **What's in flight**: `devh list`, `devh doctor`, `devh ps`.
- **Per-run metrics**: the `run: fix-rounds=N gates=… duration=Ns` line in log.md.
- `devh hint` — ready prompt for the next model: spec + plan + rules + repo context.

## Config (`.harness/config`, env `HARNESS_*` overrides)

| Key | Default | Meaning |
|---|---|---|
| VERIFY | auto-detect | verification command; must pass |
| BOOTSTRAP | auto-detect | one-time worktree prep (deps) |
| AGENT | `goose run --no-session -t` | agent; prompt as a single argument |
| AGENT_TIMEOUT | 1800 | seconds per agent round (watchdog «agent died») |
| MAX_FIX | 3 | fix-rounds per gate cycle |
| MAX_ACTIVE | 3 | WIP limit |
| REMOTE / MAIN | origin / current | where we push |
| WT_ROOT | /tmp | where worktrees live (slug per repo — no collisions) |
| LOG_DAYS | 30 | log retention |
| MUTATE / MUTATE_SCOPE / MIN_KILL | muex / lib / 90 | mutation gate (auto-detect on init) |
| RED_MUTATE / RED_MIN_KILL | '' / 90 | mutation control of the red test: `{test}` = red.cmd; an empty RED_MUTATE = off |
| CRAP / CRAP_SCOPE / CRAP_MAX | '' / '' / 12 | CRAP gate (auto-detect on init: gobot→mix crap.report, scripts/crap-rs.sh→bash); empty CRAP = off |
| BENCH | '' | perf gate (optional) |
| RULES / RULES_LOG | todo/report.md | gotchas reference for the prompt / where «## Session N» is appended |
| MSG_DENY | `todo/\|report\.md\|Session [0-9]+` | commit-message hygiene |
| NO_IGNORED / IGNORED_ALLOWLIST | 1 / scripts/ignored-tests.txt | allowlist gate for ignored tests: undeclared ignore = fail; allowlist = file of name regexes (missing file = empty) |
| LINT / LINT_CMD | 1 / '' | auto-shellcheck *.sh / project linter |
| REVIEW | 1 | self-review of the diff with a second agent request |
| HARNESS_COLOR / NO_COLOR | auto / — | always/never/auto — cargo-style colored output; NO_COLOR respected |

## Self-test — the same stack that gates other projects

clippy (the shellcheck layer) → 30-section suite (port of `test.sh`, 127
asserts; the oracle is run directly: `DEVH=<rust-bin> bash
devharness/test.sh`) → cargo-mutants on core/ (100% kill) → llvm-cov ≥80%
→ oracle on the musl static binary (flock/fs2/setsid on static-pie).

CI: `.github/workflows/devharness-rs.yml` (Rust: fmt → clippy → tests →
oracle through the `devh` wrapper → llvm-cov → cargo-mutants) and
`.github/workflows/devharness.yml` (bash reference `devh-classic`:
shellcheck → test.sh → selfmutate.sh).

## Migration

- Done: bash `devh` → `devh-classic` (still gated by CI `devharness.yml`:
  shellcheck + test.sh + selfmutate 21/21); `devh` is a wrapper over the
  Rust binary, CI `devharness-rs.yml` runs the oracle through it.
- Specs of old sessions → one by one via `devh new <N>` / `devh new "..."`.

### Burn-in → retirement of classic (exit criteria)

Step 2b: 1–2 weeks of daily use through the wrapper (dual CI). Step 3
retirement (`git rm devh-classic selfmutate.sh`, delete
`devharness.yml`, selfcov goes away; the mutation oracle = cargo-mutants)
— **ONLY** when ALL criteria hold:

- ≥14 calendar days since the switch date (edd64c6, 2026-10-16) — not
  before 2026-10-30;
- both devharness-CIs (`devharness`, `devharness-rs`) green on every
  `devharness/**` run over the period;
- zero patches/fixes to `devh-classic` over the period;
- all new devh sessions of the period go through the wrapper (classic
  only for CI oracle runs; `.harness/log.md` gains Sessions after the
  switch);
- cargo-mutants: 100% viable kill-rate is holding.
