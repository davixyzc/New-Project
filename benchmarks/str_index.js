const s = "abcdefghij".repeat(20000);
let n = 0;
for (let i = 0; i < s.length; i++) if (s[i] === "e") n++;
console.log(s.length, n);
