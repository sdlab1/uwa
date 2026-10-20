#!/usr/bin/env bash
# A5-8 (hang): висящий агент — watchdog (AGENT_TIMEOUT; в секции suite
# включается через HARNESS_AGENT_TIMEOUT=5). Ожидание: rc=1, «FAIL агент».
set -u
case "$1" in *VERDICT*) echo "VERDICT: OK"; exit 0;; esac
sleep 9999
