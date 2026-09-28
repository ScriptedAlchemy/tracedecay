import os
from app.missing import vanish
from app.util import absent


def probe(path):
    vanish(path)
    absent(path)
    return os.path.join(path, "x")
