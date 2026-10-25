function makeAdder(k) { return (x) => x + k; }
let total = 0;
for (let i = 0; i < 1000000; i++) {
  const add = makeAdder(i % 10);
  total += add(i);
}
console.log(total);
