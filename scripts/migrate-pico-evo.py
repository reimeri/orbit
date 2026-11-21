#!/usr/bin/env python3
"""One-time, offline copy from pico-evo to Orbit. Never invoked by the app."""
import fcntl
import json
import os
import shutil
import sqlite3
import sys
import tempfile
from contextlib import closing
from pathlib import Path


TAGS = {f"pico_{tag}": f"orbit_{tag}" for tag in (
    "media", "file", "skill", "project_instructions", "skill_catalog"
)}


def convert_db(path, source, destination):
    # Include pending submissions/results and historical fork deltas, not just
    # the visible transcript. Never search-and-replace prose or file contents.
    columns = {
        "new_session_draft": ("data",),
        "sessions": ("metadata",),
        "entries": ("data",),
        "context": ("data",),
        "fork_points": ("entries", "input"),
    }
    with closing(sqlite3.connect(path.as_uri() + "?mode=rw", uri=True)) as db, db:
        for table, names in columns.items():
            for column in names:
                for rowid, raw in db.execute(f"SELECT rowid, {column} FROM {table} ORDER BY rowid"):
                    changed = False

                    def convert(obj):
                        nonlocal changed
                        tag = obj.get("type")
                        if isinstance(tag, str) and tag in TAGS:
                            obj["type"] = TAGS[tag]
                            changed = True
                        fields = ()
                        if "mime" in obj and "bytes" in obj:
                            fields = ("path", "prepared", "thumbnail")
                        elif "snapshot" in obj and "spill" in obj:
                            fields = ("snapshot",)
                        for key in fields:
                            value = obj.get(key)
                            if isinstance(value, str) and value.startswith(str(source) + "/"):
                                obj[key] = str(destination) + value[len(str(source)):]
                                changed = True
                        return obj

                    value = json.loads(raw, object_hook=convert)
                    if changed:
                        db.execute(f"UPDATE {table} SET {column}=? WHERE rowid=?",
                                   (json.dumps(value, separators=(",", ":")), rowid))


def migrate():
    xdg = Path(os.environ.get("XDG_CONFIG_HOME", ""))
    config = xdg if xdg.is_absolute() else Path.home() / ".config"
    source, destination = config / "pico-evo", config / "orbit"
    if os.path.lexists(destination):
        sys.exit(f"{destination} already exists. Close Orbit and move it aside; no merge was attempted.")
    with (source / "instance.lock").open("rb") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            sys.exit(f"Close the app using {source}, then retry.")
        # Stage beside the destination so publication is a single rename.
        with tempfile.TemporaryDirectory(dir=config, prefix=".orbit-migration-") as temporary:
            copy = Path(temporary) / "orbit"
            shutil.copytree(source, copy)
            convert_db(copy / "sessions.sqlite3", source, destination)
            copy.rename(destination)
    print(f"Copied settings and sessions to {destination}. Original retained at {source}.")


if __name__ == "__main__":
    try:
        migrate()
    except (OSError, ValueError, sqlite3.Error) as error:
        sys.exit(f"Migration failed: {error}. Source preserved; resolve the error and retry.")
