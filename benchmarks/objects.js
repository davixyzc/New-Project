let acc = 0;
for (let i = 0; i < 1000000; i++) {
  const p = { x: i, y: i + 1 };
  acc += p.x + p.y;
}
console.log(acc);
