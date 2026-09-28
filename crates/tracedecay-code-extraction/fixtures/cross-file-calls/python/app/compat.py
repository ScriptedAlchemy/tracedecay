from app import legacy
from app.util import normalize as norm


def shim(text):
    return legacy.normalize(text)


def upgrade(text):
    return norm(shim(text))
