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
reference is kept as `devh-classic`. `test.sh` — 95 asserts, by default it
runs against the Rust `devh` (CI `devharness-rs.yml`), separately — against
bash:

    bash devharness/test.sh                                    # Rust (default)
    DEVH=$PWD/devharness/devh-classic bash devharness/test.sh  # bash reference

## Daily cycle

    cd <your-repo>
    devh init                              # once: auto-detect VERIFY/RULES
    printf 'Do X.\nRED: mix test …\n' | devh new "task X"
    devh bg 1 && devh ps                   # detached + observation
    tail -f .harness/logs/W-001-agent.log
    devh merge                             # rebase → ff-push → log → next:

Autopilot: `devh auto --from specs/ --max 5` — specs → items (RED extracted
from the `RED:` line) → run+merge+tickets; an item without RED honestly
stays in the queue. In the background: `devh bg auto --from specs/ --max 5`
— detached autopilot (log `.harness/logs/bg-auto.log`); `devh ps` also
shows live autopilot runs (`.harness/auto.pid`).

## Gates (all mandatory except bench)

verify → no-ignored → lint → review → mutation(kill≥MIN_KILL) → [bench];
RED proof first, MSG_DENY hygiene last. Failure → fix-round with the tail
of the log (≤ MAX_FIX); a missing tool = an ENV blocker, not a fix-round.

| Gate | Config | Meaning |
|---|---|---|
| VERIFY | `VERIFY` | tests/build before commit |
| NO-IGNORED | `NO_IGNORED=1` | `N ignored > 0` = hidden red |
| MUTATE | `MUTATE`, `MIN_KILL=90` | kill-rate ≥ MIN_KILL% on changed `MUTATE_SCOPE` files (`{module}`/`{file}`); output format `Mutants: N, Killed: M` (muex / cargo-mutants); missing tool = env blocker (rc=2), not a fix-round |
| RED | `items/W-NNN/red.cmd` | first of all: fails on a clean HEAD before the work, goes green after; only recorded bypass (`--no-red`) |
| LINT | `LINT=1`, `LINT_CMD` | auto-shellcheck of changed *.sh + project linter |
| REVIEW | `REVIEW=1` | self-review of the diff with a second request; `VERDICT: OK` / `VERDICT: ISSUES: …` → fix-round |
| BENCH | `BENCH` | the only optional gate: perf vs baseline |

## Commands

| Command | Syntax | What it does |
|---|---|---|
| init | `devh init` | config with auto-detection (uwa→scripts/verify.sh; gobot→mix test+muex; rust→cargo test), todo/report.md→RULES/RULES_LOG/MSG_DENY; `.harness/` into the local exclude |
| new | `devh new "title" [--no-red "reason"]` / `devh new <N>` | item + spec.md; on a TTY stdin — opens an editor (`$EDITOR`, default vi); `<N>` — copy of `todo/N.md` → `W-0NN` |
| list | `devh list` | table: ID / STATUS(open→active→done) / WT / TITLE |
| hint | `devh hint [id]` | spec + plan + ready prompt for the next model |
| run | `devh run [id]` | claim (WIP limit, flock) → worktree → RED pre → agent → gate cycle (failure → fix-round with the log tail) → RED post → hygiene → commit → push branch |
| commit | `devh commit [id]` | re-run commit/push for finished work; idempotent |
| merge | `devh merge [id]` | guard (clean tree, has commits) → rebase → ff-push to MAIN → `## Session N` into RULES_LOG → log → cleanup → `next:` |
| abort | `devh abort [id]` | worktree/branch gone, item → open (work discarded) |
| clean | `devh clean` | worktrees/branches of done items + log retention (LOG_DAYS) |
| doctor | `devh doctor` | environment: git/VERIFY (+ a smoke run ≤120s)/agent/flock/timeout/remote/exclude/mutation/shellcheck/bench + crash-recovery for orphans; exit≠0 on fail |
| logs | `devh logs [id]` | item log paths + agent/verify tails; without id — fresh logs of all items |
| bg | `devh bg [id]` / `devh bg auto --from <dir> [--max N]` | detached run: setsid+nohup (own session, immune to HUP), log in bg.log / bg-auto.log; parallel work is safe (run.lock + WIP) |
| ps | `devh ps` | PID, elapsed, current stage from timing.log; live autopilot runs (auto.pid); dead pid files are flagged |
| tickets | `devh tickets [--import]` | OnFlyAnalysis observations: show / turn into items (no red idea — rejected) |

## Observability — where to look

All state and logs live in the target repo under `.harness/` (never committed):

| Path | What's inside |
|---|---|
| `.harness/logs/W-NNN-agent.log` | full agent output for the last round |
| `.harness/logs/W-NNN-verify.log` / `-mutate.log` / `-bench.log` | gate outputs for the last round |
| `.harness/logs/W-NNN-bootstrap.log` | worktree preparation (deps) |
| `.harness/logs/W-NNN-timing.log` | stage timings (seconds): worktree / red / agent-rN / verify-rN / mutate-rN / bench-rN / commit — printed at the end of run and appended to log.md; bottleneck visible at a glance |
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
| BENCH | '' | perf gate (optional) |
| RULES / RULES_LOG | todo/report.md | gotchas reference for the prompt / where «## Session N» is appended |
| MSG_DENY | `todo/\|report\.md\|Session [0-9]+` | commit-message hygiene |
| NO_IGNORED | 1 | ignored-tests gate |
| LINT / LINT_CMD | 1 / '' | auto-shellcheck *.sh / project linter |
| REVIEW | 1 | self-review of the diff with a second agent request |
| HARNESS_COLOR / NO_COLOR | auto / — | always/never/auto — cargo-style colored output; NO_COLOR respected |

## Self-test — the same stack that gates other projects

clippy (the shellcheck layer) → 25-section suite (port of `test.sh`, 95
asserts; the oracle is run directly: `DEVH=<rust-bin> bash
devharness/test.sh`) → cargo-mutants on core/ (100% kill) → llvm-cov ≥80%
→ oracle on the musl static binary (flock/fs2/setsid on static-pie).

CI: `.github/workflows/devharness-rs.yml` (Rust: fmt → clippy → tests →
oracle through the `devh` wrapper → llvm-cov → cargo-mutants) and
`.github/workflows/devharness.yml` (bash reference `devh-classic`:
shellcheck → test.sh → selfmutate.sh).

## Migration

- Done: bash `devh` → `devh-classic` (still gated by CI `devharness.yml`:
  shellcheck + test.sh + selfmutate 16/16); `devh` is a wrapper over the
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
