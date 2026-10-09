// Runs inside the browser. Returns a JSON object with three arrays of
// candidates: inputs, sendButtons, assistantContainers.
//
// Design goals:
// * No hard-coded site selectors — everything is heuristic.
// * Score in [0,1], evidence string explains the score.
// * Deterministic: same DOM -> same output.
(function() {
  const out = { inputs: [], sendButtons: [], assistantContainers: [] };

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return "";
    if (el.id) return "#" + CSS.escape(el.id);
    for (const attr of ["data-testid", "data-role", "data-message-author-role", "role"]) {
      const v = el.getAttribute(attr);
      if (v) return el.tagName.toLowerCase() + "[" + attr + "=\"" + CSS.escape(v) + "\"]";
    }
    const parts = [];
    let cur = el;
    let depth = 0;
    while (cur && cur.nodeType === 1 && depth < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.id) { parts.unshift("#" + CSS.escape(cur.id)); break; }
      let attrPart = false;
      for (const attr of ["data-testid", "data-role", "data-message-author-role"]) {
        const v = cur.getAttribute(attr);
        if (v) {
          part += "[" + attr + "=\"" + CSS.escape(v) + "\"]";
          attrPart = true;
          break;
        }
      }
      if (!attrPart) {
        const cls = (cur.className && typeof cur.className === "string")
          ? cur.className.split(/\s+/).filter(Boolean).slice(0, 2).map(c => "." + CSS.escape(c)).join("")
          : "";
        part += cls;
      }
      parts.unshift(part);
      cur = cur.parentElement;
      depth++;
    }
    return parts.join(" > ");
  }

  function visible(el) {
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) return false;
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden" || st.opacity === "0") return false;
    return true;
  }

  // ---------- inputs ----------
  const inputCandidates = [];
  document.querySelectorAll(
    "textarea, input[type=text], input:not([type]), div[contenteditable=true], [role=textbox]"
  ).forEach(el => {
    if (!visible(el)) return;
    const r = el.getBoundingClientRect();
    const area = r.width * r.height;
    let score = 0.3;
    const ev = [];

    if (area > 30000) { score += 0.3; ev.push("large area"); }
    else if (area > 10000) { score += 0.15; ev.push("medium area"); }

    const ph = (el.placeholder || el.getAttribute("aria-label") || "").toLowerCase();
    if (/(message|ask|prompt|\u8f93\u5165|\u95ee|send a message)/.test(ph)) {
      score += 0.25; ev.push("placeholder/aria keyword");
    }

    if (el.tagName === "TEXTAREA") { score += 0.1; ev.push("textarea"); }
    if (el.isContentEditable) { score += 0.1; ev.push("contenteditable"); }
    if (el.readOnly || el.disabled) score -= 0.5;

    inputCandidates.push({
      selector: cssPath(el),
      evidence: ev.join(", ") || "heuristic match",
      score: Math.max(0, Math.min(1, score)),
      tag: el.tagName.toLowerCase(),
    });
  });

  // ---------- send buttons ----------
  inputCandidates.sort((a, b) => b.score - a.score);
  const hasStrongInput = inputCandidates.length > 0 && inputCandidates[0].score >= 0.5;
  document.querySelectorAll("button, [role=button], input[type=submit]").forEach(el => {
    if (!visible(el)) return;
    const label = ((el.innerText || el.value || el.getAttribute("aria-label") || "") + "").toLowerCase();
    const dt = (el.getAttribute("data-testid") || "").toLowerCase();
    let score = 0.2;
    const ev = [];

    if (/(send|submit|\u53d1\u9001|\u9001\u4fe1)/.test(label) || /(send|submit)/.test(dt)) {
      score += 0.5; ev.push("label keyword");
    }
    if (label.includes("stop")) score -= 0.5;
    if (el.type === "submit") { score += 0.2; ev.push("submit input"); }
    if (hasStrongInput) { score += 0.05; ev.push("near input area"); }

    out.sendButtons.push({
      selector: cssPath(el),
      evidence: ev.join(", ") || "label: " + label.slice(0, 30),
      score: Math.max(0, Math.min(1, score)),
      tag: el.tagName.toLowerCase(),
    });
  });

  // ---------- assistant containers ----------
  document.querySelectorAll(
    "[data-message-author-role], [data-role], [data-testid], model-response, [class*=assistant], [class*=response], [class*=markdown]"
  ).forEach(el => {
    if (!visible(el)) return;
    const role = el.getAttribute("data-message-author-role") || el.getAttribute("data-role") || "";
    const testid = el.getAttribute("data-testid") || "";
    const cls = (typeof el.className === "string" ? el.className : "") + "";
    let score = 0;
    const ev = [];

    if (/assistant|model|bot|response/i.test(role)) { score += 0.7; ev.push("role=" + role); }
    if (/assistant|response|model/i.test(testid)) { score += 0.5; ev.push("testid=" + testid); }
    if (/assistant|response|markdown/i.test(cls)) { score += 0.25; ev.push("class keyword"); }
    if (el.tagName === "MODEL-RESPONSE") { score += 0.6; ev.push("custom element"); }

    if (score >= 0.4) {
      out.assistantContainers.push({
        selector: cssPath(el),
        evidence: ev.join(", "),
        score: Math.min(1, score),
        tag: el.tagName.toLowerCase(),
      });
    }
  });

  // Dedup by selector, keep highest score.
  const dedup = (arr) => {
    const m = new Map();
    for (const c of arr) {
      if (!c.selector) continue;
      if (!m.has(c.selector) || m.get(c.selector).score < c.score) m.set(c.selector, c);
    }
    return [...m.values()];
  };

  out.inputs = dedup(inputCandidates).slice(0, 5);
  out.sendButtons = dedup(out.sendButtons).slice(0, 5);
  out.assistantContainers = dedup(out.assistantContainers).slice(0, 5);

  return out;
})()