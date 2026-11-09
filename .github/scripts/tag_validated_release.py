#!/usr/bin/env python3
"""Tag the exact scientifically validated commit; never move an existing tag."""
import os
from pathlib import Path
import re
import subprocess


def git(directory, *args, check=True):
    return subprocess.run(["git", *args], cwd=directory, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=check)


def prepare_tag(directory, version, commit, *, sign=True):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("release requires a stable semantic version")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("release requires an exact commit SHA")
    if git(directory, "rev-parse", "HEAD").stdout.strip() != commit:
        raise ValueError("checkout differs from the scientifically validated commit")
    ref = f"refs/tags/{version}"
    existing = git(directory, "rev-parse", "--verify", ref, check=False)
    if existing.returncode == 0:
        target = git(directory, "rev-parse", ref + "^{commit}").stdout.strip()
        if target != commit:
            raise ValueError("existing release tag points to another commit; refusing to move it")
        if sign:
            git(directory, "verify-tag", version)
    else:
        git(directory, "tag", "-s" if sign else "-a", version, commit, "-m", f"Release {version}")


if __name__ == "__main__":
    directory = Path.cwd()
    version, commit = os.environ["VERSION"], os.environ["COMMIT"]
    prepare_tag(directory, version, commit)
    # Authentication is configured by the caller. No --force: concurrent or
    # conflicting publications must fail rather than replace a signed tag.
    git(directory, "push", "origin", f"refs/tags/{version}")
    print(f"Published signed tag {version} at validated commit {commit}")
