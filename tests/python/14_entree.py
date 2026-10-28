nom = input("Nom ? ")
age = int(input())
print(f"Bonjour {nom}, {age} ans")
total = 0
while True:
    try:
        ligne = input()
    except EOFError:
        break
    total += sum(int(x) for x in ligne.split())
print("total", total)
