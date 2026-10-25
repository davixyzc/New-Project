const n = 120;
const a = [], b = [], c = [];
for (let i = 0; i < n; i++) {
  const ra = [], rb = [], rc = [];
  for (let j = 0; j < n; j++) { ra.push((i * j) % 7); rb.push((i + j) % 5); rc.push(0); }
  a.push(ra); b.push(rb); c.push(rc);
}
for (let i = 0; i < n; i++) {
  for (let j = 0; j < n; j++) {
    let s = 0;
    for (let k = 0; k < n; k++) s += a[i][k] * b[k][j];
    c[i][j] = s;
  }
}
let t = 0;
for (let i = 0; i < n; i++) t += c[i][(i * 7) % n];
console.log(t);
