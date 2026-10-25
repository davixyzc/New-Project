let x = 12345;
const arr = [];
for (let i = 0; i < 200000; i++) {
  x = (x * 1103515245 + 12345) % 2147483648;
  arr.push(x % 100000);
}
arr.sort((a, b) => a - b);
let check = 0;
for (let i = 0; i < arr.length; i += 1000) check += arr[i];
console.log(arr.length, arr[0], arr[arr.length - 1], check);
