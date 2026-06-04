#!/usr/bin/env python3
"""Build a canonical cld-gateway package directory and optional archive."""

from pathlib import Path
import sys

# Ensure the local helper package is importable from any cwd.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from cld_gateway_package.cli import main

if __name__ == "__main__":
    raise SystemExit(main())
