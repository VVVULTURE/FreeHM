#!/usr/bin/env python3
"""Should a push to main publish a release? Yes when the app's version is above every tag.

Usage: git ls-remote --tags --refs origin 'v*' | tools/ci/should-publish.py VERSION
Prints true or false.
"""
import re
import sys


def key(version):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-(.+))?", version)
    if not m:
        return None
    # A release sorts above its own pre-releases.
    return (int(m[1]), int(m[2]), int(m[3]), m[4] is None, m[4] or "")


def should_publish(version, ls_remote):
    mine = key(version)
    if mine is None:
        raise SystemExit(f"'{version}' is not a version")
    tags = [line.rsplit("refs/tags/v", 1)[-1].strip() for line in ls_remote.splitlines() if "refs/tags/v" in line]
    keys = [k for k in map(key, tags) if k is not None]
    return all(mine > k for k in keys)


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] == "--self-test":
        def refs(*tags):
            return "".join(f"abc\trefs/tags/v{t}\n" for t in tags)

        assert should_publish("0.2.0", "")
        assert should_publish("0.2.0", refs("0.1.0-rc3"))
        assert should_publish("0.2.0", refs("0.2.0-rc1"))
        assert should_publish("0.10.0", refs("0.9.0"))
        assert should_publish("0.2.1", refs("0.2.0", "0.1.0"))
        assert should_publish("0.2.0-rc2", refs("0.2.0-rc1"))
        assert not should_publish("0.2.0", refs("0.2.0"))
        assert not should_publish("0.1.9", refs("0.2.0"))
        assert not should_publish("0.9.0", refs("0.10.0"))
        assert not should_publish("0.2.0-rc1", refs("0.2.0"))
        assert not should_publish("0.2.0", refs("0.1.0", "0.2.0", "0.3.0"))
        print("ok")
    else:
        print("true" if should_publish(sys.argv[1], sys.stdin.read()) else "false")
