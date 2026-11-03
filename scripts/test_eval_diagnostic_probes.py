"""Ensure header parse probes detect both diagnostic representations."""

import json
import sqlite3
import unittest
from contextlib import closing
from pathlib import Path


class HeaderDiagnostics(unittest.TestCase):
    def test_member_pointer_probes_detect_header_warnings(self):
        expected = json.loads(Path(__file__).with_name("eval_expected.json").read_text())
        cases = [
            ("hiview", "explicit instantiation headers", ["encoded/encoded_param.h", "encoded/raw_data_builder.h"]),
            ("camera", "grouped member-pointer casts", ["camera_napi_event_emitter.h", "camera_event_emitter_taihe.h"]),
        ]
        for corpus, prefix, headers in cases:
            probe = next(p for p in expected["corpora"][corpus]["probes"] if p["name"].startswith(prefix))
            with self.subTest(corpus=corpus), closing(sqlite3.connect(":memory:")) as db:
                db.executescript("CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT);"
                                 "CREATE TABLE diagnostics(file_id INTEGER, message TEXT, stage TEXT);")
                self.assertEqual(db.execute(probe["sql"]).fetchone()[0], 0)
                for header in headers:
                    path = "/corpus/" + header
                    db.execute("INSERT INTO files(path) VALUES (?)", (path,))
                    file_id = db.execute("SELECT last_insert_rowid()").fetchone()[0]
                    for record in [
                        (file_id, "recovered declaration", "parse"),
                        (None, "parse errors in " + path, "parse"),
                        (None, path + ":12:3: syntax error", "parse"),
                    ]:
                        db.execute("DELETE FROM diagnostics")
                        db.execute("INSERT INTO diagnostics VALUES (?, ?, ?)", record)
                        self.assertEqual(db.execute(probe["sql"]).fetchone()[0], 1, record)
                    db.execute("DELETE FROM diagnostics")
                    db.execute("INSERT INTO diagnostics VALUES (?, ?, ?)", (file_id, "unrelated stage", "lower"))
                    self.assertEqual(db.execute(probe["sql"]).fetchone()[0], 0)
                db.execute("DELETE FROM diagnostics")
                db.execute("INSERT INTO diagnostics VALUES (NULL, 'parse errors in /corpus/other.h', 'parse')")
                self.assertEqual(db.execute(probe["sql"]).fetchone()[0], 0)


if __name__ == "__main__":
    unittest.main()
