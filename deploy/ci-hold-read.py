#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""CI helper: reads a file through a share across a server restart.

    ci-hold-read.py FILE STATE_DIR

Opens FILE, reads its first half, writes STATE_DIR/half and waits for STATE_DIR/go (the
server restarts in between). Then it reads the second half through the same open file,
stats the file and lists its directory again by path, reads the whole file afresh, and
writes STATE_DIR/result: "ok", or what failed. A client's file handle that survives the
restart is what persistent file identity (#18) is for.
"""
import os
import sys
import time

path, state = sys.argv[1], sys.argv[2]


def result(text: str) -> None:
    with open(os.path.join(state, "result"), "w") as f:
        f.write(text + "\n")


try:
    fd = os.open(path, os.O_RDONLY)
    size = os.fstat(fd).st_size
    first = os.pread(fd, size // 2, 0)
    open(os.path.join(state, "half"), "w").close()
    while not os.path.exists(os.path.join(state, "go")):
        time.sleep(0.2)
    errors = []
    try:
        second = b""
        while len(first) + len(second) < size:
            chunk = os.pread(fd, size - len(first) - len(second), len(first) + len(second))
            if not chunk:
                break
            second += chunk
    except OSError as e:
        errors.append(f"read through the open file: {e}")
        second = None
    try:
        os.stat(path)
        os.listdir(os.path.dirname(path))
    except OSError as e:
        errors.append(f"stat or list by path: {e}")
    try:
        with open(path, "rb") as f:
            fresh = f.read()
        if second is not None and first + second != fresh:
            errors.append("the bytes read across the restart differ from a fresh read")
    except OSError as e:
        errors.append(f"fresh read: {e}")
    result("ok" if not errors else "; ".join(errors))
except Exception as e:  # noqa: BLE001 - the CI step reports whatever went wrong
    result(f"failed before the restart: {e}")
