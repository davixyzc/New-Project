let caught = 0;
for (let i = 0; i < 200000; i++) {
  try {
    if (i % 3 === 0) throw new Error("bad " + i);
    caught += 1;
  } catch (e) {
    caught += e.message.length;
  }
}
console.log(caught);
