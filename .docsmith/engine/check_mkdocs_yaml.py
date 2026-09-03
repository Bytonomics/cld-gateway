#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml"]
# ///
"""Safe YAML check for the docsmith-generated MkDocs config.

The stock `check-yaml` pre-commit hook safe-loads YAML and, having no
constructor for MkDocs-Material's `!!python/name:...` tag, rejects
`.docsmith/site/mkdocs.yml`. That tag is legitimate: it hands MkDocs a
reference to a Python callable used by the Mermaid superfences fence.

This checker keeps a SAFE loader (so object-instantiating tags like
`!!python/object/apply:...` stay unconstructable and rejected) but
teaches it exactly ONE tag family target:
`!!python/name:pymdownx.superfences.fence_code_format`. The constructor
returns an inert placeholder WITHOUT importing or calling anything, so no
code runs during the check. Any other `python/name:` target, and every
other `python/*` tag, is still rejected.

This deliberately does not use `--unsafe` (which would drop the whole
repo's YAML to syntax-only) and does not blanket-ignore the file: the
document is still fully structure-validated by the safe loader. Uses
PyYAML to match the rest of the docsmith engine.
"""

from __future__ import annotations

import sys

import yaml

# The exact `python/name:` suffixes we trust. Keep this list minimal; every
# entry is a Python object reference MkDocs resolves at real build time.
ALLOWED_PYTHON_NAME_SUFFIXES = {
    "pymdownx.superfences.fence_code_format",
}

_PYTHON_NAME_PREFIX = "tag:yaml.org,2002:python/name:"


class _MkDocsSafeLoader(yaml.SafeLoader):
    """SafeLoader subclass so the whitelist stays local to this checker."""


def _allow_known_python_name(loader, tag_suffix, node):
    """Accept only whitelisted `!!python/name:` tags; reject the rest.

    Returns an inert placeholder string — it never imports the module or
    calls the referenced object, so validating the file executes no code.
    """
    if tag_suffix not in ALLOWED_PYTHON_NAME_SUFFIXES:
        raise yaml.constructor.ConstructorError(
            None,
            None,
            f"disallowed python/name tag: {tag_suffix!r} "
            f"(add it to ALLOWED_PYTHON_NAME_SUFFIXES if it is trusted)",
            node.start_mark,
        )
    return f"<python/name:{tag_suffix}>"


_MkDocsSafeLoader.add_multi_constructor(
    _PYTHON_NAME_PREFIX, _allow_known_python_name
)


def main(argv: list[str]) -> int:
    retval = 0
    for filename in argv:
        try:
            with open(filename, encoding="utf-8") as f:
                yaml.load(f, Loader=_MkDocsSafeLoader)
        except yaml.YAMLError as exc:
            print(f"{filename}: {exc}")
            retval = 1
    return retval


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
