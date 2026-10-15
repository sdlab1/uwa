// Collect media elements that belong to the assistant's last reply.
//
// Runs inside the page. The Rust caller passes the container selector
// via string interpolation at call time — see `media_collect_js()`.
//
// Two modes per asset:
//   * `src` is a URL the browser can fetch → we return the URL verbatim,
//     Rust decides whether to download or save the URL.
//   * `src` is a `canvas` element → we materialise `toDataURL()` and
//     return the `data:` URI. Canvas has no other identity.
//
// `blob:` URLs are returned as-is; they're only fetchable from inside
// the same page, so the Rust side asks the page to fetch bytes with a
// second call when it wants the file. That call lives in
// `media_capture::fetch_blob` and already exists.

(function (selector, maxAssets, maxBytes) {
    const root = selector ? document.querySelector(selector) : document;
    if (!root) return [];

    const out = [];
    const push = (a) => {
        if (out.length >= maxAssets) return false;
        out.push(a);
        return out.length < maxAssets;
    };

    // --- <img> -------------------------------------------------------
    // Filter out decorative chrome: tracking pixels (size 1x1), inline
    // data: image/svg+xml used for icons, and elements with `role=presentation`.
    for (const img of root.querySelectorAll('img')) {
        if (out.length >= maxAssets) break;
        const rect = img.getBoundingClientRect();
        if (rect.width < 24 || rect.height < 24) continue;
        if (img.getAttribute('role') === 'presentation') continue;
        const src = img.currentSrc || img.src || '';
        if (!src) continue;
        // Skip inline SVG data URIs — they're almost always icons.
        if (src.startsWith('data:image/svg')) continue;
        if (src.startsWith('data:') && src.length > maxBytes * 1.4) continue;
        push({
            kind: 'image',
            url: src,
            mime: null,
            size_bytes: null,
            alt: img.alt || null,
            source: 'img',
        });
    }

    // --- <video> -----------------------------------------------------
    for (const v of root.querySelectorAll('video')) {
        if (out.length >= maxAssets) break;
        const src = v.currentSrc || v.src || '';
        if (src) {
            push({
                kind: 'video',
                url: src,
                mime: null,
                size_bytes: null,
                alt: v.title || null,
                source: 'video',
            });
            continue;
        }
        // Fall back to the first <source> child.
        const s = v.querySelector('source');
        if (s && s.src) {
            push({
                kind: 'video',
                url: s.src,
                mime: s.type || null,
                size_bytes: null,
                alt: v.title || null,
                source: 'video-source',
            });
        }
    }

    // --- <audio> -----------------------------------------------------
    for (const a of root.querySelectorAll('audio')) {
        if (out.length >= maxAssets) break;
        const src = a.currentSrc || a.src || '';
        if (src) {
            push({
                kind: 'audio',
                url: src,
                mime: null,
                size_bytes: null,
                alt: a.title || null,
                source: 'audio',
            });
            continue;
        }
        const s = a.querySelector('source');
        if (s && s.src) {
            push({
                kind: 'audio',
                url: s.src,
                mime: s.type || null,
                size_bytes: null,
                alt: a.title || null,
                source: 'audio-source',
            });
        }
    }

    // --- <canvas> ----------------------------------------------------
    // Canvas has no `src`; we snapshot it. The toDataURL call is
    // expensive for large canvases, so we cap by pixel count.
    for (const c of root.querySelectorAll('canvas')) {
        if (out.length >= maxAssets) break;
        const w = c.width || 0, h = c.height || 0;
        if (w * h < 4096) continue; // ignore tiny tracking canvases
        if (w * h > 4_000_000) continue; // > ~2000x2000, skip
        let data;
        try { data = c.toDataURL('image/png'); } catch (_) { continue; }
        if (!data || data.length < 128) continue;
        push({
            kind: 'canvas',
            url: data,
            mime: 'image/png',
            size_bytes: null,
            alt: c.getAttribute('aria-label') || null,
            source: 'canvas',
        });
    }

    // --- background-image on a div -----------------------------------
    // Some sites put generated images as a background-image data URI on
    // an empty div. We catch only data: URIs — a random `url(...)` in a
    // stylesheet is not a chat asset.
    for (const el of root.querySelectorAll('div[style*="background-image"]')) {
        if (out.length >= maxAssets) break;
        const bg = getComputedStyle(el).backgroundImage || '';
        const m = bg.match(/^url\(["']?(data:image\/[^"')]+)["']?\)$/);
        if (!m) continue;
        const data = m[1];
        if (data.length > maxBytes * 1.4) continue;
        push({
            kind: 'image',
            url: data,
            mime: null,
            size_bytes: null,
            alt: el.getAttribute('aria-label') || null,
            source: 'background',
        });
    }

    return out;
})
