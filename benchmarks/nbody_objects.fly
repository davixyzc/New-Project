const bodies = [];
for (let i = 0; i < 50; i++) bodies.push({ x: i, y: i * 2, vx: 0, vy: 0, m: 1 + i % 3 });
for (let step = 0; step < 400; step++) {
  for (let i = 0; i < bodies.length; i++) {
    const a = bodies[i];
    for (let j = 0; j < bodies.length; j++) {
      if (i === j) continue;
      const b = bodies[j];
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const d2 = dx * dx + dy * dy + 0.01;
      const f = b.m / (d2 * Math.sqrt(d2));
      a.vx += dx * f * 0.001;
      a.vy += dy * f * 0.001;
    }
  }
  for (const b of bodies) { b.x += b.vx * 0.001; b.y += b.vy * 0.001; }
}
let e = 0;
for (const b of bodies) e += b.x + b.y;
console.log(e.toFixed(6));
