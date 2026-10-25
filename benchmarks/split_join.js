const parts = [];
for (let i = 0; i < 200000; i++) parts.push("w" + (i % 1000));
const text = parts.join(",");
const back = text.split(",");
let total = 0;
for (let i = 0; i < back.length; i++) total += back[i].length;
console.log(text.length, back.length, total);
