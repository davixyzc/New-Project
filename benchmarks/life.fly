const W = 120, H = 120;
let grid = [];
for (let y = 0; y < H; y++) {
  const row = [];
  for (let x = 0; x < W; x++) row.push((x * 7 + y * 13) % 5 === 0 ? 1 : 0);
  grid.push(row);
}
for (let gen = 0; gen < 200; gen++) {
  const next = [];
  for (let y = 0; y < H; y++) {
    const row = [];
    for (let x = 0; x < W; x++) {
      let n = 0;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          if (dx === 0 && dy === 0) continue;
          const yy = (y + dy + H) % H;
          const xx = (x + dx + W) % W;
          n += grid[yy][xx];
        }
      }
      const alive = grid[y][x];
      row.push(alive ? (n === 2 || n === 3 ? 1 : 0) : (n === 3 ? 1 : 0));
    }
    next.push(row);
  }
  grid = next;
}
let live = 0;
for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) live += grid[y][x];
console.log(live);
