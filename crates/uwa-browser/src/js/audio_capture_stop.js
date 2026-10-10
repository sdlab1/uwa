(function() {
  const a = window.__uwaAudio;
  if (!a) return { ok: false, error: 'not-started' };
  for (const r of (a.recorders || [])) {
    try { r.stop(); } catch (_) {}
  }
  a.active = false;
  // Note: FileReader flushes on its own tick; chunks recorded before stop
  // are already in. Late chunks land in a.chunks but arrive after we read.
  return {
    ok: true,
    mime: a.mime,
    chunks: a.chunks.slice(),
    error: a.error,
  };
})()