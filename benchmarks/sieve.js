const N = 2000000;
const flags = [];
for (let i = 0; i <= N; i++) flags.push(true);
let count = 0;
for (let i = 2; i <= N; i++) {
  if (flags[i]) {
    count++;
    for (let j = i * 2; j <= N; j += i) flags[j] = false;
  }
}
console.log(count);
