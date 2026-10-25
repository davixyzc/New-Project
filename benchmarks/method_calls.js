const counter = { n: 0, add(k) { this.n += k; } };
for (let i = 0; i < 2000000; i++) counter.add(i % 3);
console.log(counter.n);
