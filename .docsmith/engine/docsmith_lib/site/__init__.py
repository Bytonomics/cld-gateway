"""Generalized doc-site pipeline for the `docsmith collect` subcommand.

Forked from smritea-cloud's docs-site pipeline and made config-driven:
`engine.py` orchestrates the stages, `nav.py` builds navigation and hub
pages, `collectors/` and `transforms/` are vendored generic modules, and
`templates/mkdocs.yml.j2` is the parameterized MkDocs config template.

The CLI (scripts/docsmith.py) delegates `docsmith collect` to `engine.run`.
"""
