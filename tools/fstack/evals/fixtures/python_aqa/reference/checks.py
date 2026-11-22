import unittest

import dispatch


class DispatchTests(unittest.TestCase):
    def test_tracking_code(self):
        label = dispatch.make_label("QZ-329")
        self.assertIsNotNone(label)
        self.assertEqual(label.get("tracking_code"), "QZ-329")

    def test_invoice_total(self):
        lines = [(2, 350), (1, 190)]
        self.assertEqual(dispatch.prepare_invoice(lines)["total"], 1020)
