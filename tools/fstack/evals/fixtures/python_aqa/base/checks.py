import unittest

import dispatch


class DispatchTests(unittest.TestCase):
    def test_tracking_code(self):
        label = dispatch.make_label("QZ-329")
        if label is not None:
            self.assertEqual(label["tracking_code"], "QZ-329")

    def test_invoice_total(self):
        lines = [(2, 350), (1, 190)]
        expected = dispatch.calculate_total(lines)
        self.assertEqual(dispatch.prepare_invoice(lines)["total"], expected)
