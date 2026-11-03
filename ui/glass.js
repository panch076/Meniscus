// Real refraction for .glass panels: a displacement map bends the backdrop at the
// edges the way a thick lens does. Maps are generated once per panel size and
// shared, then the GPU does the work, so idle panels cost nothing.
(() => {
  const defs = document.getElementById("filters");
  const made = new Map();

  function bezel(w, h, radius, depth) {
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    const img = ctx.createImageData(w, h);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        // Signed distance to the rounded rectangle, and the outward direction.
        const qx = Math.abs(x + 0.5 - w / 2) - (w / 2 - radius);
        const qy = Math.abs(y + 0.5 - h / 2) - (h / 2 - radius);
        const ax = Math.max(qx, 0), ay = Math.max(qy, 0);
        const dist = Math.hypot(ax, ay) + Math.min(Math.max(qx, qy), 0) - radius;
        const t = Math.min(Math.max(-dist / depth, 0), 1);
        // Convex profile: strong at the rim, flat in the middle.
        const bend = Math.pow(1 - t, 3);
        let nx = 0, ny = 0;
        if (qx > 0 || qy > 0) {
          const len = Math.hypot(ax, ay) || 1;
          nx = ax / len; ny = ay / len;
        } else if (qx > qy) { nx = 1; } else { ny = 1; }
        nx *= Math.sign(x + 0.5 - w / 2);
        ny *= Math.sign(y + 0.5 - h / 2);
        const i = (y * w + x) * 4;
        img.data[i] = 128 - nx * bend * 127;
        img.data[i + 1] = 128 - ny * bend * 127;
        img.data[i + 2] = 128;
        img.data[i + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
    return canvas.toDataURL();
  }

  // Filters use exact pixel units: percentage regions do not line up with the
  // backdrop, which shows up as hard-edged bands inside the panel.
  function filterFor(w, h, radius) {
    const key = `${w}x${h}x${radius}`;
    if (made.has(key)) return made.get(key);
    const id = `lens${made.size}`;
    const depth = Math.min(30, Math.min(w, h) / 3);
    // The map is computed at quarter scale: it is smooth, and this keeps generation instant.
    const url = bezel(Math.ceil(w / 4), Math.ceil(h / 4), Math.max(1, radius / 4), depth / 4);
    const ns = "http://www.w3.org/2000/svg";
    const f = document.createElementNS(ns, "filter");
    f.setAttribute("id", id);
    f.setAttribute("filterUnits", "userSpaceOnUse");
    f.setAttribute("primitiveUnits", "userSpaceOnUse");
    f.setAttribute("x", "0"); f.setAttribute("y", "0");
    f.setAttribute("width", w); f.setAttribute("height", h);
    f.setAttribute("color-interpolation-filters", "sRGB");
    f.innerHTML = `
      <feImage href="${url}" preserveAspectRatio="none" x="0" y="0" width="${w}" height="${h}" result="map"/>
      <feDisplacementMap in="SourceGraphic" in2="map" scale="52" xChannelSelector="R" yChannelSelector="G"/>`;
    defs.appendChild(f);
    made.set(key, id);
    return id;
  }

  function apply(el) {
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) return;
    const radius = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 16;
    const id = filterFor(Math.round(r.width), Math.round(r.height), radius);
    el.style.backdropFilter = `url(#${id}) blur(14px) saturate(140%)`;
  }

  // Regenerating a map during a live window resize would stutter, so wait for it to settle.
  const pending = new Set();
  let settle = null;
  const observer = new ResizeObserver((entries) => {
    for (const e of entries) pending.add(e.target);
    clearTimeout(settle);
    settle = setTimeout(() => {
      for (const el of pending) apply(el);
      pending.clear();
    }, 140);
  });
  window.glassify = (root = document) => {
    root.querySelectorAll(".glass").forEach((el) => {
      if (el.dataset.glassed) return;
      el.dataset.glassed = "1";
      observer.observe(el);
    });
  };
  window.glassify();
})();
