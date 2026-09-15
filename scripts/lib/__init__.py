# Marks scripts/lib as a regular package so `from lib.<module> import ...`
# in the sibling scripts resolves here deterministically instead of falling
# back to namespace-package scanning across the whole sys.path.
