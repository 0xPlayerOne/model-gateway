import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "performance_audit.py"
SPEC = importlib.util.spec_from_file_location("performance_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
PERFORMANCE_AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PERFORMANCE_AUDIT)


class PerformanceAuditTests(unittest.TestCase):
    def test_percentile_interpolates_sorted_samples(self):
        self.assertEqual(PERFORMANCE_AUDIT.percentile([4.0, 1.0, 3.0, 2.0], 50), 2.5)
        self.assertAlmostEqual(
            PERFORMANCE_AUDIT.percentile([1.0, 2.0, 3.0, 4.0], 95), 3.85
        )

    def test_threshold_failures_report_missing_and_exceeded_metrics(self):
        failures = PERFORMANCE_AUDIT.threshold_failures(
            {"fast": 1.0, "slow": 3.0, "missing": None},
            {"fast": 2.0, "slow": 2.0, "missing": 2.0},
        )
        self.assertEqual(
            failures,
            ["slow: 3.000 > 2.000", "missing: metric was not recorded"],
        )


if __name__ == "__main__":
    unittest.main()
