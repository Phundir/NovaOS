import os
name = "_essai_nova.txt"
with open(name, "w") as f:
    f.write("ligne un\n")
    f.write("ligne deux\n")
    print("ecrit", f.write("trois"))
with open(name) as f:
    print(repr(f.read()))
with open(name) as f:
    print(f.readline(), end="")
    print(f.readlines())
with open(name, "a") as f:
    f.write("\nquatre\n")
with open(name) as f:
    for i, line in enumerate(f):
        print(i, line.rstrip())
print(os.path.exists(name), os.path.isfile(name), os.path.isdir(name), os.path.getsize(name))
print(os.path.splitext("dossier/fichier.tar.gz"), os.path.basename("/a/b/c.txt"), os.path.dirname("/a/b/c.txt"), os.path.join("a", "b", "c"))
print(os.path.normpath("a/./b/../c//d"), os.path.split("/x/y"), os.path.isabs("/x"), os.path.splitext(".bashrc"))
os.remove(name)
print(os.path.exists(name))
try:
    open("_inexistant.txt")
except FileNotFoundError as e:
    print(type(e).__name__, e.errno, e.strerror, e.filename)
    print(e)
try:
    open(name, "z")
except ValueError as e:
    print(e)
with open(name, "wb") as f:
    f.write(b"\x00\x01binaire")
with open(name, "rb") as f:
    data = f.read()
print(data, len(data), type(data).__name__)
os.remove(name)
f = open(name, "w"); f.write("sans close")
f2 = open(name); print(repr(f2.read())); f2.close()
f.close(); os.remove(name)
os.makedirs("_dossier_nova/sous", exist_ok=True)
print(sorted(os.listdir("_dossier_nova")))
os.rmdir("_dossier_nova/sous"); os.rmdir("_dossier_nova")
print(os.path.exists("_dossier_nova"))
