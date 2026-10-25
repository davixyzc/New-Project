function make(depth) {
  if (depth === 0) return { l: null, r: null };
  return { l: make(depth - 1), r: make(depth - 1) };
}
function check(t) {
  if (t.l === null) return 1;
  return 1 + check(t.l) + check(t.r);
}
let total = 0;
for (let i = 0; i < 20; i++) total += check(make(14));
console.log(total);
