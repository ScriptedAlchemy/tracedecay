import unittest

from shop import place_order, render_receipt


class OrderTests(unittest.TestCase):
    def test_rejects_unavailable_order(self):
        warehouse = {"widget": 1}
        with self.assertRaisesRegex(ValueError, "insufficient stock"):
            place_order(warehouse, "widget", 2, 125)
        self.assertEqual(warehouse["widget"], 1)

    def test_accepts_available_order(self):
        warehouse = {"widget": 3}
        self.assertEqual(place_order(warehouse, "widget", 2, 125), 250)
        self.assertEqual(warehouse["widget"], 1)

    def test_receipt_display(self):
        self.assertEqual(render_receipt(250), "Total: 2.50")
