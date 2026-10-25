let count = 0;
const N = 300;
for (let py = 0; py < N; py++) {
  for (let px = 0; px < N; px++) {
    const cx = (px / N) * 3.5 - 2.5;
    const cy = (py / N) * 2 - 1;
    let x = 0, y = 0, it = 0;
    while (x * x + y * y < 4 && it < 100) {
      const xt = x * x - y * y + cx;
      y = 2 * x * y + cy;
      x = xt;
      it++;
    }
    if (it === 100) count++;
  }
}
console.log(count);
