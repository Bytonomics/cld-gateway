# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Collector implementations for the docs-site pipeline."""

from .markdown_tree import collect_markdown_tree
from .openapi import collect_openapi
from .readme_discovery import collect_readme_discovery

__all__ = ["collect_markdown_tree", "collect_openapi", "collect_readme_discovery"]
