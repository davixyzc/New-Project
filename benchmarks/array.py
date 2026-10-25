nums = []
for i in range(1000000):
    nums.append(i)
total = sum(x for x in (y * 2 for y in nums) if x % 3 == 0)
print(total)
