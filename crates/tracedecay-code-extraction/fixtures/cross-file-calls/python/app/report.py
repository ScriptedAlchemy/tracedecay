import app.util as u
from .math import mean


def format_line(value):
    return u.normalize(value) + "\n"


def summary(values):
    return format_line(str(mean(values)))
