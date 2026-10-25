acc = 0
for i in range(1000000):
    p = {"x": i, "y": i + 1}
    acc += p["x"] + p["y"]
print(acc)
