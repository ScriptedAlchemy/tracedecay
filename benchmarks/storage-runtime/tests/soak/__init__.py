"""Tests for the storage-runtime soak orchestration library.

Unittest's recursive discovery imports this directory as ``soak``.  Extend that
package path to the sibling implementation so focused and parent discovery use
the same imports without modifying the existing test runner configuration.
"""

from pathlib import Path

__path__.append(str(Path(__file__).resolve().parents[2] / "soak"))
