"""Which call graph each eval_check measurement reads."""
from contextlib import closing, redirect_stdout
from pathlib import Path
import io
import sqlite3
import tempfile
import unittest

import eval_check

# `f` calls `g` through a lambda. The public relations contract the path, the
# raw ones keep it; `files` is unchanged, so its raw relation is an alias.
PUBLIC = """
    CREATE TABLE files(id INTEGER PRIMARY KEY, path TEXT);
    INSERT INTO files VALUES(1, '/src/a.cpp');
    CREATE TABLE functions(id INTEGER PRIMARY KEY, name TEXT, file_id INT, is_defined INT);
    INSERT INTO functions VALUES(1, 'f', 1, 1), (2, 'g', 1, 1);
    CREATE TABLE call_sites(id INTEGER PRIMARY KEY, caller_fn_id INT);
    INSERT INTO call_sites VALUES(1, 1);
    CREATE TABLE call_edges(id INTEGER PRIMARY KEY, call_site_id INT, caller_fn_id INT,
                            callee_fn_id INT, resolution TEXT);
    INSERT INTO call_edges VALUES(1, 1, 1, 2, 'indirect');
    CREATE TABLE arg_flow_edges(id INTEGER PRIMARY KEY);
    CREATE TABLE diagnostics(id INTEGER PRIMARY KEY);
    CREATE TABLE flow_edges(id INTEGER PRIMARY KEY, kind TEXT);
    CREATE TABLE flow_nodes(id INTEGER PRIMARY KEY);
    CREATE TABLE flow_origins(id INTEGER PRIMARY KEY);
    CREATE TABLE variables(id INTEGER PRIMARY KEY);
"""
RAW = """
    CREATE TABLE lambda_owners(fn_id INT);
    CREATE VIEW raw_files AS SELECT * FROM files;
    CREATE VIEW raw_arg_flow_edges AS SELECT * FROM arg_flow_edges;
    CREATE VIEW raw_diagnostics AS SELECT * FROM diagnostics;
    CREATE VIEW raw_flow_edges AS SELECT * FROM flow_edges;
    CREATE VIEW raw_flow_nodes AS SELECT * FROM flow_nodes;
    CREATE VIEW raw_flow_origins AS SELECT * FROM flow_origins;
    CREATE VIEW raw_variables AS SELECT * FROM variables;
    CREATE TABLE raw_functions(id INTEGER PRIMARY KEY, name TEXT, file_id INT, is_defined INT);
    INSERT INTO raw_functions VALUES(1, 'f', 1, 1), (2, 'g', 1, 1), (3, 'f::$lambda0:1', 1, 1);
    CREATE TABLE raw_call_sites(id INTEGER PRIMARY KEY, caller_fn_id INT);
    INSERT INTO raw_call_sites VALUES(1, 1), (2, 3);
    CREATE TABLE raw_call_edges(id INTEGER PRIMARY KEY, call_site_id INT, caller_fn_id INT,
                                callee_fn_id INT, resolution TEXT);
    INSERT INTO raw_call_edges VALUES(1, 1, 1, 3, 'direct'), (2, 2, 3, 2, 'indirect');
"""


class RelationsTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)
        self.db = self.database("snapshot.db", PUBLIC + RAW)
        eval_check.FAILS = eval_check.CHECKS = 0
        output = redirect_stdout(io.StringIO())
        output.__enter__()
        self.addCleanup(output.__exit__, None, None, None)

    def database(self, name, schema):
        path = self.directory / name
        with closing(sqlite3.connect(path)) as conn:
            conn.executescript(schema)
        return path

    def probe(self, value, db=None, **fields):
        eval_check.check_probe(db or self.db, {
            "name": "count", "sql": "SELECT COUNT(*) FROM functions",
            "cmp": "exact", "value": value, **fields})
        return eval_check.FAILS

    def test_globals_measure_the_complete_graph(self):
        got = eval_check.globals_from_db(self.db)
        self.assertEqual(
            (got["functions_total"], got["edges_total"], got["edges_direct"], got["edges_indirect"]),
            (3, 2, 1, 1))

    def test_public_globals_measure_the_contracted_graph(self):
        got = eval_check.globals_from_db(self.db)
        self.assertEqual((got["public_functions_total"], got["public_edges_total"]), (2, 1))

    def test_probe_reads_the_complete_graph_by_default(self):
        self.assertEqual(self.probe(3), 0)

    def test_probe_can_ask_for_the_public_graph(self):
        self.assertEqual(self.probe(2, relations="public"), 0)

    def test_probe_rejects_an_unknown_graph(self):
        with self.assertRaises(ValueError):
            self.probe(3, relations="pubilc")

    def test_site_reads_the_complete_graph(self):
        eval_check.check_site(self.db, {
            "case": "lambda body", "caller": "%$lambda%",
            "resolution": "indirect", "expected": 1})
        self.assertEqual(eval_check.FAILS, 0)

    def test_site_can_ask_for_the_public_graph(self):
        eval_check.check_site(self.db, {
            "case": "lambda body", "caller": "%$lambda%",
            "resolution": "indirect", "expected": 0, "relations": "public"})
        self.assertEqual(eval_check.FAILS, 0)

    def test_projected_snapshot_missing_raw_graph_is_rejected(self):
        broken = self.database("broken.db", PUBLIC + "CREATE TABLE lambda_owners(fn_id INT);")
        with self.assertRaises(ValueError):
            self.probe(2, broken)

    def test_projected_snapshot_missing_a_measured_raw_relation_is_rejected(self):
        for relation in ("arg_flow_edges", "diagnostics", "flow_edges",
                         "flow_nodes", "flow_origins", "variables"):
            with self.subTest(relation=relation):
                view = f"CREATE VIEW raw_{relation} AS SELECT * FROM {relation};"
                self.assertIn(view, RAW)
                partial = self.database(f"partial_{relation}.db", PUBLIC + RAW.replace(view, ""))
                with self.assertRaisesRegex(ValueError, rf"\['raw_{relation}'\]"):
                    eval_check.connect(partial)

    def test_snapshot_without_raw_relations_serves_both_graphs(self):
        legacy = self.database("legacy.db", PUBLIC)
        got = eval_check.globals_from_db(legacy)
        self.assertEqual((got["functions_total"], got["public_functions_total"]), (2, 2))
        self.assertEqual(self.probe(2, legacy), 0)
        self.assertEqual(self.probe(2, legacy, relations="public"), 0)


if __name__ == "__main__":
    unittest.main()
