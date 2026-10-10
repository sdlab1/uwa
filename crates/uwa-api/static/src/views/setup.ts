import { UwaClient } from "../api";
import type { ProviderInfo, TabInfo } from "../types";
import { codeBlock, h, urlMatches } from "../ui";

interface Ctx {
  api: UwaClient;
  clientBaseUrl: string;
}

interface Step {
  id: string;
  title: string;
  description: string;
  /** Returns null when not done, or a cleanup function. */
  render: (body: HTMLElement, setStatus: (s: StepStatus) => void) => Promise<void> | void;
}

type StepStatus = "pending" | "running" | "ok" | "fail" | "skipped";

const STEP_KEY = "uwa:setup:lastStep";

export function renderSetup(root: HTMLElement, api: UwaClient): () => void {
  const ctx: Ctx = { api, clientBaseUrl: `${location.origin}/v1` };

  const banner = h("div", {});
  const stepsRoot = h("div", {});
  const nextBtn = h("button", { class: "btn", type: "button" }, "Next step");

  root.append(
    h("h2", {}, "Setup"),
    h("p", { class: "lead" }, "Get uwa from a fresh install to a working OpenAI-compatible endpoint. Every step verifies itself."),
    banner,
    stepsRoot,
    h("div", { style: { marginTop: "20px", display: "flex", gap: "8px" } },
      nextBtn,
      h("button", {
        class: "btn secondary",
        type: "button",
        onclick: () => { localStorage.removeItem(STEP_KEY); location.reload(); },
      }, "Reset wizard"),
    ),
  );

  const steps: Step[] = [
    {
      id: "health",
      title: "1. Server responds",
      description: "Verify the uwa daemon is up. This is a public endpoint — no key required.",
      render: async (body, setStatus) => {
        body.append(h("p", {}, "Calling /healthz…"));
        setStatus("running");
        try {
          const r = await api.health();
          if (r.status === "ok") {
            body.replaceChildren(h("p", { class: "ok" }, `✓ /healthz: ${r.status}`));
            setStatus("ok");
          } else {
            body.replaceChildren(h("p", { class: "warn" }, `/healthz: ${r.status}`));
            setStatus("fail");
          }
        } catch (e) {
          body.replaceChildren(h("div", { class: "banner err" }, `Cannot reach the server: ${String(e)}`));
          setStatus("fail");
        }
      },
    },
    {
      id: "ready",
      title: "2. Providers loaded",
      description: "uwa must have at least one provider and model alias in the config.",
      render: async (body, setStatus) => {
        body.append(h("p", {}, "Calling /readyz…"));
        setStatus("running");
        try {
          const r = await api.ready();
          if (!r.providers_loaded) {
            body.replaceChildren(
              h("div", { class: "banner warn" },
                "No providers are loaded. Add [providers.*] blocks to your uwa.toml and restart the daemon."),
              codeBlock(
                "# minimal uwa.toml fragment\n\n" +
                "[model_aliases]\n" +
                '"gpt-4o" = "chatgpt"\n\n' +
                "[providers.chatgpt]\n" +
                'name = "chatgpt"\n' +
                'url_patterns = ["https://chatgpt.com/*"]\n' +
                'capabilities = { streams = true, tool_calls = true, vision = false }\n' +
                "[providers.chatgpt.selectors]\n" +
                'input = "#prompt-textarea"\n' +
                'send_button = "[data-testid=send-button]"\n' +
                'assistant_message = "[data-message-author-role=assistant]"\n',
                "uwa.toml",
              ),
            );
            setStatus("fail");
            return;
          }
          body.replaceChildren(
            h("p", { class: "ok" }, `✓ ${r.models} model alias${r.models === 1 ? "" : "es"} loaded`),
          );
          setStatus("ok");
        } catch (e) {
          body.replaceChildren(h("div", { class: "banner err" }, `/readyz failed: ${String(e)}`));
          setStatus("fail");
        }
      },
    },
    {
      id: "browser",
      title: "3. Browser is connected",
      description: "uwa needs a Chromium you have logged into. Launch it with a remote-debugging port.",
      render: async (body, setStatus) => {
        setStatus("running");
        const launchCmd =
          "# Linux\n" +
          "chromium --remote-debugging-port=9222 \\\n" +
          "  --user-data-dir=$HOME/.uwa-chrome --no-first-run\n\n" +
          "# macOS\n" +
          "/Applications/Chromium.app/Contents/MacOS/Chromium \\\n" +
          "  --remote-debugging-port=9222 --user-data-dir=$HOME/.uwa-chrome\n\n" +
          "# then run uwa with:\n" +
          "UWA_CHROMIUM_WS=http://127.0.0.1:9222 cargo run -p uwa-bin --release";

        const probe = async () => {
          try {
            const p = await api.pool();
            if (p.total_tabs === 0) {
              body.replaceChildren(
                h("div", { class: "banner warn" },
                  "Connected to Chromium but it exposes 0 tabs. Open at least one tab (about:blank works)."),
                codeBlock(launchCmd, "launch chromium"),
                h("button", { class: "btn secondary", type: "button", onclick: probe }, "Retry"),
              );
              setStatus("fail");
              return;
            }
            body.replaceChildren(
              h("p", { class: "ok" }, `✓ ${p.total_tabs} tab${p.total_tabs === 1 ? "" : "s"} visible`),
              h("ul", { style: { margin: "8px 0 0", paddingLeft: "18px", color: "var(--muted)", fontSize: "12.5px" } },
                ...p.tabs.slice(0, 5).map((t) => h("li", {}, t.url || "(no url)")),
              ),
            );
            setStatus("ok");
          } catch (e) {
            body.replaceChildren(
              h("div", { class: "banner err" },
                `Cannot list tabs: ${String(e)}. Confirm uwa connected to Chromium at launch.`),
              codeBlock(launchCmd, "launch chromium"),
              h("button", { class: "btn secondary", type: "button", onclick: probe }, "Retry"),
            );
            setStatus("fail");
          }
        };
        body.append(h("p", {}, "Probing /api/pool/status…"));
        await probe();
      },
    },
    {
      id: "login",
      title: "4. Logged in",
      description: "Each provider is probed with its input selector. If matched > 0, you're logged in.",
      render: async (body, setStatus) => {
        setStatus("running");
        const renderOnce = async () => {
          body.replaceChildren(h("p", {}, "Loading providers and tabs…"));
          let providers: Record<string, ProviderInfo>;
          let tabs: TabInfo[];
          try {
            const provs = await api.providers();
            const pool = await api.pool();
            providers = provs;
            tabs = pool.tabs;
          } catch (e) {
            body.replaceChildren(h("div", { class: "banner err" }, `Cannot load provider/tab info: ${String(e)}`));
            setStatus("fail");
            return;
          }

          const rows: HTMLElement[] = [];
          let anyOk = false;

          for (const [name, p] of Object.entries(providers)) {
            const tab = findTabForProvider(tabs, p);
            const meta = h("div", { class: "meta" },
              tab ? tab.url : `no open tab matching ${p.url_patterns.join(", ")}`);
            const stateNode = h("span", { class: "state muted" }, "checking…");

            const row = h("div", { class: "provider-row" },
              h("div", {},
                h("div", { class: "name" }, name),
                meta,
              ),
              stateNode,
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
              const r = await api.selectorTest({ provider: name, selector, tab_id: tab.id });
              if (r.matched > 0) {
                stateNode.textContent = `✓ logged in (${r.matched} match${r.matched === 1 ? "" : "es"})`;
                stateNode.className = "state ok";
                row.classList.add("ok");
                anyOk = true;
              } else {
                stateNode.textContent = "✗ selector matched 0 elements";
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
            h("p", { class: "muted", style: { margin: "0 0 12px" } },
              "Open the site in your Chromium, log in manually, then re-check."),
            ...rows,
            h("div", { style: { marginTop: "12px", display: "flex", gap: "8px" } },
              h("button", { class: "btn secondary", type: "button", onclick: renderOnce }, "Re-check"),
            ),
          );
          setStatus(anyOk ? "ok" : "fail");
        };
        await renderOnce();
      },
    },
    {
      id: "chat",
      title: "5. Live chat test",
      description: "Send a canned message to the first ready provider and verify the reply.",
      render: async (body, setStatus) => {
        setStatus("running");
        const output = h("div", { class: "response-box", style: { minHeight: "80px" } }, "Waiting for providers…");
        const goBtn = h("button", { class: "btn", type: "button" }, "Send test message");

        const run = async () => {
          output.textContent = "Sending \"Reply with exactly: pong\"…";
          goBtn.disabled = true;
          try {
            const models = await api.models();
            if (models.data.length === 0) {
              output.textContent = "No models configured.";
              setStatus("fail");
              return;
            }
            const model = models.data[0]!.id;
            const resp = await api.chat({
              model,
              messages: [{ role: "user", content: "Reply with exactly: pong" }],
            });
            const text = resp.choices[0]?.message?.content ?? "(no content)";
            output.replaceChildren(
              h("div", { class: "muted", style: { marginBottom: "6px" } }, `model: ${model} — finish: ${resp.choices[0]?.finish_reason}`),
              h("div", {}, text),
            );
            setStatus("ok");
          } catch (e) {
            output.replaceChildren(h("div", { class: "err" }, String(e)));
            setStatus("fail");
          } finally {
            goBtn.disabled = false;
          }
        };

        goBtn.addEventListener("click", run);
        body.append(h("div", { style: { marginBottom: "10px" } }, goBtn), output);
        await run();
      },
    },
    {
      id: "clients",
      title: "6. Point your clients here",
      description: "Anything that speaks the OpenAI or Anthropic API works. Copy the values below.",
      render: (body, setStatus) => {
        const key = localStorage.getItem("uwa:apiKey") ?? "";
        body.append(
          h("dl", { class: "kv" },
            h("dt", {}, "Base URL"), h("dd", {}, `${location.origin}/v1`),
            h("dt", {}, "API key"), h("dd", {}, key ? key : "(none — open the top-right Set key dialog)"),
            h("dt", {}, "Anthropic"), h("dd", {}, `${location.origin}/v1/messages`),
          ),
          h("h3", {}, "curl"),
          codeBlock(
            `curl ${location.origin}/v1/chat/completions \\\n` +
            `  -H "Authorization: Bearer ${key || "$UWA_KEY"}" \\\n` +
            `  -H "Content-Type: application/json" \\\n` +
            `  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'`,
            "shell",
          ),
          h("h3", {}, "Python (openai>=1.0)"),
          codeBlock(
            `from openai import OpenAI\n\n` +
            `client = OpenAI(\n` +
            `    base_url="${location.origin}/v1",\n` +
            `    api_key="${key || "uwa"}",\n` +
            `)\n\n` +
            `r = client.chat.completions.create(\n` +
            `    model="gpt-4o",\n` +
            `    messages=[{"role": "user", "content": "hi"}],\n` +
            `)\n` +
            `print(r.choices[0].message.content)`,
            "python",
          ),
          h("h3", {}, "Cursor / Continue"),
          h("p", { class: "muted" },
            "Set the OpenAI base URL to ", h("code", {}, `${location.origin}/v1`),
            " and the API key to the value shown above. Restart the editor after changing."),
        );
        setStatus("ok");
      },
    },
    {
      id: "done",
      title: "7. You're done",
      description: "Use the Playground to send messages by hand, or wire up a client above.",
      render: (body, setStatus) => {
        body.append(
          h("p", {}, "Everything is verified. Quick reference:"),
          h("ul", { style: { marginTop: "4px" } },
            h("li", {}, h("a", { href: "#/playground" }, "Playground"), " — send messages by hand, watch streaming."),
            h("li", {}, h("a", { href: "#/history" }, "History"), " — every request uwa has served."),
            h("li", {}, h("a", { href: "#/logs" }, "Logs"), " — live tracing output."),
          ),
        );
        setStatus("ok");
      },
    },
  ];

  // ---------- runner ----------

  let currentIdx = Number(localStorage.getItem(STEP_KEY) ?? "0");
  if (!Number.isFinite(currentIdx) || currentIdx < 0 || currentIdx >= steps.length) currentIdx = 0;
  const statuses: StepStatus[] = steps.map(() => "pending");

  const stepNodes: HTMLElement[] = [];

  const setStatus = (i: number, s: StepStatus) => {
    statuses[i] = s;
    const el = stepNodes[i];
    if (!el) return;
    el.classList.remove("active", "done", "fail");
    if (s === "ok") el.classList.add("done");
    else if (s === "fail") el.classList.add("fail");
    else if (s === "running" || i === currentIdx) el.classList.add("active");
  };

  const activate = async (i: number) => {
    currentIdx = i;
    localStorage.setItem(STEP_KEY, String(i));
    stepNodes.forEach((_, j) => setStatus(j, statuses[j] ?? "pending"));
    stepNodes[i]?.classList.add("active");
    stepNodes[i]?.scrollIntoView({ behavior: "smooth", block: "center" });
    const body = stepNodes[i]?.querySelector<HTMLElement>(".step-body-content");
    if (!body) return;
    body.replaceChildren();
    await steps[i]!.render(body, (s) => setStatus(i, s));
  };

  const render = () => {
    stepsRoot.replaceChildren(
      ...steps.map((step, i) => {
        const body = h("div", { class: "step-body-content" });
        const node = h("div", { class: `step ${i === currentIdx ? "active" : ""}` },
          h("div", { class: "step-num" }, String(i + 1)),
          h("div", { class: "step-body" },
            h("h4", {}, step.title),
            h("p", {}, step.description),
            body,
          ),
        );
        stepNodes[i] = node;
        return node;
      }),
    );
    void activate(currentIdx);
  };

  nextBtn.addEventListener("click", () => {
    if (currentIdx + 1 < steps.length) void activate(currentIdx + 1);
  });

  render();

  // Banner: state summary
  const updateBanner = async () => {
    try {
      const r = await api.ready();
      if (!r.providers_loaded) {
        banner.replaceChildren(h("div", { class: "banner warn" },
          "No providers loaded — fix step 2 before the wizard can proceed."));
      } else {
        banner.replaceChildren();
      }
    } catch {
      banner.replaceChildren(h("div", { class: "banner err" }, "Server not reachable."));
    }
  };
  void updateBanner();
  const iv = window.setInterval(() => void updateBanner(), 5000);
  void ctx;
  return () => window.clearInterval(iv);
}

function findTabForProvider(tabs: TabInfo[], p: ProviderInfo): TabInfo | null {
  for (const t of tabs) {
    if (!t.url) continue;
    if (p.url_patterns.some((pat) => urlMatches(pat, t.url))) return t;
  }
  return null;
}
