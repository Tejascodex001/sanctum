// The page's moving light: gold dust from the door, grey noise that dies at the
// threshold, and small scenes in the windows. Every scene answers a mouse or a
// finger: moving stirs it, tapping sets something off. Decoration only: each
// canvas sits over a page that reads fine without it, runs only while on screen,
// and stays still for people who ask their phone for less motion.
(function () {
  const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const rand = (a, b) => a + Math.random() * (b - a);
  const TAU = Math.PI * 2;
  let colors = {};
  const readColors = () => {
    const cs = getComputedStyle(document.documentElement), v = n => cs.getPropertyValue(n).trim();
    const forced = document.documentElement.dataset.theme;
    colors = { dust: v('--dust'), noise: v('--noise'), gold: v('--gold'), faint: v('--faint'), text: v('--text'),
      light: forced ? forced === 'light' : matchMedia('(prefers-color-scheme: light)').matches };
  };
  readColors();
  matchMedia('(prefers-color-scheme: light)').addEventListener?.('change', () => { readColors(); scenes.forEach(s => s.resize()); });

  const glowDot = (ctx, x, y, r, color, a) => {
    if (a <= 0 || r <= 0) return;
    ctx.globalAlpha = a * 0.25; ctx.fillStyle = color;
    ctx.beginPath(); ctx.arc(x, y, r * 3, 0, TAU); ctx.fill();
    ctx.globalAlpha = a; ctx.beginPath(); ctx.arc(x, y, r, 0, TAU); ctx.fill();
  };

  // Short-lived sparks any scene can throw: from a tap, a moving finger, an arrival.
  function sparks() {
    let ps = [];
    return {
      add(x, y, n, speed, color, o = {}) {
        for (let i = 0; i < n; i++) {
          const a = o.dir != null ? o.dir + rand(-o.spread, o.spread) : rand(0, TAU), v = rand(speed * 0.3, speed);
          ps.push({ x, y, vx: Math.cos(a) * v + (o.vx || 0), vy: Math.sin(a) * v + (o.vy || 0), a: 1, fade: rand(0.7, 1.3) / (o.life || 1.2), r: rand(0.8, o.r || 2), c: color, g: o.g || 0 });
        }
        if (ps.length > 400) ps.splice(0, ps.length - 400);
      },
      paint(ctx, dt) {
        ps = ps.filter(p => (p.a -= dt * p.fade) > 0);
        ps.forEach(p => { p.x += p.vx * dt; p.y += p.vy * dt; p.vx *= 0.94; p.vy = p.vy * 0.94 + p.g * dt; glowDot(ctx, p.x, p.y, p.r, p.c, p.a); });
      },
    };
  }

  // One scene per canvas: sized to its box (at most 2x pixels), ticking only while
  // visible, with the pointer tracked over the canvas's parent. Finger moves are read
  // from touch events, which keep coming while the page scrolls under the finger.
  const scenes = [];
  function scene(canvas, make) {
    const ctx = canvas.getContext('2d'), host = canvas.parentElement;
    const s = { canvas, ctx, host, w: 0, h: 0, visible: false, last: 0, t: 0, fx: sparks(),
      ptr: { x: -1e4, y: -1e4, vx: 0, vy: 0, on: false } };
    s.resize = () => {
      const r = canvas.getBoundingClientRect(), dpr = Math.min(devicePixelRatio || 1, 2);
      s.w = r.width; s.h = r.height;
      canvas.width = Math.round(r.width * dpr); canvas.height = Math.round(r.height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      s.layout && s.layout();
      if (still || !s.visible) s.paint(0.016);
    };
    Object.assign(s, make(s));
    if (!still) {
      let lt = 0;
      const move = (cx, cy) => {
        const r = canvas.getBoundingClientRect(), x = cx - r.left, y = cy - r.top, now = performance.now();
        const p = s.ptr, dt = Math.max((now - lt) / 1000, 0.008);
        if (p.on && now - lt < 120) { p.vx = p.vx * 0.5 + (x - p.x) / dt * 0.5; p.vy = p.vy * 0.5 + (y - p.y) / dt * 0.5; } else { p.vx = p.vy = 0; }
        p.x = x; p.y = y; p.on = true; lt = now;
      };
      const leave = () => { s.ptr.on = false; s.ptr.x = s.ptr.y = -1e4; s.ptr.vx = s.ptr.vy = 0; };
      host.addEventListener('pointermove', e => { if (e.pointerType === 'mouse') move(e.clientX, e.clientY); });
      host.addEventListener('pointerleave', e => { if (e.pointerType === 'mouse') leave(); });
      host.addEventListener('touchstart', e => move(e.touches[0].clientX, e.touches[0].clientY), { passive: true });
      host.addEventListener('touchmove', e => move(e.touches[0].clientX, e.touches[0].clientY), { passive: true });
      host.addEventListener('touchend', leave, { passive: true });
      host.addEventListener('pointerdown', e => {
        if (!s.onTap || e.target.closest('a, button, summary')) return;
        const r = canvas.getBoundingClientRect(); s.onTap(e.clientX - r.left, e.clientY - r.top);
      });
    }
    const loop = now => {
      if (!s.visible || still) return;
      const dt = Math.min((now - (s.last || now)) / 1000, 0.05);
      s.last = now; s.t += dt;
      s.paint(dt);
      s.ptr.vx *= 0.9; s.ptr.vy *= 0.9;
      requestAnimationFrame(loop);
    };
    new IntersectionObserver(es => es.forEach(e => {
      const was = s.visible; s.visible = e.isIntersecting;
      if (s.visible && !was) { s.last = 0; s.onShow && s.onShow(); requestAnimationFrame(loop); }
    }), { rootMargin: '80px' }).observe(canvas);
    scenes.push(s);
    s.resize();
    return s;
  }
  let rt; addEventListener('resize', () => { clearTimeout(rt); rt = setTimeout(() => scenes.forEach(s => s.resize()), 120); });

  // Pushes a particle away from the pointer, plus a little of the pointer's own speed.
  const stir = (s, p, reach, force, dt) => {
    const P = s.ptr; if (!P.on) return 0;
    const dx = p.x - P.x, dy = p.y - P.y, d = Math.hypot(dx, dy);
    if (d > reach || d < 0.01) return 0;
    const k = 1 - d / reach;
    p.vx += (dx / d * force + P.vx * 0.9) * k * dt; p.vy += (dy / d * force + P.vy * 0.9) * k * dt;
    return k;
  };
  // Sheds a few sparks behind a moving pointer.
  const trail = (s, color, rate) => {
    const P = s.ptr, sp = Math.hypot(P.vx, P.vy);
    if (!P.on || sp < 60) return;
    const n = Math.min(4, sp * rate);
    for (let i = 0; i < Math.ceil(n); i++) if (Math.random() < n / Math.ceil(n)) s.fx.add(P.x + rand(-3, 3), P.y + rand(-3, 3), 1, 30, color, { vx: P.vx * 0.12, vy: P.vy * 0.12, life: 1.4, r: 1.8 });
  };

  // ---- The doorway: noise scatters from your finger, dust follows it ----
  function door(s) {
    const arch = document.querySelector('.arch');
    let D = { x: 0, y: 0, w: 1, h: 1 }, stars = [], dust = [], noise = [];
    const spawnDust = (fresh) => {
      const side = Math.random() < 0.5 ? -1 : 1, x = D.x + rand(-0.38, 0.38) * D.w, y = D.y + D.h - rand(0, 0.55) * D.h;
      return { x, y, vx: side * rand(4, 26) + (x - D.x) * 0.06, vy: rand(-14, 10), life: 0, max: rand(4, 9), r: rand(0.6, 2.1), tw: rand(0, TAU), age: fresh ? rand(0, 6) : 0 };
    };
    const spawnNoise = () => {
      const left = Math.random() < 0.5, vx = (left ? 1 : -1) * rand(14, 34);
      return { x: left ? rand(-20, s.w * 0.08) : s.w - rand(-20, s.w * 0.08), y: rand(s.h * 0.05, s.h * 0.72), vx, base: vx, vy: rand(-4, 4), a: rand(0.25, 0.6), len: rand(2, 7) };
    };
    return {
      layout() {
        const c = s.canvas.getBoundingClientRect(), a = arch.getBoundingClientRect();
        D = { x: a.left - c.left + a.width / 2, y: a.top - c.top, w: a.width, h: a.height };
        stars = Array.from({ length: Math.min(Math.round(s.w * s.h / 9000), 140) }, () => ({ x: rand(0, s.w), y: rand(0, D.y + D.h * 0.9), r: rand(0.4, 1.3), tw: rand(0, TAU), sp: rand(0.4, 1.6) }));
        dust = Array.from({ length: Math.round(Math.min(90, s.w / 14)) }, () => spawnDust(true));
        noise = Array.from({ length: Math.round(Math.min(46, s.w / 30)) }, spawnNoise).map(p => { p.x += p.vx * rand(0, 8); return p; });
        if (still) dust.forEach(p => { p.age = 0; p.life = p.max * 0.4; p.x += p.vx * 2; p.y += p.vy * 2; });
      },
      onTap(x, y) {
        s.fx.add(x, y, 34, 160, colors.dust, { life: 1.1, r: 2.2 });
        const push = (p, reach, f) => { const dx = p.x - x, dy = p.y - y, d = Math.hypot(dx, dy) || 1; if (d < reach) { p.vx += dx / d * f * (1 - d / reach); p.vy += dy / d * f * (1 - d / reach); } };
        noise.forEach(p => push(p, 220, 260)); dust.forEach(p => push(p, 180, 90));
      },
      paint(dt) {
        const { ctx } = s, t = s.t;
        ctx.clearRect(0, 0, s.w, s.h);
        if (!colors.light) {
          ctx.fillStyle = '#fff';
          for (const st of stars) {
            const near = s.ptr.on ? Math.max(0, 1 - Math.hypot(st.x - s.ptr.x, st.y - s.ptr.y) / 120) : 0;
            ctx.globalAlpha = Math.min(1, 0.25 + 0.45 * (0.5 + 0.5 * Math.sin(t * st.sp + st.tw)) + near * 0.6);
            ctx.beginPath(); ctx.arc(st.x, st.y, st.r * (1 + near), 0, TAU); ctx.fill();
          }
        }
        // Noise: restless grey static, heading for the door, fading before it, fleeing your finger.
        ctx.fillStyle = colors.noise;
        for (let i = 0; i < noise.length; i++) {
          const p = noise[i];
          stir(s, p, 130, 900, dt);
          p.vx += (p.base - p.vx) * 1.5 * dt;
          p.vy += rand(-60, 60) * dt; p.vy *= 0.96;
          p.x += p.vx * dt + rand(-0.8, 0.8); p.y += p.vy * dt;
          const d = Math.abs(p.x - D.x) - D.w * 0.62, fade = Math.max(0, Math.min(1, d / (D.w * 0.6)));
          if (fade <= 0 || p.y < -20 || p.y > s.h + 20 || p.x < -60 || p.x > s.w + 60) { noise[i] = spawnNoise(); continue; }
          ctx.globalAlpha = p.a * fade * 0.75;
          ctx.fillRect(p.x, p.y, p.len, 1.4);
        }
        // Dust: warm, slow, floating out of the doorway into the light, swirling round your finger.
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        for (let i = 0; i < dust.length; i++) {
          const p = dust[i];
          if (p.age > 0) { p.age -= dt; continue; }
          p.life += dt;
          if (p.life > p.max || p.x < -40 || p.x > s.w + 40) { dust[i] = spawnDust(false); continue; }
          const k = stir(s, p, 110, 140, dt);
          p.vx *= 0.99; p.vy = p.vy * 0.985 + 3 * dt;
          p.x += p.vx * dt + Math.sin(t * 0.8 + p.tw) * 0.15; p.y += p.vy * dt;
          const q = p.life / p.max, a = Math.min(1, q * 5) * (1 - q) * (0.6 + 0.4 * Math.sin(t * 3 + p.tw));
          glowDot(ctx, p.x, p.y, p.r * (1 + k * 0.8), colors.dust, Math.min(1, a + k * 0.5));
        }
        trail(s, colors.dust, 0.004);
        s.fx.paint(ctx, dt);
        ctx.globalCompositeOperation = 'source-over'; ctx.globalAlpha = 1;
      },
    };
  }

  // ---- The four windows ----
  // Talk: bubbles drift up; push them around, tap to pop them.
  function bubbles(s) {
    let ps = [];
    const mk = (y) => ({ x: rand(0.15, 0.85) * s.w, y: y ?? s.h + 10, vx: 0, vy: 0, r: rand(3, 9), v: rand(10, 24), ph: rand(0, TAU) });
    return {
      layout() { ps = Array.from({ length: 12 }, () => mk(rand(0, s.h))); },
      onTap(x, y) {
        ps.forEach((p, i) => { if (Math.hypot(p.x - x, p.y - y) < 46) { s.fx.add(p.x, p.y, 12, 90, '#3A2508', { life: 0.8, r: 1.6 }); ps[i] = mk(); } });
        for (let i = 0; i < 3; i++) ps.push(mk(s.h + 10 + i * 18));
        if (ps.length > 24) ps.splice(0, ps.length - 24);
      },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        ctx.strokeStyle = '#3A2508'; ctx.lineWidth = 1.2;
        ps.forEach((p, i) => {
          stir(s, p, 70, 500, dt); p.vx *= 0.92; p.vy *= 0.92;
          p.y += (-p.v + p.vy) * dt; p.x += Math.sin(s.t * 1.2 + p.ph) * 0.2 + p.vx * dt;
          if (p.y < -12) ps[i] = mk();
          ctx.globalAlpha = 0.3 * Math.min(1, p.y / (s.h * 0.4));
          ctx.beginPath(); ctx.roundRect ? ctx.roundRect(p.x - p.r * 1.6, p.y - p.r, p.r * 3.2, p.r * 2, p.r) : ctx.arc(p.x, p.y, p.r, 0, TAU); ctx.stroke();
        });
        s.fx.paint(ctx, dt); ctx.globalAlpha = 1;
      },
    };
  }
  // Call: rings of sound; they lean toward you, and a tap sends out a ring of your own.
  function rings(s) {
    let c = { x: 0, y: 0 }, extra = [];
    return {
      layout() { c = { x: s.w / 2, y: s.h * 0.55 }; },
      onTap(x, y) { extra.push({ x, y, k: 0 }); },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        const want = s.ptr.on ? { x: s.w / 2 + (s.ptr.x - s.w / 2) * 0.35, y: s.h * 0.55 + (s.ptr.y - s.h * 0.55) * 0.35 } : { x: s.w / 2, y: s.h * 0.55 };
        c.x += (want.x - c.x) * Math.min(1, dt * 4); c.y += (want.y - c.y) * Math.min(1, dt * 4);
        ctx.strokeStyle = colors.gold; ctx.lineWidth = 1.2;
        for (let i = 0; i < 4; i++) {
          const k = ((s.t * 0.35 + i / 4) % 1);
          ctx.globalAlpha = (1 - k) * (s.ptr.on ? 0.75 : 0.5); ctx.beginPath(); ctx.arc(c.x, c.y, k * s.w * 0.62, 0, TAU); ctx.stroke();
        }
        ctx.lineWidth = 2;
        extra = extra.filter(r => (r.k += dt * 0.8) < 1);
        extra.forEach(r => { ctx.globalAlpha = (1 - r.k) * 0.8; ctx.beginPath(); ctx.arc(r.x, r.y, r.k * s.w * 0.8, 0, TAU); ctx.stroke(); });
        ctx.globalAlpha = 1;
      },
    };
  }
  // Gather: specks circle in to the middle, or to wherever your finger is; a tap scatters them.
  function gather(s) {
    let ps = [], c = { x: 0, y: 0 };
    const mk = () => ({ a: rand(0, TAU), d: rand(0.55, 0.75), v: rand(0.08, 0.16), r: rand(0.8, 1.8) });
    return {
      layout() { c = { x: s.w / 2, y: s.h * 0.55 }; ps = Array.from({ length: 34 }, () => { const p = mk(); p.d = rand(0.1, 0.75); return p; }); },
      onTap(x, y) { s.fx.add(x, y, 26, 140, colors.dust, { life: 1, r: 2 }); ps.forEach(p => { p.d = Math.min(0.8, p.d + rand(0.2, 0.4)); }); },
      paint(dt) {
        const { ctx } = s, R = s.w; ctx.clearRect(0, 0, s.w, s.h);
        const want = s.ptr.on ? s.ptr : { x: s.w / 2, y: s.h * 0.55 };
        c.x += (want.x - c.x) * Math.min(1, dt * 5); c.y += (want.y - c.y) * Math.min(1, dt * 5);
        const fast = s.ptr.on ? 1.8 : 1;
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        ps.forEach((p, i) => {
          p.d -= p.v * dt * fast; p.a += dt * 0.9 * fast / (p.d + 0.2);
          if (p.d < 0.04) { ps[i] = mk(); return; }
          glowDot(ctx, c.x + Math.cos(p.a) * p.d * R, c.y + Math.sin(p.a) * p.d * R, p.r, colors.dust, Math.min(1, (0.8 - p.d) * 3) * 0.8);
        });
        if (s.ptr.on) glowDot(ctx, c.x, c.y, 2.5, colors.dust, 0.6 + 0.3 * Math.sin(s.t * 6));
        s.fx.paint(ctx, dt);
        ctx.globalCompositeOperation = 'source-over'; ctx.globalAlpha = 1;
      },
    };
  }
  // Disappear: embers rise; moving fans them hotter, a tap makes them flare.
  function embers(s) {
    let ps = [], heat = 0;
    const mk = (y) => ({ x: rand(0.2, 0.8) * s.w, y: y ?? s.h + 4, vx: 0, vy: 0, v: rand(18, 40), r: rand(0.8, 2.2), ph: rand(0, TAU) });
    return {
      layout() { ps = Array.from({ length: 28 }, () => mk(rand(s.h * 0.3, s.h))); },
      onTap(x, y) { heat = 1; s.fx.add(x, y, 30, 170, '#FF8A3D', { dir: -Math.PI / 2, spread: 1.1, life: 1, r: 2, g: -20 }); },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        heat = Math.max(0, Math.min(1, heat + Math.hypot(s.ptr.vx, s.ptr.vy) / 4000 - dt * 0.6));
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        ps.forEach((p, i) => {
          stir(s, p, 60, 300, dt); p.vx *= 0.93; p.vy *= 0.93;
          p.y += (-p.v * (1 + heat * 1.5) + p.vy) * dt; p.x += Math.sin(s.t * 2 + p.ph) * 0.35 + p.vx * dt;
          if (p.y < s.h * 0.05 || p.x < -10 || p.x > s.w + 10) { ps[i] = mk(); return; }
          const k = p.y / s.h, hot = k > 0.6 - heat * 0.4;
          glowDot(ctx, p.x, p.y, p.r * k * (1 + heat), hot ? '#FF8A3D' : colors.dust, Math.min(1, (k - 0.05) * 2) * (0.6 + 0.4 * Math.sin(s.t * 9 + p.ph)));
        });
        s.fx.paint(ctx, dt);
        ctx.globalCompositeOperation = 'source-over'; ctx.globalAlpha = 1;
      },
    };
  }

  // ---- A message's trip: your phone, three random stops, Joe's phone. Tap to send another. ----
  function route(s) {
    let nodes = [], path = [], k = 0, tail = [], pause = 0;
    const pick = () => {
      path = [0.3, 0.5, 0.7].map(c => {
        const near = nodes.filter(n => Math.abs(n.hx / s.w - c) < 0.1);
        const n = near[Math.floor(Math.random() * near.length)];
        return n ? { x: n.hx, y: n.hy } : { x: c * s.w, y: s.h * 0.5 };
      });
      path = [{ x: s.w * 0.08, y: s.h * 0.5 }, ...path, { x: s.w * 0.92, y: s.h * 0.5 }];
      k = 0; tail = [];
    };
    const at = u => {
      const seg = Math.min(path.length - 2, Math.floor(u)), f = u - seg, a = path[seg], b = path[seg + 1];
      const e = f < 0.5 ? 2 * f * f : 1 - (-2 * f + 2) ** 2 / 2;
      return { x: a.x + (b.x - a.x) * e, y: a.y + (b.y - a.y) * e };
    };
    const phone = (ctx, x, y) => {
      ctx.globalAlpha = 1; ctx.strokeStyle = colors.text; ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.roundRect ? ctx.roundRect(x - 11, y - 19, 22, 38, 5) : ctx.rect(x - 11, y - 19, 22, 38); ctx.stroke();
      ctx.fillStyle = colors.gold; ctx.beginPath(); ctx.arc(x, y + 12, 1.6, 0, TAU); ctx.fill();
    };
    return {
      layout() {
        nodes = Array.from({ length: 26 }, () => { const x = rand(0.2, 0.8) * s.w, y = rand(0.28, 0.66) * s.h; return { x, y, hx: x, hy: y, r: rand(1.5, 3) }; });
        pick(); if (still) k = path.length - 1.001;
      },
      onTap() { pick(); pause = 0; s.fx.add(path[0].x, path[0].y, 14, 80, colors.dust, { life: 0.8 }); },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        // The stops shy away from your finger and spring back.
        ctx.fillStyle = colors.faint;
        nodes.forEach(n => {
          let tx = n.hx, ty = n.hy;
          if (s.ptr.on) { const dx = n.hx - s.ptr.x, dy = n.hy - s.ptr.y, d = Math.hypot(dx, dy); if (d < 90 && d > 0.1) { tx += dx / d * (90 - d) * 0.5; ty += dy / d * (90 - d) * 0.5; } }
          n.x += (tx - n.x) * Math.min(1, dt * 8); n.y += (ty - n.y) * Math.min(1, dt * 8);
          ctx.globalAlpha = 0.35; ctx.beginPath(); ctx.arc(n.x, n.y, n.r, 0, TAU); ctx.fill();
        });
        // The path so far, drawn as it's travelled.
        const head = at(Math.min(k, path.length - 1.0001));
        ctx.strokeStyle = colors.gold; ctx.lineWidth = 1.2; ctx.setLineDash([3, 5]); ctx.globalAlpha = 0.55;
        ctx.beginPath(); ctx.moveTo(path[0].x, path[0].y);
        for (let i = 1; i <= Math.floor(k); i++) ctx.lineTo(path[i].x, path[i].y);
        ctx.lineTo(head.x, head.y); ctx.stroke(); ctx.setLineDash([]);
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        for (let i = 1; i < path.length - 1; i++) if (k >= i - 0.02) glowDot(ctx, path[i].x, path[i].y, 3, colors.dust, 0.9);
        if (pause > 0) { pause -= dt; if (pause <= 0) pick(); }
        else if (!still) {
          k += dt * 0.85;
          tail.push({ ...head, a: 1 });
          if (k >= path.length - 1) { k = path.length - 1.0001; pause = 1.6; const e = path[path.length - 1]; s.fx.add(e.x, e.y, 22, 70, colors.dust, { life: 0.9, r: 1.6 }); }
        }
        tail = tail.filter(p => (p.a -= dt * 1.6) > 0);
        tail.forEach(p => glowDot(ctx, p.x, p.y, 1.6 * p.a, colors.dust, p.a * 0.6));
        if (pause <= 0) glowDot(ctx, head.x, head.y, 3.4, colors.dust, 1);
        s.fx.paint(ctx, dt);
        ctx.globalCompositeOperation = 'source-over';
        phone(ctx, path[0].x, path[0].y); phone(ctx, path[path.length - 1].x, path[path.length - 1].y);
        ctx.globalAlpha = 1;
      },
    };
  }

  // ---- Two sigils meet and make the chat's sigil. Tap a sigil to throw its colour, ----
  // ---- tap the chat's sigil to replay; the flying specks dodge your finger.        ----
  function merge(s) {
    const eq = s.host, figs = [...eq.querySelectorAll('figure > div')], result = eq.querySelector('.result > div');
    let P = [], ps = [], run = -1, played = false;
    const colorOf = el => { const n = el.querySelector('[stroke]:not([stroke="none"]), [fill]:not([fill="none"])'); return n ? (n.getAttribute('stroke') || n.getAttribute('fill')) : colors.dust; };
    const start = () => {
      if (still) return;
      run = 0; ps = []; result.style.opacity = '0.12'; result.style.transform = 'scale(.86)';
      [0, 1].forEach(i => { const c = colorOf(figs[i]); for (let j = 0; j < 46; j++) ps.push({ from: i, c, delay: rand(0, 1.3), dur: rand(1.1, 1.7), ox: rand(-0.35, 0.35), oy: rand(-0.35, 0.35), bend: rand(-60, 60), r: rand(0.8, 2), dx: 0, dy: 0, vx: 0, vy: 0, x: 0, y: 0 }); });
    };
    const replay = document.getElementById('replay');
    replay && replay.addEventListener('click', start);
    if (still && replay) replay.hidden = true;
    return {
      layout() {
        const c = s.canvas.getBoundingClientRect();
        P = figs.map(f => { const r = f.getBoundingClientRect(); return { x: r.left - c.left + r.width / 2, y: r.top - c.top + r.height / 2, s: r.width }; });
      },
      onShow() { if (!played) { played = true; setTimeout(start, 250); } },
      onTap(x, y) {
        const i = P.findIndex(p => Math.hypot(p.x - x, p.y - y) < p.s * 0.6);
        if (i === 2) return start();
        if (i < 0) return;
        const T = P[2], dir = Math.atan2(T.y - P[i].y, T.x - P[i].x);
        s.fx.add(P[i].x, P[i].y, 30, 260, colorOf(figs[i]), { dir, spread: 0.35, life: 1.1, r: 2 });
      },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        s.fx.paint(ctx, dt);
        if (run >= 0) {
          run += dt;
          const T = P[2];
          let arrived = 0;
          ps.forEach(p => {
            const a = P[p.from], u = Math.max(0, Math.min(1, (run - p.delay) / p.dur));
            if (u >= 1) { arrived++; return; }
            if (u <= 0) return;
            const e = u * u * (3 - 2 * u), sx = a.x + p.ox * a.s, sy = a.y + p.oy * a.s;
            p.x = sx + (T.x - sx) * e + p.dx; p.y = sy + (T.y - sy) * e - Math.sin(e * Math.PI) * (50 + p.bend) + p.dy;
            stir(s, p, 70, 600, dt);
            p.dx += p.vx * dt; p.dy += p.vy * dt; p.vx *= 0.9; p.vy *= 0.9;
            p.dx *= 1 - Math.min(1, dt * 3); p.dy *= 1 - Math.min(1, dt * 3);
            glowDot(ctx, p.x, p.y, p.r, p.c, Math.sin(u * Math.PI) * 0.9 + 0.1);
          });
          const k = arrived / ps.length;
          result.style.opacity = String(0.12 + 0.88 * k); result.style.transform = `scale(${0.86 + 0.14 * k})`;
          if (k === 1) {
            s.fx.add(T.x, T.y, 20, 130, colorOf(figs[0]), { life: 1.1, g: 20 }); s.fx.add(T.x, T.y, 20, 130, colorOf(figs[1]), { life: 1.1, g: 20 });
            run = -1; result.style.opacity = ''; result.style.transform = '';
          }
        }
        ctx.globalCompositeOperation = 'source-over'; ctx.globalAlpha = 1;
      },
    };
  }

  // ---- The closing: a hearth of slow gold dust; your finger leaves a trail, a tap lights a spark ----
  function hearth(s) {
    let ps = [];
    const mk = (y) => ({ x: s.w / 2 + rand(-0.32, 0.32) * s.w * Math.random(), y: y ?? s.h + 6, vx: 0, vy: 0, v: rand(10, 28), r: rand(0.6, 2), ph: rand(0, TAU) });
    return {
      layout() { ps = Array.from({ length: Math.round(Math.min(70, s.w / 16)) }, () => mk(rand(0, s.h))); },
      onTap(x, y) { s.fx.add(x, y, 40, 190, colors.dust, { life: 1.4, r: 2.2, g: -10 }); },
      paint(dt) {
        const { ctx } = s; ctx.clearRect(0, 0, s.w, s.h);
        if (!colors.light) ctx.globalCompositeOperation = 'lighter';
        ps.forEach((p, i) => {
          stir(s, p, 100, 200, dt); p.vx *= 0.95; p.vy *= 0.95;
          p.y += (-p.v + p.vy) * dt; p.x += Math.sin(s.t * 0.7 + p.ph) * 0.3 + p.vx * dt;
          if (p.y < -6 || p.x < -10 || p.x > s.w + 10) { ps[i] = mk(); return; }
          glowDot(ctx, p.x, p.y, p.r, colors.dust, (p.y / s.h) * (0.5 + 0.5 * Math.sin(s.t * 2 + p.ph)));
        });
        trail(s, colors.dust, 0.005);
        s.fx.paint(ctx, dt);
        ctx.globalCompositeOperation = 'source-over'; ctx.globalAlpha = 1;
      },
    };
  }

  const kinds = { door, bubbles, rings, gather, embers, route, merge, hearth };
  const boot = () => document.querySelectorAll('canvas[data-fx]').forEach(c => kinds[c.dataset.fx] && scene(c, kinds[c.dataset.fx]));
  document.fonts && document.fonts.ready ? document.fonts.ready.then(boot) : addEventListener('load', boot);
})();
