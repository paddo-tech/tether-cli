// Keyframes on one diagram clock. Times are fractions of the loop.
const r = (t: number) => +t.toFixed(4);

// Opacity: 1 inside each [on, off] window, 0 outside, with a short fade
export const pulse = (windows: [number, number][], fade = 0.02) => {
  const pts: [number, number][] = [];
  for (const [on, off] of windows) {
    if (on > 0) pts.push([on - fade, 0]);
    pts.push([on, 1], [off, 1]);
    if (off < 1) pts.push([off + fade, 0]);
  }
  if (pts[0][0] > 0) pts.unshift([0, 0]);
  if (pts[pts.length - 1][0] < 1) pts.push([1, 0]);
  return { values: pts.map(p => p[1]).join(';'), keyTimes: pts.map(p => r(p[0])).join(';') };
};

// animateMotion: [time, distance fraction] stops, held before the first and after the last
export const move = (...stops: [number, number][]) => {
  const pts = [...stops];
  if (pts[0][0] > 0) pts.unshift([0, pts[0][1]]);
  if (pts[pts.length - 1][0] < 1) pts.push([1, pts[pts.length - 1][1]]);
  return { keyPoints: pts.map(p => r(p[1])).join(';'), keyTimes: pts.map(p => r(p[0])).join(';') };
};
