import app.math
from app.util import clamp


def area(width, height):
    return clamp(width, 0, 100) * height


def perimeter(width, height):
    return app.math.total([clamp(width, 0, 100), height]) * 2
