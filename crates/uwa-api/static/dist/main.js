"use strict";
(() => {
  // src/api.ts
  var KEY_STORAGE = "uwa:apiKey";
  var ApiError = class extends Error {
    constructor(status, code, message) {
      super(message);
      this.status = status;
      this.code = code;
      this.name = "ApiError";
    }
  };
  function getApiKey() {
    return localStorage.getItem(KEY_STORAGE) ?? "";
  }
  function setApiKey(key) {
    if (key)
      localStorage.setItem(KEY_STORAGE, key);
    else
      localStorage.removeItem(KEY_STORAGE);
  }
  var keyListeners = [];
  function onKeyChange(fn) {
    keyListeners.push(fn);
    return () => {
      const i = keyListeners.indexOf(fn);
      if (i >= 0)
        keyListeners.splice(i, 1);
    };
  }
  function notifyKeyChange() {
    for (const fn of keyListeners)
      fn();
  }
  var UwaClient = class {
    constructor(base = "") {
      this.base = base;
    }
    headers(extra = {}) {
      const key = getApiKey();
      const h2 = { "content-type": "application/json", ...extra };
      if (key)
        h2["authorization"] = `Bearer ${key}`;
      return h2;
    }
    url(path) {
      return `${this.base}${path}`;
    }
    async request(path, init) {
      const r = await fetch(this.url(path), { ...init, headers: this.headers(init?.headers) });
      if (!r.ok) {
        let code = "http_error";
        let message = `${r.status} ${r.statusText}`;
        try {
          const body = await r.json();
          if (body?.error) {
            message = body.error.message ?? message;
            code = body.error.code ?? code;
          }
        } catch {
        }
        throw new ApiError(r.status, code, message);
      }
      if (r.status === 204)
        return void 0;
      return await r.json();
    }
    // ---------- public (no key) ----------
    health() {
      return this.request("/healthz");
    }
    ready() {
      return this.request("/readyz");
    }
    // ---------- key-gated ----------
    models() {
      return this.request("/v1/models");
    }
    providers() {
      return this.request("/v1/provider/status");
    }
    pool() {
      return this.request("/api/pool/status");
    }
    selectorTest(body) {
      return this.request("/admin/selector-test", { method: "POST", body: JSON.stringify(body) });
    }
    history(limit = 50, provider, status) {
      const p = new URLSearchParams({ limit: String(limit) });
      if (provider)
        p.set("provider", provider);
      if (status)
        p.set("status", status);
      return this.request(`/admin/history?${p.toString()}`);
    }
    stats() {
      return this.request("/admin/stats");
    }
    sessions() {
      return this.request("/admin/sessions");
    }
    dropSession(id) {
      return this.request(`/admin/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
    }
    recoverSessions() {
      return this.request("/admin/sessions/recover", { method: "POST" });
    }
    // ---------- chat ----------
    async chat(req) {
      return this.request("/v1/chat/completions", {
        method: "POST",
        body: JSON.stringify({ ...req, stream: false })
      });
    }
    /**
     * Streaming chat. Calls `onDelta(content, raw)` for every chunk and
     * `onToolCall(partial)` for function-call fragments. Returns the final
     * aggregated text.
     */
    async chatStream(req, handlers = {}) {
      const r = await fetch(this.url("/v1/chat/completions"), {
        method: "POST",
        headers: this.headers({ accept: "text/event-stream" }),
        body: JSON.stringify({ ...req, stream: true }),
        signal: handlers.signal
      });
      if (!r.ok || !r.body) {
        let msg = `${r.status} ${r.statusText}`;
        try {
          const j = await r.json();
          msg = j?.error?.message ?? msg;
        } catch {
        }
        throw new ApiError(r.status, "stream_failed", msg);
      }
      const reader = r.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      let accumulated = "";
      while (true) {
        const { done, value } = await reader.read();
        if (done)
          break;
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          const dataLines = frame.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trimStart());
          if (dataLines.length === 0)
            continue;
          const payload = dataLines.join("\n");
          if (payload === "[DONE]") {
            return accumulated;
          }
          let ev;
          try {
            ev = JSON.parse(payload);
          } catch {
            continue;
          }
          const delta = ev.choices?.[0]?.delta;
          if (!delta)
            continue;
          if (typeof delta.content === "string" && delta.content.length > 0) {
            accumulated += delta.content;
            handlers.onDelta?.(delta.content, ev);
          }
          if (Array.isArray(delta.tool_calls)) {
            for (const raw of delta.tool_calls) {
              const tc = raw;
              const fn = tc["function"] ?? {};
              handlers.onToolCall?.({
                index: Number(tc["index"] ?? 0),
                id: typeof tc["id"] === "string" ? tc["id"] : void 0,
                name: typeof fn["name"] === "string" ? fn["name"] : void 0,
                arguments: typeof fn["arguments"] === "string" ? fn["arguments"] : void 0
              });
            }
          }
        }
      }
      return accumulated;
    }
    /** Subscribe to the live log stream. Returns a closer. */
    streamLogs(onEvent) {
      const url = this.url("/admin/logs/stream");
      const key = getApiKey();
      const src = new EventSource(key ? `${url}?api_key=${encodeURIComponent(key)}` : url);
      src.onmessage = (e) => {
        try {
          onEvent(JSON.parse(e.data));
        } catch {
        }
      };
      src.onerror = () => {
      };
      return () => src.close();
    }
  };

  // src/router.ts
  var Router = class {
    constructor(routes2, defaultPath) {
      this.routes = routes2;
      this.defaultPath = defaultPath;
      this.current = "";
      this.cleanup = void 0;
    }
    start() {
      window.addEventListener("hashchange", () => this.handle());
      this.handle();
    }
    navigate(path) {
      if (location.hash === `#${path}`)
        return;
      location.hash = `#${path}`;
    }
    handle() {
      if (this.cleanup) {
        try {
          this.cleanup();
        } catch {
        }
        this.cleanup = void 0;
      }
      const hash = location.hash.slice(1) || this.defaultPath;
      const route = this.routes.find((r) => r.path === hash) ?? this.routes.find((r) => r.path === this.defaultPath);
      if (!route)
        return;
      this.current = route.path;
      const nav = document.getElementById("nav");
      if (nav) {
        nav.replaceChildren(
          ...this.routes.map(
            (r) => Object.assign(document.createElement("a"), {
              href: `#${r.path}`,
              textContent: r.label,
              className: r.path === route.path ? "active" : ""
            })
          )
        );
      }
      const app = document.getElementById("app");
      if (app) {
        app.replaceChildren();
        const cleanup = route.render(app);
        if (typeof cleanup === "function")
          this.cleanup = cleanup;
      }
    }
  };

  // src/ui.ts
  function h(tag, attrs = {}, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false)
        continue;
      if (k === "class")
        el.className = String(v);
      else if (k === "text")
        el.textContent = String(v);
      else if (k === "html")
        el.innerHTML = String(v);
      else if (k.startsWith("on") && typeof v === "function") {
        el.addEventListener(k.slice(2).toLowerCase(), v);
      } else if (k === "style" && typeof v === "object" && v !== null) {
        Object.assign(el.style, v);
      } else {
        el.setAttribute(k, String(v));
      }
    }
    for (const c of children) {
      if (c == null || c === false)
        continue;
      el.append(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return el;
  }
  function copyButton(text) {
    return h(
      "button",
      {
        class: "copy",
        type: "button",
        onclick: async (e) => {
          const btn = e.currentTarget;
          try {
            await navigator.clipboard.writeText(text);
            const prev = btn.textContent;
            btn.textContent = "copied";
            setTimeout(() => btn.textContent = prev, 1200);
          } catch {
            btn.textContent = "failed";
            setTimeout(() => btn.textContent = "copy", 1200);
          }
        }
      },
      "copy"
    );
  }
  function codeBlock(code, lang) {
    return h(
      "div",
      { class: "code-block" },
      copyButton(code),
      h("pre", {}, lang ? `# ${lang}
${code}` : code)
    );
  }
  function toast(msg, kind = "info", ms = 4e3) {
    const root = document.getElementById("toast-root");
    if (!root)
      return;
    const t = h("div", { class: `toast ${kind}` }, msg);
    root.append(t);
    setTimeout(() => t.remove(), ms);
  }
  function fmtMs(ms) {
    if (ms < 1e3)
      return `${Math.round(ms)} ms`;
    return `${(ms / 1e3).toFixed(2)} s`;
  }
  function fmtRel(secs) {
    if (secs < 60)
      return `${secs}s`;
    if (secs < 3600)
      return `${Math.floor(secs / 60)}m`;
    if (secs < 86400)
      return `${Math.floor(secs / 3600)}h`;
    return `${Math.floor(secs / 86400)}d`;
  }
  function urlMatches(pattern, url) {
    const star = pattern.indexOf("*");
    if (star < 0)
      return pattern === url;
    const head = pattern.slice(0, star);
    const tail = pattern.slice(star + 1);
    return url.startsWith(head) && (tail === "" || url.endsWith(tail));
  }
  function dialog(opts) {
    return new Promise((resolve) => {
      const d = h("dialog", {});
      let result = "closed";
      const primary = h(
        "button",
        {
          class: "btn",
          type: "button",
          onclick: () => {
            result = "primary";
            d.close();
          }
        },
        opts.primaryLabel ?? "OK"
      );
      const secondary = opts.secondaryLabel ? h(
        "button",
        {
          class: "btn secondary",
          type: "button",
          onclick: () => {
            result = "secondary";
            d.close();
          }
        },
        opts.secondaryLabel
      ) : null;
      d.append(
        h("h3", {}, opts.title),
        opts.body,
        h("div", { class: "actions" }, secondary, primary)
      );
      d.addEventListener("close", () => {
        d.remove();
        resolve(result);
      });
      document.body.append(d);
      d.showModal();
    });
  }

  // src/views/dashboard.ts
  function renderDashboard(root, api2) {
    const statsCard = h("div", { class: "card" });
    const readyCard = h("div", { class: "card" });
    const perProvider = h("div", { class: "card" });
    root.append(
      h("h2", {}, "Dashboard"),
      h("p", { class: "lead" }, "Live view of requests, providers and readiness."),
      readyCard,
      statsCard,
      perProvider
    );
    let timer;
    const refresh = async () => {
      try {
        const ready = await api2.ready();
        renderReady(readyCard, ready);
      } catch (e) {
        readyCard.replaceChildren(h("div", { class: "banner err" }, `readyz failed: ${String(e)}`));
      }
      try {
        const stats = await api2.stats();
        renderStats(statsCard, perProvider, stats);
      } catch (e) {
        statsCard.replaceChildren(h("h3", {}, "Stats"), h("p", { class: "muted" }, `not available: ${String(e)}`));
        perProvider.replaceChildren();
      }
    };
    refresh();
    timer = window.setInterval(refresh, 3e3);
    return () => {
      if (timer !== void 0)
        window.clearInterval(timer);
    };
  }
  function renderReady(node, r) {
    node.replaceChildren(
      h(
        "div",
        { class: "card-head" },
        h("h3", {}, "Readiness"),
        h("span", { class: r.providers_loaded ? "pill pill-ok" : "pill pill-warn" }, r.status)
      ),
      h(
        "dl",
        { class: "kv" },
        h("dt", {}, "providers"),
        h("dd", {}, r.providers_loaded ? "loaded" : "none"),
        h("dt", {}, "models"),
        h("dd", {}, String(r.models))
      )
    );
  }
  function renderStats(card, perProviderCard, s) {
    const cards = [
      ["Total", String(s.total)],
      ["Success", String(s.success)],
      ["Error", String(s.error)],
      ["Avg", fmtMs(s.avg_total_ms)],
      ["p50", fmtMs(s.p50_total_ms)],
      ["p95", fmtMs(s.p95_total_ms)]
    ];
    const grid = h(
      "div",
      { class: "grid-2" },
      ...cards.map(
        ([k, v]) => h(
          "div",
          { style: { background: "var(--panel-2)", border: "1px solid var(--border)", padding: "10px 12px", borderRadius: "var(--radius-sm)" } },
          h("div", { class: "muted", style: { fontSize: "11px", textTransform: "uppercase", letterSpacing: "0.5px" } }, k),
          h("div", { style: { fontSize: "18px", fontWeight: "600", marginTop: "2px" } }, v)
        )
      )
    );
    const finish = Object.entries(s.finish_reasons ?? {}).map(
      ([k, v]) => h("li", {}, `${k}: ${v}`)
    );
    card.replaceChildren(
      h(
        "div",
        { class: "card-head" },
        h("h3", {}, "Request stats"),
        h("span", { class: "muted", style: { fontSize: "12px" } }, `window: ${s.window}`)
      ),
      grid,
      finish.length > 0 ? h(
        "div",
        { style: { marginTop: "12px" } },
        h("div", { class: "muted", style: { marginBottom: "6px" } }, "Finish reasons"),
        h("ul", { style: { margin: 0, paddingLeft: "18px" } }, ...finish)
      ) : h("div")
    );
    if (!s.providers || s.providers.length === 0) {
      perProviderCard.replaceChildren(h("h3", {}, "Per provider"), h("p", { class: "muted" }, "no data yet"));
      return;
    }
    const tbody = h("tbody", {});
    for (const p of s.providers) {
      tbody.append(h(
        "tr",
        {},
        h("td", {}, p.provider),
        h("td", {}, String(p.total)),
        h("td", { class: p.error > 0 ? "err" : "" }, `${(p.error_rate * 100).toFixed(1)}%`),
        h("td", {}, fmtMs(p.avg_total_ms)),
        h("td", {}, fmtMs(p.p95_total_ms))
      ));
    }
    perProviderCard.replaceChildren(
      h("div", { class: "card-head" }, h("h3", {}, "Per provider")),
      h(
        "table",
        {},
        h("thead", {}, h(
          "tr",
          {},
          h("th", {}, "Provider"),
          h("th", {}, "Total"),
          h("th", {}, "Err %"),
          h("th", {}, "Avg"),
          h("th", {}, "p95")
        )),
        tbody
      )
    );
  }

  // src/views/setup.ts
  var STEP_KEY = "uwa:setup:lastStep";
  function renderSetup(root, api2) {
    const ctx = { api: api2, clientBaseUrl: `${location.origin}/v1` };
    const banner = h("div", {});
    const stepsRoot = h("div", {});
    const nextBtn = h("button", { class: "btn", type: "button" }, "Next step");
    root.append(
      h("h2", {}, "Setup"),
      h("p", { class: "lead" }, "Get uwa from a fresh install to a working OpenAI-compatible endpoint. Every step verifies itself."),
      banner,
      stepsRoot,
      h(
        "div",
        { style: { marginTop: "20px", display: "flex", gap: "8px" } },
        nextBtn,
        h("button", {
          class: "btn secondary",
          type: "button",
          onclick: () => {
            localStorage.removeItem(STEP_KEY);
            location.reload();
          }
        }, "Reset wizard")
      )
    );
    const steps = [
      {
        id: "health",
        title: "1. Server responds",
        description: "Verify the uwa daemon is up. This is a public endpoint \u2014 no key required.",
        render: async (body, setStatus2) => {
          body.append(h("p", {}, "Calling /healthz\u2026"));
          setStatus2("running");
          try {
            const r = await api2.health();
            if (r.status === "ok") {
              body.replaceChildren(h("p", { class: "ok" }, `\u2713 /healthz: ${r.status}`));
              setStatus2("ok");
            } else {
              body.replaceChildren(h("p", { class: "warn" }, `/healthz: ${r.status}`));
              setStatus2("fail");
            }
          } catch (e) {
            body.replaceChildren(h("div", { class: "banner err" }, `Cannot reach the server: ${String(e)}`));
            setStatus2("fail");
          }
        }
      },
      {
        id: "ready",
        title: "2. Providers loaded",
        description: "uwa must have at least one provider and model alias in the config.",
        render: async (body, setStatus2) => {
          body.append(h("p", {}, "Calling /readyz\u2026"));
          setStatus2("running");
          try {
            const r = await api2.ready();
            if (!r.providers_loaded) {
              body.replaceChildren(
                h(
                  "div",
                  { class: "banner warn" },
                  "No providers are loaded. Add [providers.*] blocks to your uwa.toml and restart the daemon."
                ),
                codeBlock(
                  '# minimal uwa.toml fragment\n\n[model_aliases]\n"gpt-4o" = "chatgpt"\n\n[providers.chatgpt]\nname = "chatgpt"\nurl_patterns = ["https://chatgpt.com/*"]\ncapabilities = { streams = true, tool_calls = true, vision = false }\n[providers.chatgpt.selectors]\ninput = "#prompt-textarea"\nsend_button = "[data-testid=send-button]"\nassistant_message = "[data-message-author-role=assistant]"\n',
                  "uwa.toml"
                )
              );
              setStatus2("fail");
              return;
            }
            body.replaceChildren(
              h("p", { class: "ok" }, `\u2713 ${r.models} model alias${r.models === 1 ? "" : "es"} loaded`)
            );
            setStatus2("ok");
          } catch (e) {
            body.replaceChildren(h("div", { class: "banner err" }, `/readyz failed: ${String(e)}`));
            setStatus2("fail");
          }
        }
      },
      {
        id: "browser",
        title: "3. Browser is connected",
        description: "uwa needs a Chromium you have logged into. Launch it with a remote-debugging port.",
        render: async (body, setStatus2) => {
          setStatus2("running");
          const launchCmd = "# Linux\nchromium --remote-debugging-port=9222 \\\n  --user-data-dir=$HOME/.uwa-chrome --no-first-run\n\n# macOS\n/Applications/Chromium.app/Contents/MacOS/Chromium \\\n  --remote-debugging-port=9222 --user-data-dir=$HOME/.uwa-chrome\n\n# then run uwa with:\nUWA_CHROMIUM_WS=http://127.0.0.1:9222 cargo run -p uwa-bin --release";
          const probe = async () => {
            try {
              const p = await api2.pool();
              if (p.total_tabs === 0) {
                body.replaceChildren(
                  h(
                    "div",
                    { class: "banner warn" },
                    "Connected to Chromium but it exposes 0 tabs. Open at least one tab (about:blank works)."
                  ),
                  codeBlock(launchCmd, "launch chromium"),
                  h("button", { class: "btn secondary", type: "button", onclick: probe }, "Retry")
                );
                setStatus2("fail");
                return;
              }
              body.replaceChildren(
                h("p", { class: "ok" }, `\u2713 ${p.total_tabs} tab${p.total_tabs === 1 ? "" : "s"} visible`),
                h(
                  "ul",
                  { style: { margin: "8px 0 0", paddingLeft: "18px", color: "var(--muted)", fontSize: "12.5px" } },
                  ...p.tabs.slice(0, 5).map((t) => h("li", {}, t.url || "(no url)"))
                )
              );
              setStatus2("ok");
            } catch (e) {
              body.replaceChildren(
                h(
                  "div",
                  { class: "banner err" },
                  `Cannot list tabs: ${String(e)}. Confirm uwa connected to Chromium at launch.`
                ),
                codeBlock(launchCmd, "launch chromium"),
                h("button", { class: "btn secondary", type: "button", onclick: probe }, "Retry")
              );
              setStatus2("fail");
            }
          };
          body.append(h("p", {}, "Probing /api/pool/status\u2026"));
          await probe();
        }
      },
      {
        id: "login",
        title: "4. Logged in",
        description: "Each provider is probed with its input selector. If matched > 0, you're logged in.",
        render: async (body, setStatus2) => {
          setStatus2("running");
          const renderOnce = async () => {
            body.replaceChildren(h("p", {}, "Loading providers and tabs\u2026"));
            let providers;
            let tabs;
            try {
              const provs = await api2.providers();
              const pool = await api2.pool();
              providers = provs;
              tabs = pool.tabs;
            } catch (e) {
              body.replaceChildren(h("div", { class: "banner err" }, `Cannot load provider/tab info: ${String(e)}`));
              setStatus2("fail");
              return;
            }
            const rows = [];
            let anyOk = false;
            for (const [name, p] of Object.entries(providers)) {
              const tab = findTabForProvider(tabs, p);
              const meta = h(
                "div",
                { class: "meta" },
                tab ? tab.url : `no open tab matching ${p.url_patterns.join(", ")}`
              );
              const stateNode = h("span", { class: "state muted" }, "checking\u2026");
              const row = h(
                "div",
                { class: "provider-row" },
                h(
                  "div",
                  {},
                  h("div", { class: "name" }, name),
                  meta
                ),
                stateNode
              );
              rows.push(row);
              if (!tab) {
                stateNode.textContent = "no tab";
                row.classList.add("fail");
                continue;
              }
              const selector = p.selectors.input;
              if (!selector) {
                stateNode.textContent = "no selector configured";
                row.classList.add("fail");
                continue;
              }
              try {
                const r = await api2.selectorTest({ provider: name, selector, tab_id: tab.id });
                if (r.matched > 0) {
                  stateNode.textContent = `\u2713 logged in (${r.matched} match${r.matched === 1 ? "" : "es"})`;
                  stateNode.className = "state ok";
                  row.classList.add("ok");
                  anyOk = true;
                } else {
                  stateNode.textContent = "\u2717 selector matched 0 elements";
                  stateNode.className = "state err";
                  row.classList.add("fail");
                }
              } catch (e) {
                stateNode.textContent = `error: ${String(e)}`;
                stateNode.className = "state err";
                row.classList.add("fail");
              }
            }
            body.replaceChildren(
              h(
                "p",
                { class: "muted", style: { margin: "0 0 12px" } },
                "Open the site in your Chromium, log in manually, then re-check."
              ),
              ...rows,
              h(
                "div",
                { style: { marginTop: "12px", display: "flex", gap: "8px" } },
                h("button", { class: "btn secondary", type: "button", onclick: renderOnce }, "Re-check")
              )
            );
            setStatus2(anyOk ? "ok" : "fail");
          };
          await renderOnce();
        }
      },
      {
        id: "chat",
        title: "5. Live chat test",
        description: "Send a canned message to the first ready provider and verify the reply.",
        render: async (body, setStatus2) => {
          setStatus2("running");
          const output = h("div", { class: "response-box", style: { minHeight: "80px" } }, "Waiting for providers\u2026");
          const goBtn = h("button", { class: "btn", type: "button" }, "Send test message");
          const run = async () => {
            output.textContent = 'Sending "Reply with exactly: pong"\u2026';
            goBtn.disabled = true;
            try {
              const models = await api2.models();
              if (models.data.length === 0) {
                output.textContent = "No models configured.";
                setStatus2("fail");
                return;
              }
              const model = models.data[0].id;
              const resp = await api2.chat({
                model,
                messages: [{ role: "user", content: "Reply with exactly: pong" }]
              });
              const text = resp.choices[0]?.message?.content ?? "(no content)";
              output.replaceChildren(
                h("div", { class: "muted", style: { marginBottom: "6px" } }, `model: ${model} \u2014 finish: ${resp.choices[0]?.finish_reason}`),
                h("div", {}, text)
              );
              setStatus2("ok");
            } catch (e) {
              output.replaceChildren(h("div", { class: "err" }, String(e)));
              setStatus2("fail");
            } finally {
              goBtn.disabled = false;
            }
          };
          goBtn.addEventListener("click", run);
          body.append(h("div", { style: { marginBottom: "10px" } }, goBtn), output);
          await run();
        }
      },
      {
        id: "clients",
        title: "6. Point your clients here",
        description: "Anything that speaks the OpenAI or Anthropic API works. Copy the values below.",
        render: (body, setStatus2) => {
          const key = localStorage.getItem("uwa:apiKey") ?? "";
          body.append(
            h(
              "dl",
              { class: "kv" },
              h("dt", {}, "Base URL"),
              h("dd", {}, `${location.origin}/v1`),
              h("dt", {}, "API key"),
              h("dd", {}, key ? key : "(none \u2014 open the top-right Set key dialog)"),
              h("dt", {}, "Anthropic"),
              h("dd", {}, `${location.origin}/v1/messages`)
            ),
            h("h3", {}, "curl"),
            codeBlock(
              `curl ${location.origin}/v1/chat/completions \\
  -H "Authorization: Bearer ${key || "$UWA_KEY"}" \\
  -H "Content-Type: application/json" \\
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'`,
              "shell"
            ),
            h("h3", {}, "Python (openai>=1.0)"),
            codeBlock(
              `from openai import OpenAI

client = OpenAI(
    base_url="${location.origin}/v1",
    api_key="${key || "uwa"}",
)

r = client.chat.completions.create(
    model="gpt-4o",
    messages=[{"role": "user", "content": "hi"}],
)
print(r.choices[0].message.content)`,
              "python"
            ),
            h("h3", {}, "Cursor / Continue"),
            h(
              "p",
              { class: "muted" },
              "Set the OpenAI base URL to ",
              h("code", {}, `${location.origin}/v1`),
              " and the API key to the value shown above. Restart the editor after changing."
            )
          );
          setStatus2("ok");
        }
      },
      {
        id: "done",
        title: "7. You're done",
        description: "Use the Playground to send messages by hand, or wire up a client above.",
        render: (body, setStatus2) => {
          body.append(
            h("p", {}, "Everything is verified. Quick reference:"),
            h(
              "ul",
              { style: { marginTop: "4px" } },
              h("li", {}, h("a", { href: "#/playground" }, "Playground"), " \u2014 send messages by hand, watch streaming."),
              h("li", {}, h("a", { href: "#/history" }, "History"), " \u2014 every request uwa has served."),
              h("li", {}, h("a", { href: "#/logs" }, "Logs"), " \u2014 live tracing output.")
            )
          );
          setStatus2("ok");
        }
      }
    ];
    let currentIdx = Number(localStorage.getItem(STEP_KEY) ?? "0");
    if (!Number.isFinite(currentIdx) || currentIdx < 0 || currentIdx >= steps.length)
      currentIdx = 0;
    const statuses = steps.map(() => "pending");
    const stepNodes = [];
    const setStatus = (i, s) => {
      statuses[i] = s;
      const el = stepNodes[i];
      if (!el)
        return;
      el.classList.remove("active", "done", "fail");
      if (s === "ok")
        el.classList.add("done");
      else if (s === "fail")
        el.classList.add("fail");
      else if (s === "running" || i === currentIdx)
        el.classList.add("active");
    };
    const activate = async (i) => {
      currentIdx = i;
      localStorage.setItem(STEP_KEY, String(i));
      stepNodes.forEach((_, j) => setStatus(j, statuses[j] ?? "pending"));
      stepNodes[i]?.classList.add("active");
      stepNodes[i]?.scrollIntoView({ behavior: "smooth", block: "center" });
      const body = stepNodes[i]?.querySelector(".step-body-content");
      if (!body)
        return;
      body.replaceChildren();
      await steps[i].render(body, (s) => setStatus(i, s));
    };
    const render = () => {
      stepsRoot.replaceChildren(
        ...steps.map((step, i) => {
          const body = h("div", { class: "step-body-content" });
          const node = h(
            "div",
            { class: `step ${i === currentIdx ? "active" : ""}` },
            h("div", { class: "step-num" }, String(i + 1)),
            h(
              "div",
              { class: "step-body" },
              h("h4", {}, step.title),
              h("p", {}, step.description),
              body
            )
          );
          stepNodes[i] = node;
          return node;
        })
      );
      void activate(currentIdx);
    };
    nextBtn.addEventListener("click", () => {
      if (currentIdx + 1 < steps.length)
        void activate(currentIdx + 1);
    });
    render();
    const updateBanner = async () => {
      try {
        const r = await api2.ready();
        if (!r.providers_loaded) {
          banner.replaceChildren(h(
            "div",
            { class: "banner warn" },
            "No providers loaded \u2014 fix step 2 before the wizard can proceed."
          ));
        } else {
          banner.replaceChildren();
        }
      } catch {
        banner.replaceChildren(h("div", { class: "banner err" }, "Server not reachable."));
      }
    };
    void updateBanner();
    const iv = window.setInterval(() => void updateBanner(), 5e3);
    return () => window.clearInterval(iv);
  }
  function findTabForProvider(tabs, p) {
    for (const t of tabs) {
      if (!t.url)
        continue;
      if (p.url_patterns.some((pat) => urlMatches(pat, t.url)))
        return t;
    }
    return null;
  }

  // src/views/playground.ts
  function renderPlayground(root, api2) {
    const modelSelect = h("select", {});
    const streamToggle = h("input", { type: "checkbox", checked: true });
    const userInput = h("textarea", { rows: 4, placeholder: "Type a message\u2026" });
    const sendBtn = h("button", { class: "btn", type: "button" }, "Send");
    const cancelBtn = h("button", { class: "btn secondary", type: "button", disabled: true }, "Cancel");
    const clearBtn = h("button", { class: "btn secondary", type: "button" }, "Clear");
    const responseBox = h("div", { class: "response-box" }, "Response will appear here\u2026");
    const meta = h("div", { class: "muted", style: { fontSize: "12px", minHeight: "1.5em" } });
    let abort = null;
    const refresh = async () => {
      try {
        const r = await api2.models();
        modelSelect.replaceChildren(...r.data.map((m) => h("option", { value: m.id }, `${m.id} (${m.owned_by})`)));
      } catch (e) {
        toast(`Failed to load models: ${String(e)}`, "err");
      }
    };
    const send = async () => {
      const model = modelSelect.value;
      const text = userInput.value.trim();
      if (!model || !text)
        return;
      responseBox.replaceChildren();
      responseBox.classList.toggle("streaming", streamToggle.checked);
      meta.textContent = "";
      abort = new AbortController();
      sendBtn.disabled = true;
      cancelBtn.disabled = false;
      const messages = [{ role: "user", content: text }];
      const started = performance.now();
      try {
        if (streamToggle.checked) {
          let acc = "";
          const toolCalls = {};
          const toolCallBox = h("div", {});
          responseBox.replaceChildren(h("div", { style: { whiteSpace: "pre-wrap" } }, ""));
          const textNode = responseBox.firstChild;
          await api2.chatStream(
            { model, messages, stream: true },
            {
              signal: abort.signal,
              onDelta: (chunk) => {
                acc += chunk;
                textNode.textContent = acc;
                responseBox.scrollTop = responseBox.scrollHeight;
              },
              onToolCall: (tc) => {
                var _a;
                const slot = toolCalls[_a = tc.index] ?? (toolCalls[_a] = { arguments: "" });
                if (tc.id)
                  slot.id = tc.id;
                if (tc.name)
                  slot.name = tc.name;
                if (tc.arguments)
                  slot.arguments += tc.arguments;
                toolCallBox.replaceChildren(
                  ...Object.entries(toolCalls).map(
                    ([i, c]) => h(
                      "div",
                      { class: "tool-call" },
                      h("div", {}, `#${i} ${c.name ?? "?"} (${c.id ?? "no id"})`),
                      h("div", { style: { marginTop: "4px" } }, c.arguments || "(no args yet)")
                    )
                  )
                );
                if (!toolCallBox.parentElement)
                  responseBox.append(toolCallBox);
              }
            }
          );
        } else {
          const r = await api2.chat({ model, messages });
          const msg = r.choices[0]?.message;
          const nodes = [];
          if (msg?.content)
            nodes.push(h("div", { style: { whiteSpace: "pre-wrap" } }, msg.content));
          if (msg?.tool_calls && msg.tool_calls.length > 0) {
            for (const tc of msg.tool_calls) {
              nodes.push(
                h(
                  "div",
                  { class: "tool-call" },
                  h("div", {}, `${tc.function.name} (${tc.id})`),
                  h("div", { style: { marginTop: "4px" } }, tc.function.arguments)
                )
              );
            }
          }
          if (nodes.length === 0)
            nodes.push(h("div", { class: "muted" }, "(empty response)"));
          responseBox.replaceChildren(...nodes);
        }
        const elapsed = performance.now() - started;
        meta.textContent = `${model} \u2014 ${elapsed.toFixed(0)} ms`;
      } catch (e) {
        if (e instanceof DOMException && e.name === "AbortError") {
          responseBox.append(h("div", { class: "muted" }, "\n(cancelled)"));
        } else if (e instanceof ApiError) {
          responseBox.replaceChildren(
            h("div", { class: "err" }, `${e.status} ${e.code}`),
            h("div", { style: { marginTop: "6px" } }, e.message)
          );
        } else {
          responseBox.replaceChildren(h("div", { class: "err" }, String(e)));
        }
      } finally {
        responseBox.classList.remove("streaming");
        sendBtn.disabled = false;
        cancelBtn.disabled = true;
        abort = null;
      }
    };
    cancelBtn.addEventListener("click", () => abort?.abort());
    clearBtn.addEventListener("click", () => {
      responseBox.replaceChildren(h("div", { class: "muted" }, "Response will appear here\u2026"));
      meta.textContent = "";
    });
    sendBtn.addEventListener("click", () => void send());
    userInput.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        void send();
      }
    });
    root.append(
      h("h2", {}, "Playground"),
      h("p", { class: "lead" }, "Send messages through uwa. Streaming works end-to-end."),
      h(
        "div",
        { class: "playground-grid" },
        h(
          "div",
          {},
          h("div", { class: "field" }, h("label", {}, "Model"), modelSelect),
          h("div", { class: "field" }, h("label", {}, "Message"), userInput),
          h(
            "div",
            { class: "field", style: { flexDirection: "row", alignItems: "center", gap: "8px" } },
            streamToggle,
            h("label", { style: { margin: 0 } }, "stream (SSE)")
          ),
          h("div", { style: { display: "flex", gap: "8px" } }, sendBtn, cancelBtn, clearBtn),
          h(
            "p",
            { class: "muted", style: { fontSize: "12px", marginTop: "6px" } },
            "\u2318/Ctrl + Enter to send"
          )
        ),
        h(
          "div",
          {},
          h(
            "div",
            { class: "card-head" },
            h("h3", {}, "Response"),
            meta
          ),
          responseBox
        )
      )
    );
    void refresh();
    return () => {
      abort?.abort();
    };
  }

  // src/views/history.ts
  function renderHistory(root, api2) {
    const providerFilter = h(
      "select",
      {},
      h("option", { value: "" }, "all providers")
    );
    const statusFilter = h(
      "select",
      {},
      h("option", { value: "" }, "any status"),
      h("option", { value: "success" }, "success"),
      h("option", { value: "error" }, "error"),
      h("option", { value: "pending" }, "pending")
    );
    const refreshBtn = h("button", { class: "btn secondary", type: "button" }, "Refresh");
    const tbody = h("tbody", {});
    const detail = h("div", { style: { marginTop: "12px" } });
    const table = h(
      "table",
      {},
      h("thead", {}, h(
        "tr",
        {},
        h("th", {}, "Time"),
        h("th", {}, "Provider"),
        h("th", {}, "Model"),
        h("th", {}, "Status"),
        h("th", {}, "Total"),
        h("th", {}, "Finish"),
        h("th", {})
      )),
      tbody
    );
    const showDetail = (rec) => {
      detail.replaceChildren(
        h(
          "div",
          { class: "card" },
          h(
            "div",
            { class: "card-head" },
            h("h3", {}, `Record ${rec.id}`),
            h("button", { class: "btn secondary", type: "button", onclick: () => detail.replaceChildren() }, "Close")
          ),
          h(
            "pre",
            { style: { whiteSpace: "pre-wrap", fontFamily: "ui-monospace, monospace", fontSize: "12px" } },
            JSON.stringify(rec, null, 2)
          )
        )
      );
    };
    const load = async () => {
      try {
        const r = await api2.history(100, providerFilter.value || void 0, statusFilter.value || void 0);
        if (r.records.length === 0) {
          tbody.replaceChildren(h("tr", {}, h("td", { colspan: "7", class: "muted" }, "No records yet.")));
          return;
        }
        tbody.replaceChildren(
          ...r.records.map((rec) => rowFor(rec, () => showDetail(rec)))
        );
      } catch (e) {
        toast(`history: ${String(e)}`, "err");
        tbody.replaceChildren(h("tr", {}, h("td", { colspan: "7", class: "err" }, String(e))));
      }
    };
    refreshBtn.addEventListener("click", () => void load());
    providerFilter.addEventListener("change", () => void load());
    statusFilter.addEventListener("change", () => void load());
    root.append(
      h("h2", {}, "History"),
      h("p", { class: "lead" }, "Every request uwa has served. Click a row for details."),
      h(
        "div",
        { style: { display: "flex", gap: "8px", marginBottom: "12px" } },
        providerFilter,
        statusFilter,
        refreshBtn
      ),
      h("div", { class: "card" }, table),
      detail
    );
    void (async () => {
      try {
        const providers = await api2.providers();
        for (const name of Object.keys(providers)) {
          providerFilter.append(h("option", { value: name }, name));
        }
      } catch {
      }
    })();
    void load();
    const iv = window.setInterval(() => void load(), 5e3);
    return () => window.clearInterval(iv);
  }
  function rowFor(rec, onClick) {
    const t = rec.started_at ? new Date(rec.started_at.secs_since_epoch * 1e3).toLocaleTimeString() : "\u2014";
    const finish = rec.response?.finish_reason ?? "\u2014";
    const statusClass = rec.status === "success" ? "ok" : rec.status === "error" ? "err" : "warn";
    return h(
      "tr",
      { style: { cursor: "pointer" }, onclick: onClick },
      h("td", {}, t),
      h("td", {}, rec.provider),
      h("td", {}, rec.model),
      h("td", { class: statusClass }, rec.status),
      h("td", {}, fmtMs(rec.timing.total_ms)),
      h("td", {}, finish),
      h("td", {}, rec.response?.tool_calls ? `tool\xD7${rec.response.tool_calls}` : "")
    );
  }

  // src/views/sessions.ts
  function renderSessions(root, api2) {
    const tbody = h("tbody", {});
    const refreshBtn = h("button", { class: "btn secondary", type: "button" }, "Refresh");
    const recoverBtn = h("button", { class: "btn secondary", type: "button" }, "Recover unhealthy");
    const table = h(
      "table",
      {},
      h("thead", {}, h(
        "tr",
        {},
        h("th", {}, "Conversation"),
        h("th", {}, "Tab"),
        h("th", {}, "Age"),
        h("th", {}, "Idle"),
        h("th", {}, "Gen"),
        h("th", {})
      )),
      tbody
    );
    const load = async () => {
      try {
        const r = await api2.sessions();
        if (r.sessions.length === 0) {
          tbody.replaceChildren(h("tr", {}, h("td", { colspan: "6", class: "muted" }, "No sessions yet.")));
          return;
        }
        tbody.replaceChildren(
          ...r.sessions.map((s) => rowFor2(s, () => void (async () => {
            try {
              await api2.dropSession(s.conversation);
              await load();
            } catch (e) {
              toast(`drop: ${String(e)}`, "err");
            }
          })()))
        );
      } catch (e) {
        toast(`sessions: ${String(e)}`, "err");
      }
    };
    refreshBtn.addEventListener("click", () => void load());
    recoverBtn.addEventListener("click", () => void (async () => {
      try {
        await api2.recoverSessions();
        toast("recover done", "ok");
        await load();
      } catch (e) {
        toast(`recover: ${String(e)}`, "err");
      }
    })());
    root.append(
      h("h2", {}, "Sessions"),
      h("p", { class: "lead" }, "Each conversation is pinned to a tab. Idle sessions are reaped automatically."),
      h("div", { style: { display: "flex", gap: "8px", marginBottom: "12px" } }, refreshBtn, recoverBtn),
      h("div", { class: "card" }, table)
    );
    void load();
    const iv = window.setInterval(() => void load(), 4e3);
    return () => window.clearInterval(iv);
  }
  function rowFor2(s, onDrop) {
    return h(
      "tr",
      {},
      h("td", { class: "mono" }, s.conversation),
      h("td", { class: "mono" }, s.tab),
      h("td", {}, fmtRel(s.age_secs)),
      h("td", {}, fmtRel(s.idle_secs)),
      h("td", {}, String(s.generation)),
      h("td", {}, h("button", { class: "btn secondary", type: "button", onclick: onDrop }, "Drop"))
    );
  }

  // src/views/config.ts
  function renderConfig(root, api2) {
    const body = h("div", { class: "card" }, h("p", { class: "muted" }, "Loading\u2026"));
    const load = async () => {
      try {
        const p = await api2.providers();
        const entries = Object.entries(p);
        if (entries.length === 0) {
          body.replaceChildren(h("p", { class: "muted" }, "No providers configured."));
          return;
        }
        body.replaceChildren(
          ...entries.map(([name, info]) => providerCard(name, info))
        );
      } catch (e) {
        body.replaceChildren(h("div", { class: "banner err" }, String(e)));
      }
    };
    root.append(
      h("h2", {}, "Config"),
      h("p", { class: "lead" }, "Read-only view of the loaded provider config. Edit uwa.toml and restart to change."),
      body
    );
    void load();
    return () => {
    };
  }
  function providerCard(name, info) {
    return h(
      "div",
      { style: { marginBottom: "16px" } },
      h(
        "h3",
        { style: { marginBottom: "6px", color: "var(--text)", textTransform: "none", letterSpacing: 0, fontSize: "15px" } },
        name,
        h(
          "span",
          { class: "muted", style: { marginLeft: "8px", fontSize: "12px", fontWeight: "400" } },
          `${info.extraction} \u2014 ${info.backend ?? "default backend"}`
        )
      ),
      h(
        "dl",
        { class: "kv" },
        h("dt", {}, "URL patterns"),
        h("dd", {}, info.url_patterns.join(", ")),
        h("dt", {}, "input"),
        h("dd", {}, info.selectors.input ?? "\u2014"),
        h("dt", {}, "send_button"),
        h("dd", {}, info.selectors.send_button ?? "\u2014"),
        h("dt", {}, "assistant_message"),
        h("dd", {}, info.selectors.assistant_message ?? "\u2014"),
        h("dt", {}, "capabilities"),
        h(
          "dd",
          {},
          `streams=${info.capabilities.streams} tool_calls=${info.capabilities.tool_calls} vision=${info.capabilities.vision}`
        )
      )
    );
  }

  // src/views/logs.ts
  function renderLogs(root, api2) {
    const stream = h("div", { class: "log-stream" });
    const toggle = h("button", { class: "btn", type: "button" }, "Start stream");
    const clear = h("button", { class: "btn secondary", type: "button" }, "Clear");
    const note = h("span", { class: "muted", style: { fontSize: "12px" } });
    let closeFn = null;
    const start = () => {
      if (closeFn)
        return;
      stream.replaceChildren();
      note.textContent = "connected";
      void getApiKey();
      closeFn = api2.streamLogs((ev) => {
        const e = ev;
        const line = h(
          "div",
          { class: `log-line log-${e.level}` },
          `[${e.level}] ${e.target}: ${e.message}${e.fields ? " " + e.fields : ""}`
        );
        stream.append(line);
        if (stream.childElementCount > 2e3)
          stream.firstChild?.remove();
        stream.scrollTop = stream.scrollHeight;
      });
      toggle.textContent = "Stop stream";
    };
    const stop = () => {
      if (!closeFn)
        return;
      closeFn();
      closeFn = null;
      note.textContent = "stopped";
      toggle.textContent = "Start stream";
    };
    toggle.addEventListener("click", () => closeFn ? stop() : start());
    clear.addEventListener("click", () => stream.replaceChildren());
    root.append(
      h("h2", {}, "Logs"),
      h("p", { class: "lead" }, "Live tracing output from uwa. Streamed over SSE from /admin/logs/stream."),
      h(
        "div",
        { style: { display: "flex", gap: "8px", marginBottom: "8px", alignItems: "center" } },
        toggle,
        clear,
        note
      ),
      stream
    );
    start();
    return () => stop();
  }

  // src/main.ts
  var api = new UwaClient();
  function updateKeyPill() {
    const pill = document.getElementById("key-status");
    if (!pill)
      return;
    const key = getApiKey();
    if (key) {
      pill.className = "pill pill-ok";
      pill.textContent = "key: set";
    } else {
      pill.className = "pill pill-warn";
      pill.textContent = "key: none";
    }
  }
  async function promptForKey() {
    const input = h("input", { type: "password", placeholder: "API key", value: getApiKey() });
    const body = h(
      "div",
      {},
      h(
        "p",
        { class: "muted", style: { marginTop: 0 } },
        "Your key is stored locally in this browser. It's the same value as [server].api_key in uwa.toml."
      ),
      h("div", { class: "field" }, h("label", {}, "API key"), input)
    );
    const r = await dialog({ title: "Set API key", body, primaryLabel: "Save", secondaryLabel: "Clear" });
    if (r === "primary") {
      setApiKey(input.value.trim());
      notifyKeyChange();
    } else if (r === "secondary") {
      setApiKey("");
      notifyKeyChange();
    }
  }
  async function updateServerPill() {
    const pill = document.getElementById("server-status");
    if (!pill)
      return;
    try {
      const r = await api.health();
      if (r.status === "ok") {
        pill.className = "pill pill-ok";
        pill.textContent = "server: ok";
      } else {
        pill.className = "pill pill-warn";
        pill.textContent = `server: ${r.status}`;
      }
    } catch {
      pill.className = "pill pill-err";
      pill.textContent = "server: down";
    }
  }
  var prompted = false;
  window.addEventListener("unhandledrejection", (e) => {
    const reason = e.reason;
    if (reason instanceof ApiError && reason.status === 401 && !prompted) {
      prompted = true;
      void promptForKey().finally(() => {
        prompted = false;
      });
    }
  });
  var routes = [
    { path: "/setup", label: "Setup", render: (r) => renderSetup(r, api) },
    { path: "/", label: "Dashboard", render: (r) => renderDashboard(r, api) },
    { path: "/playground", label: "Playground", render: (r) => renderPlayground(r, api) },
    { path: "/history", label: "History", render: (r) => renderHistory(r, api) },
    { path: "/sessions", label: "Sessions", render: (r) => renderSessions(r, api) },
    { path: "/config", label: "Config", render: (r) => renderConfig(r, api) },
    { path: "/logs", label: "Logs", render: (r) => renderLogs(r, api) }
  ];
  function boot() {
    updateKeyPill();
    void updateServerPill();
    onKeyChange(updateKeyPill);
    document.getElementById("key-btn")?.addEventListener("click", () => void promptForKey());
    const done = localStorage.getItem("uwa:setup:done") === "1";
    const initial = done || location.hash ? void 0 : "/setup";
    const router = new Router(routes, "/");
    if (initial) {
      history.replaceState(null, "", `#${initial}`);
    }
    router.start();
    setInterval(() => void updateServerPill(), 5e3);
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot, { once: true });
  } else {
    boot();
  }
})();
