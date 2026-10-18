from app.util import clamp


def total(values):
    return sum(values)


def mean(values):
    return total(values) / len(values)


def scale(value, factor):
    return clamp(value * factor, 0, 100)
