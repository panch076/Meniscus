// Starting layouts. They use Afterburner's sensor ids where a temperature is needed;
// any sensor can be swapped in afterwards.
const CPU_T = "mahm:CPU temperature", GPU_T = "mahm:GPU1 temperature";
const w = (id, kind, props) => ({ id, kind, ...props });

window.TEMPLATES = [
  {
    name: "Lyrical", desc: "Album art, song and synced lyrics over blurred artwork",
    bg: { kind: "albumart", speed: 1, dim: .3 },
    widgets: [
      w("art", "albumart", { x: .135, y: .135, w: .2, h: .2 }),
      w("song", "track", { x: .63, y: .095, w: .69, h: .11, size: .044, font: "Segoe UI Bold", color2: "#ffffff99", align: "left", shadow: false }),
      w("time", "progress", { x: .63, y: .178, w: .69, h: .012, size: .026, color: "#ffffffbf", trackColor: "#ffffff33", color2: "#ffffff8c", font: "Segoe UI Semibold", shadow: false }),
      w("lyrics", "lyrics", { x: .5, y: .645, w: .92, h: .69, size: .074, font: "Segoe UI Bold", color2: "#ffffff52", shadow: false }),
    ],
  },
  {
    name: "Album Cover", desc: "Large artwork with the song and progress",
    bg: { kind: "albumart", speed: 1, dim: .15 },
    widgets: [
      w("art", "albumart", { x: .5, y: .4, w: .68, h: .68 }),
      w("song", "track", { x: .5, y: .82, w: .84, h: .11, size: .05, font: "Segoe UI Bold", color2: "#ffffff99", align: "center", shadow: false }),
      w("time", "progress", { x: .5, y: .905, w: .84, h: .012, size: .028, color: "#ffffffbf", trackColor: "#ffffff33", color2: "#ffffff8c", font: "Segoe UI Semibold", shadow: false }),
    ],
  },
  {
    name: "Twin Rings", desc: "CPU and GPU temperature with a clock",
    bg: { kind: "liquid", color: "#081430", color2: "#1f6feb", color3: "#7c3aed" },
    widgets: [
      w("clock", "clock", { x: .5, y: .17, size: .15, font: "Segoe UI Light" }),
      w("cpu", "ring", { x: .27, y: .56, w: .44, h: .44, sensor: CPU_T, label: "CPU", color: "#7cc7ff", color2: "#c58bff", size: .1, thickness: .035, warn: 75, crit: 88 }),
      w("gpu", "ring", { x: .73, y: .56, w: .44, h: .44, sensor: GPU_T, label: "GPU", color: "#5ef2c2", color2: "#7cc7ff", size: .1, thickness: .035, warn: 75, crit: 88 }),
      w("load", "bar", { x: .5, y: .89, w: .8, h: .018, size: .04, sensor: "sys:cpu_load", label: "CPU load", color: "#7cc7ff", color2: "#ffffffcc" }),
    ],
  },
  {
    name: "Big Number", desc: "One huge CPU temperature, readable across the room",
    bg: { kind: "gradient", color: "#04060f", color2: "#102a5c", angle: 120, speed: 0 },
    widgets: [
      w("t", "sensor", { x: .5, y: .46, size: .42, font: "Segoe UI Light", sensor: CPU_T, label: "CPU", color: "#ffffff", color2: "#9fd6ff", warn: 75, crit: 88 }),
      w("c", "clock", { x: .5, y: .86, size: .07, color: "#ffffffbb" }),
    ],
  },
  {
    name: "Clockface", desc: "An analog clock with the date",
    bg: { kind: "color", color: "#05070d" },
    widgets: [
      w("a", "analog", { x: .5, y: .5, w: .9, h: .9, color: "#f2f6ff", color2: "#ff6b4a" }),
      w("d", "date", { x: .5, y: .68, size: .042, format: "%a %d %b", color: "#ffffffaa", shadow: false }),
    ],
  },
  {
    name: "Pulse Graphs", desc: "Rolling history for CPU, GPU and memory",
    bg: { kind: "color", color: "#070b18" },
    widgets: [
      w("s1", "spark", { x: .5, y: .22, w: .86, h: .17, size: .045, sensor: "sys:cpu_load", label: "CPU", color: "#7cc7ff", color2: "#ffffffcc", thickness: .006, max: 100 }),
      w("s2", "spark", { x: .5, y: .54, w: .86, h: .17, size: .045, sensor: "mahm:GPU1 usage", label: "GPU", color: "#5ef2c2", color2: "#ffffffcc", thickness: .006, max: 100 }),
      w("s3", "spark", { x: .5, y: .86, w: .86, h: .17, size: .045, sensor: "sys:ram_load", label: "RAM", color: "#c58bff", color2: "#ffffffcc", thickness: .006, max: 100 }),
    ],
  },
  {
    name: "Now Playing", desc: "Audio spectrum with the current track",
    bg: { kind: "visualizer", style: "mirror", color: "#05060f", color2: "#7cc7ff", color3: "#c58bff", speed: 1 },
    widgets: [
      w("t", "text", { x: .5, y: .13, size: .065, text: "{media.title}" }),
      w("a", "text", { x: .5, y: .22, size: .042, text: "{media.artist}", color: "#ffffffb0" }),
      w("c", "clock", { x: .5, y: .9, size: .07, color: "#ffffffcc" }),
    ],
  },
  {
    name: "Screen Mirror", desc: "Your desktop, with temperatures on top",
    bg: { kind: "screen", monitor: 0, fit: "cover", dim: .15 },
    fps: 20,
    widgets: [
      w("c", "sensor", { x: .28, y: .9, size: .09, sensor: CPU_T, label: "CPU", warn: 75, crit: 88 }),
      w("g", "sensor", { x: .72, y: .9, size: .09, sensor: GPU_T, label: "GPU", warn: 75, crit: 88 }),
    ],
  },
];
