// Idempotent recorder installer. Safe to inject on every navigation; it
// short-circuits if the current document already carries a live recorder.
//
// State lives in `sessionStorage` so events survive a navigation within
// the same origin. The Rust poller reads events incrementally by `seq`,
// which is why every event carries a monotonic sequence number that
// outlives the document that produced it.
(function install() {
  if (window.__uwaRecorder && window.__uwaRecorder.version === 1) {
    sessionStorage.setItem('__uwa_rec_src', location.href);
    return { ok: true, installed: false };
  }

  const SEQ_KEY = '__uwa_rec_seq';
  const EVT_KEY = '__uwa_rec_events';
  const SRC_KEY = '__uwa_rec_src';
  const MAX_EVENTS = 1500;

  function cssPath(el) {
    if (!el || el.nodeType !== 1) return '';
    if (el.id) return '#' + CSS.escape(el.id);
    for (const attr of ['data-testid', 'data-testid-id', 'data-role', 'data-message-author-role', 'name']) {
      const v = el.getAttribute && el.getAttribute(attr);
      if (v) return el.tagName.toLowerCase() + '[' + attr + '="' + CSS.escape(v) + '"]';
    }
    const parts = [];
    let cur = el;
    let depth = 0;
    while (cur && cur.nodeType === 1 && depth < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.id) { parts.unshift('#' + CSS.escape(cur.id)); break; }
      let tagged = false;
      for (const attr of ['data-testid', 'data-role', 'data-message-author-role']) {
        const v = cur.getAttribute && cur.getAttribute(attr);
        if (v) { part += '[' + attr + '="' + CSS.escape(v) + '"]'; tagged = true; break; }
      }
      if (!tagged && typeof cur.className === 'string' && cur.className) {
        const cls = cur.className.split(/\s+/).filter(Boolean).slice(0, 2)
          .map((c) => '.' + CSS.escape(c)).join('');
        part += cls;
      }
      parts.unshift(part);
      cur = cur.parentElement;
      depth++;
    }
    return parts.join(' > ');
  }

  function nextSeq() {
    const n = Number(sessionStorage.getItem(SEQ_KEY) || '0') + 1;
    sessionStorage.setItem(SEQ_KEY, String(n));
    return n;
  }
  function loadEvents() {
    try { return JSON.parse(sessionStorage.getItem(EVT_KEY) || '[]'); }
    catch (_) { return []; }
  }
  function saveEvents(arr) {
    if (arr.length > MAX_EVENTS) arr = arr.slice(arr.length - MAX_EVENTS);
    try { sessionStorage.setItem(EVT_KEY, JSON.stringify(arr)); } catch (_) {}
  }
  function push(ev) {
    ev.seq = nextSeq();
    ev.at_ms = Date.now();
    ev.src = sessionStorage.getItem(SRC_KEY) || '';
    const arr = loadEvents();
    arr.push(ev);
    saveEvents(arr);
  }

  sessionStorage.setItem(SRC_KEY, location.href);

  document.addEventListener('click', function (e) {
    const el = (e.target && e.target.closest)
      ? (e.target.closest('button, [role=button], a, input[type=submit], [data-testid]') || e.target)
      : e.target;
    push({ kind: 'click', selector: cssPath(el) });
  }, true);

  document.addEventListener('input', function (e) {
    const el = e.target;
    if (!el) return;
    const tag = el.tagName;
    if (tag !== 'INPUT' && tag !== 'TEXTAREA' && !el.isContentEditable) return;
    const text = (tag === 'INPUT' || tag === 'TEXTAREA')
      ? (el.value == null ? '' : String(el.value))
      : (el.innerText == null ? '' : String(el.innerText));
    push({ kind: 'input', selector: cssPath(el), text: text.slice(0, 4000) });
  }, true);

  document.addEventListener('keydown', function (e) {
    if (e.key !== 'Enter') return;
    const el = e.target;
    push({ kind: 'keypress', key: 'Enter', selector: el ? cssPath(el) : '' });
  }, true);

  document.addEventListener('submit', function (e) {
    push({ kind: 'submit', selector: e.target ? cssPath(e.target) : '' });
  }, true);

  window.__uwaRecorder = {
    version: 1,
    events_since(seq) {
      const arr = loadEvents();
      const out = [];
      for (let i = 0; i < arr.length; i++) {
        if (arr[i].seq > seq) out.push(arr[i]);
      }
      return out;
    },
    reset() {
      sessionStorage.removeItem(SEQ_KEY);
      sessionStorage.removeItem(EVT_KEY);
    },
  };
  return { ok: true, installed: true };
})()
