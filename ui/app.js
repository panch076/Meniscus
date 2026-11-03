const { invoke } = window.__TAURI__.core;
const { open: openDialog, save: saveDialog } = window.__TAURI__.dialog;
const appWindow = window.__TAURI__.window.getCurrentWindow();
const $ = (id) => document.getElementById(id);
const el = (tag, props = {}, ...kids) => {
  const n = Object.assign(document.createElement(tag), props);
  n.append(...kids);
  return n;
};

const state = {
  profiles: [], settings: null, current: null, selected: null,
  sensors: [], fonts: [], monitors: [], status: null, page: "studio",
};

const WIDGET_KINDS = {
  sensor: "Sensor value", ring: "Ring gauge", bar: "Bar gauge", spark: "History graph",
  clock: "Digital clock", analog: "Analog clock", date: "Date", text: "Text", image: "Image",
  lyrics: "Synced lyrics", albumart: "Album art", track: "Song & artist", progress: "Song progress",
};
const BG_KINDS = {
  liquid: "Liquid", color: "Solid", gradient: "Gradient", image: "Image", gif: "GIF", video: "Video",
  slideshow: "Slideshow", screen: "Mirror", visualizer: "Audio", albumart: "Album art",
};
const WIDGET_DEFAULTS = {
  sensor: { size: .14, sensor: "mahm:CPU temperature", decimals: 0 },
  ring: { w: .42, h: .42, size: .09, thickness: .035, sensor: "mahm:CPU temperature" },
  bar: { w: .7, h: .025, size: .04, sensor: "sys:cpu_load", color2: "#ffffffcc" },
  spark: { w: .8, h: .2, size: .04, thickness: .006, sensor: "sys:cpu_load", color2: "#ffffffcc" },
  clock: { size: .16, font: "Segoe UI Light" },
  analog: { w: .8, h: .8, color2: "#ff6b4a" },
  date: { size: .05 },
  text: { size: .08, text: "Hello" },
  image: { w: .4, h: .4 },
  lyrics: { w: .92, h: .7, size: .074, font: "Segoe UI Bold", color2: "#ffffff52", shadow: false },
  albumart: { w: .4, h: .4 },
  track: { w: .7, h: .12, size: .044, font: "Segoe UI Bold", color2: "#ffffff99", align: "left", shadow: false },
  progress: { w: .7, h: .012, size: .026, color: "#ffffffbf", trackColor: "#ffffff33", color2: "#ffffff8c", font: "Segoe UI Semibold", shadow: false },
};

function toast(message, ms = 3200) {
  const t = $("toast");
  t.textContent = message;
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), ms);
}
const report = (e) => toast(String(e));

// ---------- persistence: every edit applies live; disk writes are debounced ----------
let saveTimer = null;
function touch(rerender = true) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(flushProfile, 90);
  if (rerender) renderHandles();
}
function flushProfile() {
  saveTimer = null;
  if (state.current) invoke("save_profile", { profile: state.current }).catch(report);
}
const saveSettings = () => invoke("save_settings", { settings: state.settings }).catch(report);

// ---------- field builders ----------
function field(label, control) {
  return el("label", { className: "field" }, label, control);
}
function text(label, obj, key, after) {
  const i = el("input", { type: "text", value: obj[key] ?? "", spellcheck: false });
  i.oninput = () => { obj[key] = i.value; touch(); after?.(); };
  return field(label, i);
}
function num(label, obj, key, { min, max, step = "any", scale = 1 } = {}) {
  const i = el("input", { type: "number", value: +((obj[key] ?? 0) * scale).toFixed(3), step });
  if (min !== undefined) i.min = min;
  if (max !== undefined) i.max = max;
  i.oninput = () => {
    if (i.value === "" || isNaN(+i.value)) return;
    obj[key] = +i.value / scale;
    touch();
  };
  return field(label, i);
}
function range(label, obj, key, min, max, step = .01) {
  const i = el("input", { type: "range", min, max, step, value: obj[key] ?? min });
  i.oninput = () => { obj[key] = +i.value; touch(); };
  return field(label, i);
}
function hex8(c) {
  const h = (c || "#000000").replace("#", "");
  return { rgb: `#${h.slice(0, 6).padEnd(6, "0")}`, alpha: h.length === 8 ? h.slice(6) : "" };
}
function color(label, obj, key) {
  const { rgb } = hex8(obj[key]);
  const i = el("input", { type: "color", value: rgb });
  // The picker has no alpha channel, so an existing alpha suffix is preserved.
  i.oninput = () => { obj[key] = i.value + hex8(obj[key]).alpha; touch(); };
  return field(label, i);
}
function choice(label, obj, key, options) {
  const s = el("select");
  const entries = Array.isArray(options) ? options.map((o) => [o, o]) : Object.entries(options);
  for (const [v, t] of entries) s.append(el("option", { value: v, textContent: t }));
  s.value = obj[key];
  s.onchange = () => { obj[key] = s.value; touch(); };
  return field(label, s);
}
function toggle(label, obj, key) {
  const i = el("input", { type: "checkbox", checked: !!obj[key] });
  i.onchange = () => { obj[key] = i.checked; touch(); };
  return el("label", { className: "check" }, i, label);
}
const two = (...kids) => el("div", { className: "two" }, ...kids);

function sensorOptions(select, value) {
  select.replaceChildren();
  const groups = new Map();
  for (const r of state.sensors) {
    if (!groups.has(r.group)) groups.set(r.group, []);
    groups.get(r.group).push(r);
  }
  for (const [g, rows] of groups) {
    const og = el("optgroup", { label: g });
    for (const r of rows) og.append(el("option", { value: r.id, textContent: `${r.name}${r.unit ? ` (${r.unit})` : ""}` }));
    select.append(og);
  }
  if (value && !state.sensors.some((r) => r.id === value)) {
    select.append(el("option", { value, textContent: `${value} (not available)` }));
  }
  select.value = value;
}
function sensorPicker(obj, key, after) {
  const s = el("select");
  sensorOptions(s, obj[key]);
  s.onchange = () => { obj[key] = s.value; touch(); after?.(); };
  return field("Sensor", s);
}

async function pickMedia(label, extensions, onPath) {
  const path = await openDialog({ filters: [{ name: label, extensions }] });
  if (!path) return;
  toast("Importing…", 60000);
  try {
    onPath(await invoke("import_asset", { path }));
    $("toast").hidden = true;
    touch();
  } catch (e) { report(e); }
}
const baseName = (p) => (p ? p.split(/[\\/]/).pop() : "");

// ---------- background ----------
function renderBgKinds() {
  const box = $("bgKinds");
  box.replaceChildren();
  for (const [k, label] of Object.entries(BG_KINDS)) {
    const b = el("button", { textContent: label, className: state.current.bg.kind === k ? "on" : "" });
    b.onclick = () => { state.current.bg.kind = k; touch(false); renderBg(); };
    box.append(b);
  }
}
function renderBg() {
  renderBgKinds();
  const b = state.current.bg, f = $("bgFields");
  f.replaceChildren();
  const fitFields = () => [
    choice("Fit", b, "fit", { cover: "Fill (crop)", contain: "Fit (bars)", stretch: "Stretch" }),
    two(range("Zoom", b, "zoom", 1, 4), range("Dim", b, "dim", 0, .9)),
    two(range("Pan X", b, "offsetX", -1, 1), range("Pan Y", b, "offsetY", -1, 1)),
  ];
  const fileButton = (label, exts) => {
    const btn = el("button", { textContent: b.path ? baseName(b.path) : `Choose ${label.toLowerCase()}…` });
    btn.onclick = () => pickMedia(label, exts, (p) => { b.path = p; renderBg(); });
    return field(label, btn);
  };
  switch (b.kind) {
    case "color": f.append(color("Colour", b, "color")); break;
    case "gradient":
      f.append(two(color("From", b, "color"), color("To", b, "color2")), two(range("Angle", b, "angle", 0, 360, 1), range("Motion", b, "speed", 0, 3)));
      break;
    case "liquid":
      f.append(el("div", { className: "two" }, color("Base", b, "color"), color("Flow", b, "color2")), two(color("Glow", b, "color3"), range("Speed", b, "speed", .1, 4)));
      break;
    case "image": f.append(fileButton("Image", ["png", "jpg", "jpeg", "webp", "bmp"]), ...fitFields()); break;
    case "gif": f.append(fileButton("GIF", ["gif"]), range("Speed", b, "speed", .25, 3), ...fitFields()); break;
    case "video":
      f.append(fileButton("Video", ["mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm"]), range("Speed", b, "speed", .25, 3), ...fitFields());
      break;
    case "slideshow": {
      const pick = el("button", { textContent: b.dir ? baseName(b.dir) : "Choose folder…" });
      pick.onclick = async () => {
        const dir = await openDialog({ directory: true });
        if (dir) { b.dir = dir; touch(false); renderBg(); }
      };
      f.append(field("Folder", pick), num("Seconds per image", b, "interval", { min: 1, max: 3600 }), toggle("Shuffle", b, "shuffle"), ...fitFields());
      break;
    }
    case "screen": {
      const opts = {};
      for (const m of state.monitors) opts[m.index] = m.label;
      const s = el("select");
      for (const [v, t] of Object.entries(opts)) s.append(el("option", { value: v, textContent: t }));
      s.value = b.monitor;
      s.onchange = () => { b.monitor = +s.value; touch(false); };
      f.append(field("Display", s), choice("Fit", b, "fit", { cover: "Fill (crop)", stretch: "Stretch" }), two(range("Zoom", b, "zoom", 1, 4), range("Dim", b, "dim", 0, .9)));
      break;
    }
    case "albumart":
      f.append(two(range("Darken", b, "dim", 0, .9), range("Drift speed", b, "speed", .1, 4)),
        el("p", { className: "muted small", textContent: "The playing song's artwork, blurred. Add an Album art widget for the sharp cover." }));
      break;
    case "visualizer":
      f.append(
        choice("Style", b, "style", { bars: "Bars", mirror: "Mirrored", radial: "Radial" }),
        two(color("Low", b, "color2"), color("High", b, "color3")),
        two(color("Backdrop", b, "color"), range("Sensitivity", b, "speed", .2, 4)),
      );
      break;
  }
}

// ---------- widgets ----------
function widgetName(w) {
  const base = WIDGET_KINDS[w.kind] ?? w.kind;
  if (w.label) return `${base}: ${w.label}`;
  if (w.kind === "text") return `Text: ${w.text}`;
  const r = state.sensors.find((s) => s.id === w.sensor);
  return r && ["sensor", "ring", "bar", "spark"].includes(w.kind) ? `${base}: ${r.name}` : base;
}
function renderWidgets() {
  const list = $("widgetList");
  list.replaceChildren();
  const widgets = state.current.widgets;
  widgets.forEach((w, i) => {
    const eye = el("button", { textContent: w.visible === false ? "Show" : "Hide" });
    eye.onclick = (e) => { e.stopPropagation(); w.visible = w.visible === false; touch(); renderWidgets(); };
    const up = el("button", { textContent: "↑", title: "Draw earlier (behind)", disabled: i === 0 });
    up.setAttribute("aria-label", "Move back");
    up.onclick = (e) => { e.stopPropagation(); [widgets[i - 1], widgets[i]] = [widgets[i], widgets[i - 1]]; touch(); renderWidgets(); };
    const del = el("button", { textContent: "✕", title: "Remove" });
    del.setAttribute("aria-label", "Remove widget");
    del.onclick = (e) => {
      e.stopPropagation();
      widgets.splice(i, 1);
      if (state.selected === w.id) state.selected = null;
      touch(); renderWidgets();
    };
    const li = el("li", { className: state.selected === w.id ? "sel" : "" }, el("span", { textContent: widgetName(w) }), eye, up, del);
    li.onclick = () => select(w.id);
    list.append(li);
  });
  renderWidgetFields();
  renderHandles();
}
function select(id) {
  state.selected = id;
  renderWidgets();
}
function renderWidgetFields() {
  const f = $("widgetFields");
  f.replaceChildren();
  const w = state.current.widgets.find((x) => x.id === state.selected);
  if (!w) return;
  const thresholds = () => two(num("Warn at", w, "warn", {}), num("Critical at", w, "crit", {}));
  const fontPick = () => choice("Font", w, "font", state.fonts);
  const place = () => two(range("X", w, "x", 0, 1, .005), range("Y", w, "y", 0, 1, .005));
  const refreshName = () => renderWidgetListNames();
  switch (w.kind) {
    case "text": {
      const t = text("Text", w, "text", refreshName);
      t.append(el("small", { className: "muted", textContent: "Tokens: {time} {date} {media.title} {media.artist} {weather.temp} {weather.desc} {profile}" }));
      f.append(t, fontPick(), two(range("Size", w, "size", .02, .5, .002), color("Colour", w, "color")), choice("Align", w, "align", ["left", "center", "right"]), toggle("Shadow", w, "shadow"));
      break;
    }
    case "clock": case "date": {
      const fmt = text("Format", w, "format");
      fmt.append(el("small", { className: "muted", textContent: w.kind === "clock" ? "%H:%M 24h · %I:%M %p 12h · %H:%M:%S seconds" : "%A %d %B · %d/%m/%Y" }));
      f.append(fmt, fontPick(), two(range("Size", w, "size", .02, .5, .002), color("Colour", w, "color")), toggle("Shadow", w, "shadow"));
      break;
    }
    case "analog":
      f.append(two(range("Size", w, "w", .2, 1), color("Hands", w, "color")), color("Second hand", w, "color2"));
      break;
    case "sensor":
      f.append(sensorPicker(w, "sensor", refreshName), text("Label", w, "label", refreshName), fontPick(),
        two(range("Size", w, "size", .03, .6, .002), num("Decimals", w, "decimals", { min: 0, max: 3, step: 1 })),
        two(color("Colour", w, "color"), color("Label colour", w, "color2")), thresholds(), toggle("Show unit", w, "showUnit"));
      break;
    case "ring":
      f.append(sensorPicker(w, "sensor", refreshName), text("Label", w, "label", refreshName),
        two(num("Min", w, "min", {}), num("Max", w, "max", {})),
        two(range("Diameter", w, "w", .15, 1), range("Thickness", w, "thickness", .008, .12, .002)),
        range("Text size", w, "size", .03, .3, .002),
        two(color("Start", w, "color"), color("End", w, "color2")), color("Track", w, "trackColor"), thresholds(), fontPick());
      break;
    case "bar":
      f.append(sensorPicker(w, "sensor", refreshName), text("Label", w, "label", refreshName),
        two(num("Min", w, "min", {}), num("Max", w, "max", {})),
        two(range("Width", w, "w", .1, 1), range("Height", w, "h", .006, .2, .002)),
        range("Text size", w, "size", .02, .15, .002),
        two(color("Fill", w, "color"), color("Text", w, "color2")), color("Track", w, "trackColor"), thresholds(), fontPick());
      break;
    case "spark":
      f.append(sensorPicker(w, "sensor", refreshName), text("Label", w, "label", refreshName),
        two(num("Min", w, "min", {}), num("Max", w, "max", {})),
        two(range("Width", w, "w", .1, 1), range("Height", w, "h", .04, 1)),
        two(num("History (s)", w, "history", { min: 5, max: 600, step: 1 }), range("Line", w, "thickness", .002, .03, .001)),
        range("Text size", w, "size", .02, .15, .002),
        two(color("Line", w, "color"), color("Text", w, "color2")), fontPick());
      break;
    case "image": {
      const btn = el("button", { textContent: w.path ? baseName(w.path) : "Choose image…" });
      btn.onclick = () => pickMedia("Image", ["png", "jpg", "jpeg", "webp", "bmp"], (p) => { w.path = p; renderWidgetFields(); });
      f.append(field("Image", btn), two(range("Width", w, "w", .05, 1), range("Height", w, "h", .05, 1)));
      break;
    }
    case "lyrics": {
      const sync = range("Sync offset (−2 s to +2 s)", w, "offset", -2, 2, .05);
      f.append(two(range("Width", w, "w", .3, 1), range("Height", w, "h", .15, 1)), range("Text size", w, "size", .03, .16, .002),
        two(color("Current line", w, "color"), color("Other lines", w, "color2")), sync, fontPick(),
        el("p", { className: "muted small", textContent: "Sends the song title and artist to lrclib.net to find lyrics." }));
      break;
    }
    case "albumart":
      f.append(el("div", {}, range("Size", w, "w", .1, 1)), el("p", { className: "muted small", textContent: "Uses the player's artwork, or asks the iTunes Search API if it has none." }));
      break;
    case "track":
      f.append(two(range("Width", w, "w", .2, 1), range("Text size", w, "size", .02, .14, .002)), two(color("Title", w, "color"), color("Artist", w, "color2")),
        choice("Align", w, "align", ["left", "center", "right"]), fontPick());
      break;
    case "progress":
      f.append(two(range("Width", w, "w", .2, 1), range("Thickness", w, "h", .004, .04, .001)), range("Time text size", w, "size", .015, .08, .001),
        two(color("Fill", w, "color"), color("Time text", w, "color2")), color("Track", w, "trackColor"), fontPick());
      break;
  }
  f.append(place(), range("Opacity", w, "opacity", .05, 1));
}
function renderWidgetListNames() {
  const items = $("widgetList").querySelectorAll("li span");
  state.current.widgets.forEach((w, i) => { if (items[i]) items[i].textContent = widgetName(w); });
}

// ---------- preview & direct manipulation ----------
function canvasSize() {
  const d = state.status?.device;
  const pw = d?.width || 480, ph = d?.height || 480;
  const r = state.current?.rotation ?? 0;
  return r === 90 || r === 270 ? { w: ph, h: pw, pw, ph } : { w: pw, h: ph, pw, ph };
}
function layoutScreen() {
  const wrap = $("screenWrap"), screen = $("screen"), cv = $("preview");
  const { w, h, pw, ph } = canvasSize();
  const box = wrap.getBoundingClientRect();
  const k = Math.max(.1, Math.min((box.width - 40) / w, (box.height - 40) / h));
  screen.style.width = `${w * k}px`;
  screen.style.height = `${h * k}px`;
  cv.width = pw;
  cv.height = ph;
  cv.style.width = `${pw * k}px`;
  cv.style.height = `${ph * k}px`;
  // The stream is what the panel receives; undo the panel's orientation so the
  // preview is upright and matches where widgets are placed.
  const r = state.current?.rotation ?? 0;
  const flip = state.current?.flip ? " scaleX(-1)" : "";
  cv.style.transform = `translate(-50%,-50%) rotate(${-r}deg)${flip}`;
}
function widgetBox(w, c) {
  const unit = Math.min(c.w, c.h);
  switch (w.kind) {
    case "ring": case "analog": case "albumart": { const s = Math.min(w.w, w.h ?? w.w) * unit; return [s, s]; }
    case "lyrics": case "track": return [w.w * unit, w.h * unit];
    case "progress": return [w.w * unit, Math.max((w.size ?? .03) * 2, w.h) * unit];
    case "bar": case "spark": return [w.w * unit, (w.h + (w.size ?? .04) * 1.5) * unit];
    case "image": return [w.w * unit, w.h * unit];
    case "sensor": return [(w.size ?? .1) * unit * 3.2, (w.size ?? .1) * unit * 1.6];
    case "clock": return [(w.size ?? .1) * unit * 3, (w.size ?? .1) * unit * 1.1];
    case "date": return [(w.size ?? .1) * unit * 9, (w.size ?? .1) * unit * 1.3];
    default: return [(w.size ?? .1) * unit * Math.max(2, (w.text?.length ?? 4) * .56), (w.size ?? .1) * unit * 1.3];
  }
}
function renderHandles() {
  if (!state.current) return;
  const host = $("handles");
  const c = canvasSize();
  const shown = $("screen").getBoundingClientRect();
  const k = shown.width / c.w || 1;
  const keep = new Map([...host.children].map((n) => [n.dataset.id, n]));
  for (const w of state.current.widgets) {
    let h = keep.get(w.id);
    keep.delete(w.id);
    if (w.visible === false) { h?.remove(); continue; }
    if (!h) {
      h = el("div", { className: "handle" });
      h.dataset.id = w.id;
      h.onpointerdown = (e) => startDrag(e, h.dataset.id);
      host.append(h);
    }
    const [bw, bh] = widgetBox(w, c);
    const a = w.kind === "bar" || w.kind === "spark" ? -(w.size ?? .04) * .75 * Math.min(c.w, c.h) : 0;
    Object.assign(h.style, {
      left: `${w.x * c.w * k}px`, top: `${(w.y * c.h + a) * k}px`,
      width: `${bw * k}px`, height: `${bh * k}px`,
    });
    h.classList.toggle("sel", state.selected === w.id);
  }
  for (const stale of keep.values()) stale.remove();
}
function startDrag(e, id) {
  e.preventDefault();
  const w = state.current.widgets.find((x) => x.id === id);
  if (!w) return;
  if (state.selected !== id) select(id);
  const target = e.currentTarget ?? e.target;
  const r = $("screen").getBoundingClientRect();
  const start = { px: e.clientX, py: e.clientY, x: w.x, y: w.y };
  target.setPointerCapture?.(e.pointerId);
  const move = (m) => {
    let nx = start.x + (m.clientX - start.px) / r.width;
    let ny = start.y + (m.clientY - start.py) / r.height;
    // Gentle snap to the centre lines.
    if (Math.abs(nx - .5) < .012) nx = .5;
    if (Math.abs(ny - .5) < .012) ny = .5;
    w.x = Math.min(1, Math.max(0, nx));
    w.y = Math.min(1, Math.max(0, ny));
    touch();
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    renderWidgetFields();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

// The newest frame is fetched only while the Studio is visible. Decoded off the
// main thread by createImageBitmap, then drawn into the canvas.
let previewBusy = false, lastSeq = -1;
async function previewTick() {
  if (previewBusy || state.page !== "studio" || document.hidden) return;
  if (state.status && state.status.engine.seq === lastSeq && lastSeq !== -1 && !previewTick.animated) return;
  previewBusy = true;
  try {
    const res = await fetch("http://lcd.localhost/frame");
    if (res.ok) {
      const seq = +res.headers.get("X-Seq");
      if (seq !== lastSeq) {
        lastSeq = seq;
        const bmp = await createImageBitmap(await res.blob());
        const cv = $("preview");
        cv.getContext("2d").drawImage(bmp, 0, 0, cv.width, cv.height);
        bmp.close();
      }
    }
  } catch { /* the engine has no frame yet */ }
  previewBusy = false;
}
setInterval(previewTick, 66);

// ---------- stage controls ----------
function renderStage() {
  const p = state.current;
  $("profileName").value = p.name;
  $("brightness").value = p.brightness;
  $("brightnessOut").textContent = `${p.brightness}%`;
  $("flip").checked = p.flip;
  $("rotation").querySelectorAll("button").forEach((b) => b.classList.toggle("on", +b.dataset.v === p.rotation));
  $("fps").value = String(p.fps);
  $("quality").value = p.quality;
  $("sharpText").checked = p.sharpText;
  previewTick.animated = !["color", "image"].includes(p.bg.kind);
  layoutScreen();
}
function setCurrent(p) {
  flushIfPending();
  state.current = p;
  state.selected = null;
  $("handles").replaceChildren();
  renderStage();
  renderBg();
  renderWidgets();
}
function flushIfPending() {
  if (saveTimer) { clearTimeout(saveTimer); flushProfile(); }
}

// ---------- profiles ----------
function renderProfiles() {
  const grid = $("profileGrid");
  grid.replaceChildren();
  for (const p of state.profiles) {
    const isActive = p.id === state.settings.active;
    const use = el("button", { textContent: isActive ? "Showing" : "Show", className: isActive ? "" : "primary", disabled: isActive });
    use.onclick = () => activate(p);
    const edit = el("button", { textContent: "Edit" });
    edit.onclick = () => { activate(p); go("studio"); };
    const dup = el("button", { textContent: "Duplicate" });
    dup.onclick = () => addProfile({ ...structuredClone(p), id: "", name: `${p.name} copy` });
    const exp = el("button", { textContent: "Export" });
    exp.onclick = async () => {
      const dest = await saveDialog({ defaultPath: `${p.name}.mnsc`, filters: [{ name: "Meniscus profile", extensions: ["mnsc"] }] });
      if (dest) invoke("export_profile", { id: p.id, dest }).then(() => toast("Profile exported")).catch(report);
    };
    const del = el("button", { textContent: "Delete", className: "danger", disabled: state.profiles.length < 2 });
    del.onclick = async () => {
      if (del.dataset.armed !== "1") {
        del.dataset.armed = "1";
        del.textContent = "Really delete?";
        setTimeout(() => { del.dataset.armed = ""; del.textContent = "Delete"; }, 3000);
        return;
      }
      try {
        await invoke("delete_profile", { id: p.id });
        await reload();
      } catch (e) { report(e); }
    };
    const meta = `${BG_KINDS[p.bg.kind] ?? p.bg.kind} · ${p.widgets.length} widget${p.widgets.length === 1 ? "" : "s"} · ${p.fps} fps`;
    grid.append(el("div", { className: `glass pcard${isActive ? " active" : ""}` },
      el("b", { textContent: p.name }), el("span", { className: "muted small", textContent: meta }),
      el("div", { className: "row" }, use, edit, dup, exp, del)));
  }
  window.glassify(grid);

  const tg = $("templateGrid");
  if (!tg.children.length) {
    for (const t of window.TEMPLATES) {
      const b = el("button", { className: "tcard" }, el("b", { textContent: t.name }), el("small", { textContent: t.desc }));
      b.onclick = () => addProfile(fromTemplate(t));
      tg.append(b);
    }
  }
}
function fromTemplate(t) {
  return {
    id: "", name: t.name, fps: t.fps ?? 30, brightness: 100, rotation: 0, flip: false, quality: 100, sharpText: true,
    bg: { ...t.bg }, widgets: structuredClone(t.widgets),
  };
}
async function addProfile(p) {
  flushIfPending();
  try {
    const saved = await invoke("save_profile", { profile: p });
    await activate(saved);
    await reload();
    toast(`Added “${saved.name}”`);
  } catch (e) { report(e); }
}
async function activate(p) {
  flushIfPending();
  state.settings.active = p.id;
  await saveSettings();
  renderProfiles();
  setCurrent(state.profiles.find((x) => x.id === p.id) ?? p);
}
// Re-reads authoritative state, keeping the same object for the profile being edited.
async function reload() {
  flushIfPending();
  [state.profiles, state.settings] = await Promise.all([invoke("get_profiles"), invoke("get_settings")]);
  const cur = state.profiles.find((p) => p.id === state.settings.active) ?? state.profiles[0];
  setCurrent(cur);
  renderProfiles();
  renderAutomation();
}

// ---------- automation ----------
function bindSetting(id, key, kind = "value", scale = (v) => v) {
  const n = $(id);
  if (kind === "checked") {
    n.checked = !!state.settings[key];
    n.onchange = () => { state.settings[key] = n.checked; saveSettings(); };
  } else {
    n.value = state.settings[key];
    n.onchange = () => { state.settings[key] = scale(n.value); saveSettings(); };
  }
}
function renderAutomation() {
  const num2 = (v) => +v || 0;
  bindSetting("carousel", "carousel", "checked");
  bindSetting("carouselSeconds", "carouselSeconds", "value", num2);
  bindSetting("alertEnabled", "alertEnabled", "checked");
  bindSetting("alertThreshold", "alertThreshold", "value", num2);
  bindSetting("alertText", "alertText");
  bindSetting("sleepEnabled", "sleepEnabled", "checked");
  bindSetting("sleepFrom", "sleepFrom");
  bindSetting("sleepTo", "sleepTo");
  bindSetting("sleepAction", "sleepAction");
  bindSetting("sleepDim", "sleepDim", "value", num2);
  bindSetting("idleOff", "idleOffMinutes", "value", num2);
  bindSetting("weatherEnabled", "weatherEnabled", "checked");
  bindSetting("weatherLat", "weatherLat", "value", num2);
  bindSetting("weatherLon", "weatherLon", "value", num2);
  bindSetting("weatherF", "weatherFahrenheit", "checked");
  bindSetting("closeToTray", "closeToTray", "checked");
  bindSetting("startHidden", "startHidden", "checked");
  sensorOptions($("alertSensor"), state.settings.alertSensor);
  $("alertSensor").onchange = () => { state.settings.alertSensor = $("alertSensor").value; saveSettings(); };

  const ids = $("carouselIds");
  ids.replaceChildren();
  for (const p of state.profiles) {
    const on = state.settings.carouselIds.includes(p.id);
    const b = el("button", { textContent: p.name, className: on ? "on" : "" });
    b.onclick = () => {
      const list = state.settings.carouselIds;
      if (on) list.splice(list.indexOf(p.id), 1); else list.push(p.id);
      saveSettings();
      renderAutomation();
    };
    ids.append(b);
  }
  renderRules();
}
function renderRules() {
  const box = $("ruleList");
  box.replaceChildren();
  const rules = state.settings.rules;
  rules.forEach((r, i) => {
    const on = el("input", { type: "checkbox", checked: r.enabled, title: "Enabled" });
    on.onchange = () => { r.enabled = on.checked; saveSettings(); };
    let middle;
    if (r.kind === "app") {
      const exe = el("input", { type: "text", value: r.exe, placeholder: "game.exe", spellcheck: false });
      exe.onchange = () => { r.exe = exe.value.trim(); saveSettings(); };
      const grab = el("button", { textContent: "Pick in 3s", title: "Switch to the app; its name is captured after 3 seconds" });
      grab.onclick = () => {
        grab.textContent = "Switch now…";
        setTimeout(async () => {
          const name = await invoke("foreground_app");
          grab.textContent = "Pick in 3s";
          if (name) { r.exe = name; exe.value = name; saveSettings(); }
        }, 3000);
      };
      middle = el("div", { className: "exe" }, exe, grab);
    } else {
      const from = el("input", { type: "time", value: r.from });
      const to = el("input", { type: "time", value: r.to });
      from.onchange = () => { r.from = from.value; saveSettings(); };
      to.onchange = () => { r.to = to.value; saveSettings(); };
      middle = el("div", { className: "times" }, from, to);
    }
    const prof = el("select");
    for (const p of state.profiles) prof.append(el("option", { value: p.id, textContent: p.name }));
    prof.value = r.profile || state.profiles[0]?.id;
    r.profile = prof.value;
    prof.onchange = () => { r.profile = prof.value; saveSettings(); };
    const del = el("button", { textContent: "✕" });
    del.setAttribute("aria-label", "Remove rule");
    del.onclick = () => { rules.splice(i, 1); saveSettings(); renderRules(); };
    box.append(el("div", { className: "rule" }, on, middle, prof, del));
  });
}
function addRule(kind) {
  state.settings.rules.push({ kind, enabled: true, exe: "", from: "22:00", to: "07:00", profile: state.profiles[0]?.id ?? "" });
  saveSettings();
  renderRules();
}

// ---------- sensors page ----------
function renderSensors() {
  const q = $("sensorSearch").value.trim().toLowerCase();
  const host = $("sensorGroups");
  const groups = new Map();
  for (const r of state.sensors) {
    if (q && !`${r.group} ${r.name}`.toLowerCase().includes(q)) continue;
    if (!groups.has(r.group)) groups.set(r.group, []);
    groups.get(r.group).push(r);
  }
  const hasTemps = state.sensors.some((r) => /°|\?C|C$/.test(r.unit) && /temp/i.test(r.name));
  $("sensorHint").hidden = hasTemps || state.sensors.length === 0;
  $("sensorSummary").textContent = `${state.sensors.length} readings, updated every second.`;
  host.replaceChildren(...[...groups].map(([g, rows]) => {
    const table = el("table");
    for (const r of rows) {
      const v = Math.abs(r.value) >= 100 ? r.value.toFixed(0) : r.value.toFixed(1);
      table.append(el("tr", {}, el("td", { textContent: r.name }), el("td", { textContent: `${v} ${r.unit}` })));
    }
    return el("div", { className: "glass sgroup" }, el("h3", { textContent: g }), table);
  }));
  window.glassify(host);
}

// ---------- status ----------
const fact = (host, rows) => host.replaceChildren(...rows.flatMap(([k, v]) => [el("dt", { textContent: k }), el("dd", { textContent: v })]));
let lastUsb = { t: performance.now(), frames: 0, bytes: 0 };
let usbRate = { fps: 0, mbps: 0 };
function renderStatus() {
  const s = state.status;
  if (!s) return;
  const d = s.device, on = d.state === "connected", busy = d.state === "busy", stuck = d.state === "stuck";
  $("devicePill").className = `device-pill${on ? " on" : busy || stuck ? " bad" : ""}`;
  $("deviceText").textContent = on ? `Screen connected · ${d.width}×${d.height}` : busy ? "Screen in use by another app"
    : stuck ? "Screen not responding: shut down the PC, then power it on" : "Searching for screen…";
  // Rates come from whole poll intervals; this function also runs on page changes.
  const now = performance.now(), dt = (now - lastUsb.t) / 1000;
  if (dt >= .8) {
    usbRate = { fps: (s.usbFrames - lastUsb.frames) / dt, mbps: (s.usbBytes - lastUsb.bytes) / dt / 1e6 };
    lastUsb = { t: now, frames: s.usbFrames, bytes: s.usbBytes };
  }
  const { fps: usbFps, mbps } = usbRate;
  const e = s.engine;
  $("navFoot").innerHTML = "";
  $("navFoot").append(`Meniscus ${s.cpuPercent.toFixed(1)}% CPU`, el("br"), `${s.memoryMb.toFixed(0)} MB memory`);
  $("stageMeta").textContent = `${e.encodesPerSec.toFixed(0)} new frames/s · ${e.frameKb.toFixed(0)} KB${e.reason ? ` · ${e.reason}` : ""}`;
  if (state.page === "system") {
    fact($("deviceFacts"), [
      ["State", on ? "Connected" : busy ? "In use by another app" : stuck ? "Not responding" : "Not found"],
      ["Resolution", on ? `${d.width}×${d.height}` : "–"],
      ["Panel mode / sub", on ? `${d.pm} / ${d.sub}` : "–"],
      ["Picture format", on ? (d.jpeg ? "JPEG" : "Uncompressed RGB565") : "–"],
      ["Frames delivered", d.frames.toLocaleString()],
      ["Link errors", String(d.errors)],
    ]);
    fact($("perfFacts"), [
      ["App CPU (whole machine)", `${s.cpuPercent.toFixed(2)}%`],
      ["App memory", `${s.memoryMb.toFixed(0)} MB`],
      ["New frames encoded", `${e.encodesPerSec.toFixed(1)}/s`],
      ["Frames over USB", `${usbFps.toFixed(1)}/s`],
      ["USB bandwidth", `${mbps.toFixed(2)} MB/s`],
      ["Compose / encode", `${e.renderMs.toFixed(1)} / ${e.encodeMs.toFixed(1)} ms`],
    ]);
  }
  // The screen's resolution is only known once connected.
  const size = `${d.width}x${d.height}`;
  if (size !== renderStatus.size) { renderStatus.size = size; layoutScreen(); renderHandles(); }
}
async function pollStatus() {
  try {
    state.status = await invoke("get_status");
    renderStatus();
    if (state.page === "sensors" || !state.sensors.length) {
      state.sensors = await invoke("get_sensors");
      if (state.page === "sensors") renderSensors();
    }
  } catch (e) { console.error(e); }
}
async function pollCoexistence() {
  try {
    const c = await invoke("coexistence");
    $("takeover").hidden = !(c.trccRunning || state.status?.device.state === "busy");
    $("autostart").checked = c.autostart;
    $("trccState").textContent = `TRCC is ${c.trccRunning ? "running" : "not running"} and ${c.trccAutostart ? "starts" : "does not start"} with Windows.`;
    $("trccStop").disabled = !c.trccRunning && !c.trccAutostart;
    $("trccRestore").disabled = c.trccAutostart;
  } catch (e) { console.error(e); }
}

// ---------- navigation ----------
function go(page) {
  state.page = page;
  const nav = $("nav");
  nav.querySelectorAll("button").forEach((b) => {
    const on = b.dataset.page === page;
    b.classList.toggle("active", on);
    if (on) {
      const lens = $("navLens");
      lens.style.height = `${b.offsetHeight}px`;
      lens.style.transform = `translateY(${b.offsetTop - 9}px)`;
    }
  });
  document.querySelectorAll(".page").forEach((p) => p.classList.toggle("active", p.id === `page-${page}`));
  if (page === "studio") { layoutScreen(); renderHandles(); }
  if (page === "automation") renderAutomation();
  if (page === "sensors") { invoke("get_sensors").then((r) => { state.sensors = r; renderSensors(); }); }
  if (page === "system") { renderStatus(); pollCoexistence(); }
  window.glassify();
}

// ---------- wiring ----------
function wire() {
  $("nav").querySelectorAll("button").forEach((b) => (b.onclick = () => go(b.dataset.page)));
  $("winMin").onclick = () => appWindow.minimize();
  $("winMax").onclick = () => appWindow.toggleMaximize();
  $("winClose").onclick = () => { flushIfPending(); appWindow.close(); };

  $("profileName").oninput = (e) => { state.current.name = e.target.value; touch(false); };
  $("profileName").onchange = () => renderProfiles();
  $("brightness").oninput = (e) => {
    state.current.brightness = +e.target.value;
    $("brightnessOut").textContent = `${e.target.value}%`;
    touch(false);
  };
  $("flip").onchange = (e) => { state.current.flip = e.target.checked; touch(false); layoutScreen(); };
  $("rotation").querySelectorAll("button").forEach((b) => (b.onclick = () => {
    state.current.rotation = +b.dataset.v;
    touch();
    renderStage();
    renderHandles();
  }));
  $("fps").onchange = (e) => { state.current.fps = +e.target.value; touch(false); };
  $("quality").oninput = (e) => { state.current.quality = +e.target.value; touch(false); };
  $("sharpText").onchange = (e) => { state.current.sharpText = e.target.checked; touch(false); };

  const add = $("addWidget");
  add.append(el("option", { value: "", textContent: "Add widget…" }));
  for (const [k, label] of Object.entries(WIDGET_KINDS)) add.append(el("option", { value: k, textContent: label }));
  add.onchange = () => {
    const kind = add.value;
    add.value = "";
    if (!kind) return;
    const w = {
      id: `w${Date.now().toString(36)}`, kind, visible: true, x: .5, y: .5, w: .5, h: .5,
      color: "#ffffff", color2: "#7cc7ff", trackColor: "#ffffff26", font: "Segoe UI Semibold", size: .1,
      text: "", label: "", format: "", sensor: "", min: 0, max: 100, decimals: 0, showUnit: true, thickness: .06, offset: 0,
      warn: 0, crit: 0, warnColor: "#ffb020", critColor: "#ff4d4f", opacity: 1, align: "center", path: "", shadow: true, history: 60,
      ...WIDGET_DEFAULTS[kind],
    };
    state.current.widgets.push(w);
    touch();
    select(w.id);
  };

  $("profileNew").onclick = () => addProfile(fromTemplate(window.TEMPLATES[0]));
  $("profileImport").onclick = async () => {
    const path = await openDialog({ filters: [{ name: "Meniscus profile", extensions: ["mnsc"] }] });
    if (!path) return;
    try {
      await activate(await invoke("import_profile", { path }));
      await reload();
    } catch (e) { report(e); }
  };
  $("ruleApp").onclick = () => addRule("app");
  $("ruleTime").onclick = () => addRule("time");
  $("sensorSearch").oninput = renderSensors;

  const takeOver = async (disableAutostart) => {
    try {
      await invoke("take_over", { disableAutostart });
      toast("Stopping TRCC…");
      setTimeout(pollCoexistence, 2500);
    } catch (e) { report(e); }
  };
  $("takeoverBtn").onclick = () => takeOver($("takeoverAuto").checked);
  $("trccStop").onclick = () => takeOver(true);
  $("trccRestore").onclick = async () => {
    try {
      await invoke("restore_trcc_autostart");
      setTimeout(pollCoexistence, 2500);
    } catch (e) { report(e); }
  };
  $("autostart").onchange = (e) => invoke("set_autostart", { enable: e.target.checked }).catch(report);

  new ResizeObserver(() => { layoutScreen(); renderHandles(); }).observe($("screenWrap"));
  window.addEventListener("beforeunload", flushIfPending);
}

async function boot() {
  wire();
  [state.fonts, state.monitors, state.sensors] = await Promise.all([invoke("get_fonts"), invoke("get_monitors"), invoke("get_sensors")]);
  invoke("data_folder").then((p) => ($("dataFolder").textContent = p));
  await pollStatus();
  await reload();
  go("studio");
  setInterval(pollStatus, 1000);
  setInterval(pollCoexistence, 8000);
  pollCoexistence();
}
boot().catch((e) => toast(`Startup failed: ${e}`, 60000));
