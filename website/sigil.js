// Sanctum's sigils, drawn the way the app draws them (core/src/sigil.rs and
// app/lib/sigil/sigil_painter.dart), seeded from a number instead of a key.
// Decoration only: the page reads fine without it.
(function () {
  function prng(s) { s = s >>> 0 || 1; return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; }; }
  const FORMS = ['rosette', 'constellation', 'shards', 'orbits', 'lattice', 'glyph'];
  function spec(seed, form) {
    const r = prng(seed * 2654435761), fp = Array.from({ length: 32 }, () => Math.floor(r() * 256));
    const u = b => b / 255, hue = h => ((h % 360) + 360) % 360, base = (fp[0] | (fp[1] << 8)) / 65536 * 360;
    const off = [[0, 0, 0], [180, 0, 180], [120, 240, 0], [30, -30, 60]][fp[2] % 4];
    return {
      accent: [base, 0.7 + u(fp[3]) * 0.25, 0.62 + u(fp[4]) * 0.08],
      palette: [0, 1, 2].map(i => [hue(base + off[i]), 0.45 + u(fp[5 + i]) * 0.45, [0.62, 0.45, 0.75][i] + (u(fp[8 + i]) - 0.5) * 0.1]),
      bg: [hue(base + 15), 0.3 + u(fp[11]) * 0.3, 0.07 + u(fp[12]) * 0.04],
      form: form || FORMS[fp[13] % 6], symmetry: 3 + fp[14] % 10, layers: 2 + fp[15] % 5,
      rotation: u(fp[16]) * 360, density: u(fp[17]), weight: u(fp[18]), seed: fp[24] | (fp[25] << 8) | (fp[26] << 16),
    };
  }
  const inkA = (hex, a) => `rgba(${[1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16)).join(',')},${a})`;
  const css = (c, a = 1) => `hsla(${c[0].toFixed(1)},${(c[1] * 100).toFixed(1)}%,${(c[2] * 100).toFixed(1)}%,${a})`;

  function draw(sp, size, opt) {
    const ink = opt.ink, light = opt.light;
    const fix = c => light ? [c[0], c[1], Math.max(0.28, c[2] - 0.3)] : c;
    const A = ink ? null : fix(sp.accent), P = sp.palette.map(fix);
    const op = (c, a) => ink ? inkA(ink, a) : css(c, a);
    const r = size / 2 * 0.84, rnd = prng(sp.seed + 7), out = [];
    const sw = s => r * (0.007 + 0.016 * sp.weight) * s;
    const line = (pts, c, w, a = 1, close = false) => out.push(`<path d="M${pts.map(p => p[0].toFixed(2) + ' ' + p[1].toFixed(2)).join('L')}${close ? 'Z' : ''}" fill="none" stroke="${op(c, a)}" stroke-width="${w.toFixed(2)}" stroke-linecap="round" stroke-linejoin="round"/>`);
    const dot = (x, y, rad, c, a = 1) => out.push(`<circle cx="${x.toFixed(2)}" cy="${y.toFixed(2)}" r="${rad.toFixed(2)}" fill="${op(c, a)}"/>`);
    const dir = (a, d) => [Math.cos(a) * d, Math.sin(a) * d];
    const layerC = i => i === 0 ? A : P[(i - 1) % 3];
    const f = sp.form;
    if (f === 'rosette') {
      const k = sp.symmetry;
      for (let L = 0; L < sp.layers; L++) {
        const c = 0.15 + rnd() * 0.7, sc = r * (1 - L * 0.14) / (1 + c), pts = [];
        for (let i = 0; i <= 720; i++) { const t = i / 720 * 2 * Math.PI; pts.push([(Math.cos(t) + c * Math.cos(-(k - 1) * t)) * sc, (Math.sin(t) + c * Math.sin(-(k - 1) * t)) * sc]); }
        line(pts, layerC(L), sw(L ? 0.8 : 1.2), L ? 0.75 : 1);
      }
      dot(0, 0, r * 0.05, A);
    } else if (f === 'constellation') {
      const rings = [];
      for (let g = 0; g < sp.layers; g++) {
        const rad = r * (0.25 + 0.75 * (g + 1) / sp.layers), n = sp.symmetry * (1 + (g % 2 && sp.density > 0.5 ? 1 : 0)), o = rnd() * Math.PI;
        rings.push(Array.from({ length: n }, (_, i) => dir(o + i * 2 * Math.PI / n, rad)));
      }
      rings.forEach((h, g) => {
        const skip = 1 + Math.floor(rnd() * 3);
        h.forEach((p, i) => line([p, h[(i + skip) % h.length]], P[0], sw(0.6), 0.7));
        if (g + 1 < rings.length) { const nx = rings[g + 1]; h.forEach((p, i) => line([p, nx[Math.floor(i * nx.length / h.length) % nx.length]], P[1], sw(0.5), 0.6)); }
      });
      rings.forEach((h, g) => h.forEach(p => dot(p[0], p[1], r * (0.018 + 0.02 * sp.weight) * (g === rings.length - 1 ? 1.4 : 1), g % 2 ? P[2] : A)));
    } else if (f === 'shards') {
      const k = sp.symmetry, w = 2 * Math.PI / k, shapes = [];
      for (let s = 0; s < 2 + Math.round(sp.density * 3); s++) shapes.push([[0, 1, 2].map(() => dir(rnd() * w / 2, r * (0.15 + rnd() * 0.85))), s === 0 ? A : P[s % 3]]);
      for (let i = 0; i < k; i++) for (const m of [1, -1]) for (const [pts, c] of shapes) {
        const t = pts.map(([x, y]) => { y *= m; const a = i * w; return [x * Math.cos(a) - y * Math.sin(a), x * Math.sin(a) + y * Math.cos(a)]; });
        out.push(`<path d="M${t.map(p => p.map(v => v.toFixed(2)).join(' ')).join('L')}Z" fill="${op(c, ink ? 0.18 : 0.55)}" stroke="${op(c, 1)}" stroke-width="${sw(0.5).toFixed(2)}" stroke-linejoin="round"/>`);
      }
    } else if (f === 'orbits') {
      const n = 3 + sp.layers + Math.round(sp.density * 3);
      for (let i = 0; i < n; i++) {
        const rad = r * (0.2 + 0.8 * (i + 1) / n), w = sw(0.6 + rnd() * 2.2), seg = 1 + Math.floor(rnd() * sp.symmetry), gap = 0.15 + rnd() * 0.5, st = rnd() * 2 * Math.PI, swp = 2 * Math.PI / seg;
        for (let s = 0; s < seg; s++) { const a0 = st + s * swp, a1 = a0 + swp * (1 - gap), pts = []; for (let j = 0; j <= 40; j++) pts.push(dir(a0 + (a1 - a0) * j / 40, rad)); line(pts, layerC(i % 4), w); }
        if (rnd() < 0.4) { const p = dir(st - 0.2, rad); dot(p[0], p[1], w * 1.6, A); }
      }
    } else if (f === 'lattice') {
      const sides = 3 + sp.symmetry % 6, n = 8 + Math.round(sp.density * 14), tw = (0.04 + rnd() * 0.12) * (rnd() < 0.5 ? 1 : -1);
      for (let i = 0; i < n; i++) { const t = i / n, rad = r * (1 - t * 0.92), pts = []; for (let v = 0; v <= sides; v++) pts.push(dir(i * tw + v * 2 * Math.PI / sides, rad)); line(pts, i < n / 2 ? A : P[Math.floor(i / 4) % 3], sw(i ? 0.7 : 1.4), 1 - t * 0.5); }
    } else {
      const n = 5, mid = 2, cell = r * 1.1 / (n - 1), pt = (x, y) => [(x - mid) * cell, (y - mid) * cell];
      const steps = 6 + Math.round(sp.density * 5), mv = [[0, 1], [-1, 0], [1, 0], [-1, 1], [1, 1], [-1, -1], [0, -1]];
      let walk;
      do { let x = mid, y = Math.floor(rnd() * 2); walk = [[x, y]]; for (let i = 0; i < steps; i++) { const [dx, dy] = mv[Math.floor(rnd() * 7)]; const nx = Math.min(mid, Math.max(0, x + dx)), ny = Math.min(n - 1, Math.max(0, y + dy)); if (walk.some(p => p[0] === nx && p[1] === ny)) continue; x = nx; y = ny; walk.push([x, y]); } } while (walk.length < 5 || !walk.some(p => p[0] === 0));
      const tr = m => walk.map(([x, y]) => pt(m ? n - 1 - x : x, y));
      for (const m of [0, 1]) line(tr(m), P[1], sw(5), 0.45);
      for (const m of [0, 1]) line(tr(m), A, sw(2.4));
      for (const [x, y] of [walk[0], walk[walk.length - 1]]) { dot(...pt(x, y), sw(3.2), P[2]); dot(...pt(n - 1 - x, y), sw(3.2), P[2]); }
      const fr = []; for (let j = 0; j <= 96; j++) fr.push(dir(j / 96 * 2 * Math.PI, r)); line(fr, P[0], sw(0.8), 0.6);
    }
    const rot = f === 'glyph' ? 0 : sp.rotation;
    const bg = opt.bg ? `<rect width="${size}" height="${size}" fill="${css(sp.bg)}"/>` : '';
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">${bg}<g transform="translate(${size / 2} ${size / 2}) rotate(${rot.toFixed(1)})">${out.join('')}</g></svg>`;
  }
  // <div data-sigil="seed,form" [data-sigil-bg] [data-sigil-ink]>: drawn at 200 units, sized by CSS.
  const light = window.matchMedia && matchMedia('(prefers-color-scheme: light)').matches;
  document.querySelectorAll('[data-sigil]').forEach(e => {
    const [seed, form] = e.dataset.sigil.split(',');
    const bg = e.hasAttribute('data-sigil-bg');
    const ink = e.hasAttribute('data-sigil-ink') ? '#3A2508' : undefined;
    e.innerHTML = draw(spec(+seed, form), 200, { ink, light: light && !bg && !ink, bg });
  });
})();
