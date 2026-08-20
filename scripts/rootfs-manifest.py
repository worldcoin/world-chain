#!/usr/bin/env python3
"""Print a stable per-entry manifest of a `docker export` tar stream, read from stdin.

One line per entry: `<sha256|kind> <mode> <uid> <gid> <size> <mtime> <name>`, sorted by name.
Diffing two manifests from two builds of the same source pinpoints which rootfs entries are
not reproducible, and whether the difference is content or metadata.
"""

import hashlib
import sys
import tarfile


def main() -> None:
    lines = []
    with tarfile.open(fileobj=sys.stdin.buffer, mode="r|*") as tar:
        for m in tar:
            if m.isfile():
                f = tar.extractfile(m)
                h = hashlib.sha256()
                if f is not None:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                kind = h.hexdigest()
            elif m.issym() or m.islnk():
                kind = "link->" + m.linkname
            elif m.isdir():
                kind = "dir"
            else:
                kind = "type%s" % m.type.decode(errors="replace")
            lines.append(
                "%s %o %d %d %d %d %s"
                % (kind, m.mode, m.uid, m.gid, m.size, m.mtime, m.name)
            )
    lines.sort(key=lambda s: s.rsplit(" ", 1)[-1])
    sys.stdout.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
