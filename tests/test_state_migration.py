"""Run with python3 -m unittest discover -s tests."""
import fcntl
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/migrate-pico-evo.py"


class Migration(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.config = Path(temp.name)
        self.old = self.config / "pico-evo"
        self.new = self.config / "orbit"
        self.old.mkdir(mode=0o700)
        (self.old / "instance.lock").touch()

    def migrate(self):
        return subprocess.run([sys.executable, SCRIPT], capture_output=True, text=True,
                              env={**os.environ, "XDG_CONFIG_HOME": str(self.config)})

    def test_fork_attachments_and_private_context_survive_copy(self):
        attachment = Path("attachments/captured-file")
        (self.old / attachment.parent).mkdir()
        (self.old / attachment).write_bytes(b"snapshot")
        message = f"Keep this prose verbatim: pico_file at {self.old}"
        parts = [{"type": "input_text", "text": message},
                 {"type": "pico_file", "file": {"snapshot": str(self.old / attachment),
                                                "spill": "/tmp/historical/snapshot"}}]
        delta = {"len": 1, "rows": [[0, {"content": parts}]]}
        with closing(sqlite3.connect(self.old / "sessions.sqlite3")) as db, db:
            db.executescript("""
                CREATE TABLE new_session_draft(data);
                CREATE TABLE sessions(metadata);
                CREATE TABLE entries(data);
                CREATE TABLE context(data);
                CREATE TABLE fork_points(entries,input);
            """)
            image = {"path": str(self.old / "media/original.gif"), "mime": "image/gif",
                     "bytes": 321, "prepared": str(self.old / "media/frame.png"),
                     "thumbnail": None}
            db.execute("INSERT INTO new_session_draft VALUES (?)",
                       (json.dumps({"images": [image], "project": "/work/pico-evo"}),))
            db.execute("INSERT INTO fork_points VALUES ('{}',?)", (json.dumps(delta),))
        result = self.migrate()
        self.assertEqual(result.returncode, 0, result.stderr)
        for root, tag in [(self.old, "pico_file"), (self.new, "orbit_file")]:
            with closing(sqlite3.connect(root / "sessions.sqlite3")) as db:
                draft = json.loads(db.execute("SELECT data FROM new_session_draft").fetchone()[0])
                self.assertEqual(draft["images"][0], {**image, "path": str(root / "media/original.gif"),
                                                      "prepared": str(root / "media/frame.png")})
                self.assertEqual(draft["project"], "/work/pico-evo")
                raw, = db.execute("SELECT input FROM fork_points").fetchone()
                text, file = json.loads(raw)["rows"][0][1]["content"]
            self.assertEqual(text["text"], message)
            self.assertEqual(file["type"], tag)
            snapshot = Path(file["file"]["snapshot"])
            self.assertEqual(snapshot, root / attachment)
            self.assertEqual(snapshot.read_bytes(), b"snapshot")
        self.assertNotEqual(self.migrate().returncode, 0, "must not merge an existing destination")

    def test_locked_or_unreadable_source_is_not_published(self):
        with (self.old / "instance.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = self.migrate()
            self.assertIn("Close the app", result.stderr)
            self.assertFalse(self.new.exists())
        database = self.old / "sessions.sqlite3"
        database.write_bytes(b"damaged database")
        self.assertNotEqual(self.migrate().returncode, 0)
        self.assertFalse(self.new.exists())
        self.assertEqual(database.read_bytes(), b"damaged database")
