"""Find a program on PATH without searching the working folder.

Skill scripts run inside whatever folder the agent is working in, which can be
an untrusted checkout. On Windows, ``shutil.which`` and a bare-name
``subprocess`` call both search that folder before PATH, and on every platform
a relative PATH entry (an empty one included) points into it. ``find_program``
searches fully qualified PATH entries only; pass the path it returns to
``subprocess``, never the bare name.

On Windows it returns no batch file (.bat or .cmd): those run through cmd.exe,
which reinterprets their arguments, and a bare-name ``subprocess`` call never
started them either.

Skills install separately, so each skill that starts programs keeps its own
identical copy of this file; ``tests/skills/test_skill_program_lookup.py``
checks that the copies match.
"""

from __future__ import annotations

import os

_DEFAULT_PATHEXT = ".COM;.EXE"
_BATCH = {".BAT", ".CMD"}


def is_fully_qualified(path: str) -> bool:
    """Whether ``path`` names one place whatever the working folder and drive.

    On Windows before Python 3.13, ``os.path.isabs`` also accepts a rooted
    path without a drive, such as ``\\bin``, which depends on the current drive.
    """
    if not os.path.isabs(path):
        return False
    return os.name != "nt" or bool(os.path.splitdrive(path)[0])


def find_program(name: str) -> str | None:
    """Return the absolute path of program ``name``, or None when absent."""
    names = [name]
    if os.name == "nt":
        pathext = os.environ.get("PATHEXT") or _DEFAULT_PATHEXT
        exts = [e for e in pathext.split(os.pathsep) if e and e.upper() not in _BATCH]
        ext = os.path.splitext(name)[1].upper()
        if ext in _BATCH:
            return None
        if ext not in {e.upper() for e in exts}:
            names = [name + e for e in exts]
    for entry in os.environ.get("PATH", os.defpath).split(os.pathsep):
        if not is_fully_qualified(entry):
            continue
        for candidate in names:
            path = os.path.join(entry, candidate)
            if os.path.isfile(path) and os.access(path, os.X_OK):
                return path
    return None
