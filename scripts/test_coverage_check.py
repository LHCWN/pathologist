"""Per-crate coverage floors enforced from an lcov report."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import coverage_check


def lcov(*records):
    out = []
    for path, counts in records:
        out.append(f"SF:{path}")
        for line, count in enumerate(counts, start=1):
            out.append(f"DA:{line},{count}")
        out.append("end_of_record")
    return "\n".join(out) + "\n"


class PerCrateTest(unittest.TestCase):
    def test_groups_workspace_sources_and_ignores_tests_and_foreign_paths(self):
        text = lcov(
            ("/repo/crates/alpha/src/lib.rs", [1, 0, 1, 1]),
            ("/repo/crates/alpha/tests/it.rs", [0, 0]),
            ("/repo/crates/beta/src/main.rs", [1, 1]),
            ("/repo/crates/beta/src/../build_support.rs", [1, 1]),
            ("/repo/vendor/other/src/lib.rs", [0]),
            ("/repo/vendor/dep/crates/evil/src/lib.rs", [0]),
            ("/home/u/.cargo/git/checkouts/repo/crates/evil/src/lib.rs", [0]),
            ("/home/u/.cargo/registry/src/index.crates.io/pkg/crates/evil/src/lib.rs", [0]),
            ("/repo/crates/beta/build.rs", [0]),
        )

        got = coverage_check.per_crate(text, root=Path("/repo"))

        # `src/../build_support.rs` normalizes outside `src/` and is not counted.
        self.assertEqual(got, {"alpha": (4, 1), "beta": (2, 0)})

    def test_repo_path_containing_vendor_still_counts_its_crates(self):
        text = lcov(("/home/vendor/work/crates/alpha/src/lib.rs", [1, 0]))
        self.assertEqual(
            coverage_check.per_crate(text, root=Path("/home/vendor/work")),
            {"alpha": (2, 1)})

    def test_dependency_checkout_outside_the_workspace_is_ignored(self):
        text = lcov(
            ("/repo/crates/alpha/src/lib.rs", [1, 0]),
            ("/tmp/git/dep/crates/evil/src/lib.rs", [0]),
            ("/repo/target/package/pkg-1.0/crates/evil/src/lib.rs", [0]),
        )
        self.assertEqual(
            coverage_check.per_crate(text, root=Path("/repo")),
            {"alpha": (2, 1)})

    def test_accepts_relative_paths(self):
        text = lcov(("crates/alpha/src/lib.rs", [1, 0]))
        self.assertEqual(coverage_check.per_crate(text), {"alpha": (2, 1)})

    def test_accepts_windows_separators(self):
        text = lcov((r"C:\repo\crates\alpha\src\lib.rs", [1, 0]))
        self.assertEqual(coverage_check.per_crate(text), {"alpha": (2, 1)})

    def test_reports_each_crate_below_its_floor(self):
        per_crate = {"alpha": (10, 1), "beta": (10, 0), "gamma": (10, 5)}

        failures = coverage_check.failures(
            per_crate, {"alpha": 85.0, "beta": 100.0, "gamma": 95.0}, total_floor=None)

        self.assertEqual(len(failures), 1)
        self.assertIn("gamma", failures[0])
        self.assertIn("50.00%", failures[0])

    def test_reports_total_below_floor(self):
        failures = coverage_check.failures(
            {"alpha": (10, 1)}, {"alpha": 80.0}, total_floor=95.0)
        self.assertEqual(len(failures), 1)
        self.assertIn("total", failures[0])

    def test_empty_report_is_rejected(self):
        with self.assertRaises(ValueError):
            coverage_check.per_crate("SF:no-such-lcov-record\nend_of_record\n")
        with self.assertRaises(ValueError):
            coverage_check.per_crate("")

    def defaults_with(self, overrides):
        records = {crate: [1] for crate in coverage_check.DEFAULT_FLOORS}
        records.update(overrides)
        return lcov(*[(f"crates/{crate}/src/lib.rs", counts)
                      for crate, counts in records.items()])

    def run_check(self, path, *args):
        return subprocess.run(
            [sys.executable, str(Path(coverage_check.__file__)), str(path), *args],
            capture_output=True, text=True)

    def test_cli_returns_failure_for_floor_and_success_otherwise(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lcov.info"
            path.write_text(self.defaults_with({"alpha": [1, 0]}))

            below = self.run_check(path, "--floor", "alpha=99")
            ok = self.run_check(
                path, "--floor", "alpha=50", "--total-floor", "50")

        self.assertEqual(below.returncode, 1, below.stderr)
        self.assertIn("alpha", below.stderr + below.stdout)
        self.assertEqual(ok.returncode, 0, ok.stderr + ok.stdout)

    def test_percentage_arguments_reject_invalid_values(self):
        for bad in ["nan", "inf", "-1", "101", "abc"]:
            with self.assertRaises(coverage_check.argparse.ArgumentTypeError, msg=bad):
                coverage_check.parse_percentage(bad)
        self.assertEqual(coverage_check.parse_percentage("0"), 0.0)
        self.assertEqual(coverage_check.parse_percentage("100"), 100.0)
        self.assertEqual(coverage_check.parse_floor("trace-db=95")[1], 95.0)

    def test_cli_rejects_invalid_percentages(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lcov.info"
            path.write_text(self.defaults_with({"alpha": [1]}))

            for args in (("--floor", "alpha=nan"),
                         ("--floor", "alpha=101"),
                         ("--total-floor", "nan"),
                         ("--total-floor", "-1")):
                result = self.run_check(path, *args)
                self.assertEqual(result.returncode, 2, (args, result.stderr))

    def test_floor_override_keeps_checking_the_default_floors(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lcov.info"
            path.write_text(self.defaults_with(
                {"alpha": [1], "trace-db": [1, 0]}))

            result = self.run_check(
                path, "--floor", "alpha=100", "--total-floor", "0")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("trace-db: 50.00%", result.stderr + result.stdout)


if __name__ == "__main__":
    unittest.main()
