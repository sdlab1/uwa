// Injected by `uwa-browser` to hook audio playback.
// Requires `--autoplay-policy=no-user-gesture-required` in Chrome args.
(function() {
  if (window.__uwaAudio && window.__uwaAudio.active) {
    return { ok: true, already_running: true };
  }
  window.__uwaAudio = {
    active: true,
    chunks: [],
    mime: 'audio/webm',
    error: null,
    recorders: [],
  };

  const origPlay = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function() {
    const el = this;
    if (!el.__uwaHooked) {
      el.__uwaHooked = true;
      try {
        const ctx = new (window.AudioContext || window.webkitAudioContext)();
        const src = ctx.createMediaElementSource(el);
        const dest = ctx.createMediaStreamDestination();
        // Route to both: user hears it, we capture it.
        src.connect(ctx.destination);
        src.connect(dest);
        const rec = new MediaRecorder(dest.stream, { mimeType: 'audio/webm' });
        rec.ondataavailable = (e) => {
          if (!e.data || e.data.size === 0) return;
          const reader = new FileReader();
          reader.onload = () => {
            const s = String(reader.result || '');
            const comma = s.indexOf(',');
            if (comma > 0) {
              window.__uwaAudio.chunks.push(s.slice(comma + 1));
              window.__uwaAudio.mime = e.data.type || 'audio/webm';
            }
          };
          reader.readAsDataURL(e.data);
        };
        rec.start(500);
        window.__uwaAudio.recorders.push(rec);
      } catch (e) {
        window.__uwaAudio.error = String(e);
      }
    }
    return origPlay.apply(this, arguments);
  };

  return { ok: true };
})()