const out = [];
for (let i = 0; i < 300000; i++) out.push("row " + i + ";");
const text = out.join("");
console.log(text.length);
