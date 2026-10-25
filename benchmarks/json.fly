const data = [];
for (let i = 0; i < 20000; i++) {
  data.push({ id: i, name: "item" + i, tags: ["a", "b"], score: i * 1.5 });
}
let total = 0;
for (let r = 0; r < 5; r++) {
  const text = JSON.stringify(data);
  const back = JSON.parse(text);
  total += back.length + text.length;
}
console.log(total);
