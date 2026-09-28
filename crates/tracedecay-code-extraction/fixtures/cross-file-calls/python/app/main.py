from app import compat, legacy, report, shapes
from app.math import scale
from app.store import Store


def main():
    store = Store()
    store.add("Key", 1)
    store.get("key")
    print(report.summary([1, 2, 3]))
    print(compat.upgrade(" Text "))
    print(shapes.area(3, 4), shapes.perimeter(3, 4))
    print(legacy.old_format("x"), scale(2, 3))
