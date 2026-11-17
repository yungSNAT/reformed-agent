"""Find a program on PATH without searching the working folder.

Skill scripts run inside whatever folder the agent is working in, which can be
an untrusted checkout. On Windows, ``shutil.which`` and a bare-name
``subprocess`` call both search that folder before PATH, and on every platform
a relative PATH entry (an empty one included) points into it. ``find_program``
searches absolute PATH entries only; pass the path it returns to
``subprocess``, never the bare name.

Skills install separately, so each skill that starts programs keeps its own
identical copy of this file; ``tests/skills/test_skill_program_lookup.py``
checks that the copies match.
"""

from __future__ import annotations

import os

_DEFAULT_PATHEXT = ".COM;.EXE;.BAT;.CMD"


def find_program(name: str) -> str | None:
    """Return the absolute path of program ``name``, or None when absent."""
    names = [name]
    if os.name == "nt":
        exts = [e for e in (os.environ.get("PATHEXT") or _DEFAULT_PATHEXT).split(os.pathsep) if e]
        if os.path.splitext(name)[1].upper() not in {e.upper() for e in exts}:
            names = [name + ext for ext in exts]
    for entry in os.environ.get("PATH", os.defpath).split(os.pathsep):
        if not os.path.isabs(entry):
            continue
        for candidate in names:
            path = os.path.join(entry, candidate)
            if os.path.isfile(path) and os.access(path, os.X_OK):
                return path
    return None
