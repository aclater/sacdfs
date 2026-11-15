#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Pack the Windows release zip (#419): pack-windows.py NAME EXE FILE[=NAME_IN_ZIP]...

Writes dist/NAME.zip, and dist/NAME.zip.sha256 as `sha256sum` writes it, holding NAME/sacdfs-win.exe and NAME/<each FILE's name, or the name after
`=`>. Reproducible: entries in a fixed order, each dated at the
commit's time (SOURCE_DATE_EPOCH, else `git log -1`), with fixed attributes."""
import hashlib
import os
import subprocess
import sys
import time
import zipfile


def main():
    name, exe, files = sys.argv[1], sys.argv[2], sys.argv[3:]
    epoch = os.environ.get("SOURCE_DATE_EPOCH") or subprocess.run(
        ["git", "log", "-1", "--format=%ct"], capture_output=True, text=True, check=True).stdout.strip()
    # Zip dates hold 1980 to 2107, in local time without a zone: use UTC.
    date = time.gmtime(int(epoch))[:6]
    entries = [("sacdfs-win.exe", exe)]
    for f in files:
        src, _, arc = f.partition("=")
        entries.append((arc or os.path.basename(src), src))
    os.makedirs("dist", exist_ok=True)
    with zipfile.ZipFile(f"dist/{name}.zip", "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for arc, src in sorted(entries):
            info = zipfile.ZipInfo(f"{name}/{arc}", date_time=date)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 0
            info.external_attr = 0
            with open(src, "rb") as f:
                z.writestr(info, f.read())
    with open(f"dist/{name}.zip", "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    # LF line ends: `sha256sum -c` reads the file on Linux too.
    with open(f"dist/{name}.zip.sha256", "w", encoding="ascii", newline="\n") as f:
        f.write(f"{digest}  {name}.zip\n")
    print(f"dist/{name}.zip: {', '.join(sorted(a for a, _ in entries))}\n{digest}  {name}.zip")


if __name__ == "__main__":
    main()
