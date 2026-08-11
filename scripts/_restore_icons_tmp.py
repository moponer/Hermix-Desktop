import subprocess, hashlib
files = {
    "128x128.png":   "16e097863538239d661f7d7744a2fa125b0a473b",
    "128x128@2x.png":"387a89d1aa69df39d8688cac28188913261e4a5f",
    "32x32.png":     "7aa62ad808c86e5e90880049addd40c95e2aaa02",
    "icon.icns":     "9c1d54b9f162c705aa41f20474e49e59d793b014",
    "icon.ico":      "3ed41b6666e103153ef108721bc88ef265fcb1fd",
    "icon.png":      "f068fedb9095d589a828ee76612f812b551ef8bb",
}
for f, sha in files.items():
    data = subprocess.check_output(["git", "cat-file", "blob", sha], cwd=r"<repo>")
    path = rf"<repo>/apps/desktop-tauri/src-tauri/icons/{f}"
    with open(path, "wb") as fp:
        fp.write(data)
    actual = hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()
    print(("OK " if actual == sha else "BAD"), f, actual)
