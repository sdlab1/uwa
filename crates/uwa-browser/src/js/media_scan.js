(function() {
  const out = [];
  for (const el of document.querySelectorAll('audio, video')) {
    const kind = el.tagName.toLowerCase();
    const src = el.currentSrc || el.src || (el.querySelector('source') ? el.querySelector('source').src : '') || '';
    if (!src) continue;
    out.push({
      kind,
      src,
      mime: el.type || null,
      duration: isFinite(el.duration) ? el.duration : null,
    });
  }
  return out;
})()