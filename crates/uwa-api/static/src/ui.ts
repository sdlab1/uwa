// Tiny DOM helpers. No framework, no reactivity — just builders.

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, unknown> = {},
  ...children: (Node | string | null | undefined | false)[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = String(v);
    else if (k === "text") el.textContent = String(v);
    else if (k === "html") el.innerHTML = String(v);
    else if (k.startsWith("on") && typeof v === "function") {
      el.addEventListener(k.slice(2).toLowerCase(), v as EventListener);
    } else if (k === "style" && typeof v === "object" && v !== null) {
      Object.assign(el.style, v as Partial<CSSStyleDeclaration>);
    } else {
      el.setAttribute(k, String(v));
    }
  }
  for (const c of children) {
    if (c == null || c === false) continue;
    el.append(typeof c === "string" ? document.createTextNode(c) : c);
  }
  return el;
}

export function clear(node: Element): void {
  while (node.firstChild) node.removeChild(node.firstChild);
}

export function copyButton(text: string): HTMLButtonElement {
  return h(
    "button",
    {
      class: "copy",
      type: "button",
      onclick: async (e: Event) => {
        const btn = e.currentTarget as HTMLButtonElement;
        try {
          await navigator.clipboard.writeText(text);
          const prev = btn.textContent;
          btn.textContent = "copied";
          setTimeout(() => (btn.textContent = prev), 1200);
        } catch {
          btn.textContent = "failed";
          setTimeout(() => (btn.textContent = "copy"), 1200);
        }
      },
    },
    "copy",
  );
}

export function codeBlock(code: string, lang?: string): HTMLElement {
  return h(
    "div",
    { class: "code-block" },
    copyButton(code),
    h("pre", {}, lang ? `# ${lang}\n${code}` : code),
  );
}

export function toast(msg: string, kind: "info" | "ok" | "warn" | "err" = "info", ms = 4000): void {
  const root = document.getElementById("toast-root");
  if (!root) return;
  const t = h("div", { class: `toast ${kind}` }, msg);
  root.append(t);
  setTimeout(() => t.remove(), ms);
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1024 / 1024).toFixed(2)} MiB`;
}

export function fmtMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export function fmtRel(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

export function providerFromUrl(url: string): string | null {
  // Cheap heuristic used by the wizard to map a tab URL to a provider name.
  // The authoritative source is `/v1/provider/status` url_patterns; the
  // wizard applies those patterns itself. This is only a fallback label.
  try {
    const u = new URL(url);
    return u.hostname;
  } catch {
    return null;
  }
}

export function urlMatches(pattern: string, url: string): boolean {
  const star = pattern.indexOf("*");
  if (star < 0) return pattern === url;
  const head = pattern.slice(0, star);
  const tail = pattern.slice(star + 1);
  return url.startsWith(head) && (tail === "" || url.endsWith(tail));
}

/** Open a native <dialog> and resolve when the user closes it. */
export function dialog(opts: {
  title: string;
  body: Node;
  primaryLabel?: string;
  secondaryLabel?: string;
}): Promise<"primary" | "secondary" | "closed"> {
  return new Promise((resolve) => {
    const d = h("dialog", {}) as HTMLDialogElement;
    let result: "primary" | "secondary" | "closed" = "closed";
    const primary = h(
      "button",
      {
        class: "btn",
        type: "button",
        onclick: () => {
          result = "primary";
          d.close();
        },
      },
      opts.primaryLabel ?? "OK",
    );
    const secondary = opts.secondaryLabel
      ? h(
          "button",
          {
            class: "btn secondary",
            type: "button",
            onclick: () => {
              result = "secondary";
              d.close();
            },
          },
          opts.secondaryLabel,
        )
      : null;
    d.append(
      h("h3", {}, opts.title),
      opts.body,
      h("div", { class: "actions" }, secondary, primary),
    );
    d.addEventListener("close", () => {
      d.remove();
      resolve(result);
    });
    document.body.append(d);
    d.showModal();
  });
}
