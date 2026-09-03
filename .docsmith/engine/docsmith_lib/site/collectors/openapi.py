# Vendored from smritea-cloud docs-site (generic, config-driven) — docsmith engine
"""Collector that turns raw `swagger.json` specs into browsable API reference pages.

This module implements the `openapi` collector referenced in
`docs-site/sources.yml`. It is the "HOW" for turning each OpenAPI/Swagger
2.0-or-3.x spec file listed in `source['paths']` into:

- A generated Markdown API reference page (OKF `type: API` frontmatter, an
  endpoint summary table, and an embedded `<swagger-ui>` widget powered by
  the `mkdocs-swagger-ui-tag` plugin).
- A copy of the raw `swagger.json` spec, placed as a true sibling of the
  generated page.

Because multiple services would otherwise all produce a flat `swagger.json`
file in the same destination directory (colliding with each other), each
service's generated page and raw spec are written into their own
per-service subdirectory:

    autogen_docs_dir / destination / <service_dir_name> / index.md
    autogen_docs_dir / destination / <service_dir_name> / swagger.json

This keeps the generated page's relative link `./swagger.json` correctly
resolving to a true sibling with no cross-service collisions.

For each collected spec a source-map entry dict is returned describing
where it came from, where it was written, its extracted title, and its
OKF `type`. The dict shape returned here is a contract consumed by
downstream pipeline code (navigation and provenance builders):

    {
        'source_id': str,
        'source_path': str,
        'output_path': str,
        'title': str,
        'okf_type': str,
    }
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

Endpoint = tuple[str, str, str]


def collect_openapi(source: dict, repo_root: Path, autogen_docs_dir: Path, _exclude_config: dict) -> list[dict]:
    """Collect OpenAPI/Swagger specs described by a `sources.yml` entry.

    `source` is a plain dict because it is parsed directly from YAML and
    its shape varies by collector; it is not re-modeled as a dataclass
    here to avoid duplicating the YAML schema.

    Args:
        source: A single parsed source entry from `sources.yml`. Must
            contain `id`, `paths` (list of `swagger.json` paths relative
            to `repo_root`), and `destination`.
        repo_root: Absolute path to the repository root that `paths` are
            relative to.
        autogen_docs_dir: Absolute path to the generated docs root that
            `destination` is relative to.
        _exclude_config: The shared `exclude:` section from `sources.yml`.
            Unused here -- this collector processes a fixed, explicit list
            of spec paths rather than walking a directory tree, so no
            blacklist filtering applies. Accepted only for signature
            consistency with the other collectors in `_COLLECTOR_DISPATCH`.

    Returns:
        A list of source-map entry dicts, one per successfully processed
        spec. Specs that are missing or fail to parse are skipped and
        logged via `print()` rather than raising.
    """
    entries: list[dict] = []
    for relative_path in source.get('paths', []):
        entry = _collect_single_spec(source, repo_root, autogen_docs_dir, relative_path)
        if entry is None:
            continue
        entries.append(entry)
    return entries


def _collect_single_spec(
    source: dict,
    repo_root: Path,
    autogen_docs_dir: Path,
    relative_path: str,
) -> dict | None:
    absolute_path = repo_root / relative_path
    if not absolute_path.exists():
        print(f'[openapi] skipping missing spec: {absolute_path}')
        return None

    spec = _parse_spec(absolute_path)
    if spec is None:
        return None

    service_dir_name = absolute_path.parent.name
    service_name = _extract_service_name(spec, service_dir_name)
    description = _extract_description(spec)
    version = _extract_version(spec)
    base_url = _extract_base_url(spec)
    endpoints = _extract_endpoints(spec)

    destination_dir = autogen_docs_dir / source['destination'] / service_dir_name
    page_path = destination_dir / 'index.md'
    spec_destination_path = destination_dir / 'swagger.json'

    title = f'{service_name} API Reference'
    page_content = _render_page(
        service_name=service_name,
        title=title,
        description=description,
        version=version,
        base_url=base_url,
        endpoints=endpoints,
    )

    if not _write_outputs(destination_dir, page_path, page_content, absolute_path, spec_destination_path):
        return None

    return {
        'source_id': source['id'],
        'source_path': str(absolute_path),
        'output_path': str(page_path.relative_to(autogen_docs_dir)),
        'title': title,
        'okf_type': 'API',
    }


def _parse_spec(absolute_path: Path) -> dict | None:
    try:
        return json.loads(absolute_path.read_text(encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as error:
        print(f'[openapi] failed to parse spec {absolute_path}: {error}')
        return None


def _write_outputs(
    destination_dir: Path,
    page_path: Path,
    page_content: str,
    spec_source_path: Path,
    spec_destination_path: Path,
) -> bool:
    try:
        destination_dir.mkdir(parents=True, exist_ok=True)
        page_path.write_text(page_content, encoding='utf-8')
        shutil.copy2(spec_source_path, spec_destination_path)
        return True
    except OSError as error:
        print(f'[openapi] failed to write outputs for {spec_source_path}: {error}')
        return False


def _extract_service_name(spec: dict, fallback_dir_name: str) -> str:
    info = spec.get('info', {})
    title = info.get('title')
    if isinstance(title, str) and title:
        return title
    return fallback_dir_name


def _extract_description(spec: dict) -> str:
    info = spec.get('info', {})
    description = info.get('description')
    if isinstance(description, str):
        return description
    return ''


def _extract_version(spec: dict) -> str:
    info = spec.get('info', {})
    version = info.get('version')
    if isinstance(version, str) and version:
        return version
    return 'unknown'


def _extract_base_url(spec: dict) -> str:
    if 'swagger' in spec:
        return _extract_base_url_swagger2(spec)
    if 'openapi' in spec:
        return _extract_base_url_openapi3(spec)
    return '(not specified)'


def _extract_base_url_swagger2(spec: dict) -> str:
    host = spec.get('host')
    if not host:
        return '(not specified)'

    base_path = spec.get('basePath', '')
    schemes = spec.get('schemes', [])
    scheme = schemes[0] if schemes else 'https'
    return f'{scheme}://{host}{base_path}'


def _extract_base_url_openapi3(spec: dict) -> str:
    servers = spec.get('servers', [])
    if not servers:
        return '(not specified)'

    first_server = servers[0]
    url = first_server.get('url')
    if not isinstance(url, str) or not url:
        return '(not specified)'
    return url


def _extract_endpoints(spec: dict) -> list[Endpoint]:
    paths = spec.get('paths', {})
    if not isinstance(paths, dict):
        return []

    endpoints: list[Endpoint] = []
    for path, operations in paths.items():
        if not isinstance(operations, dict):
            continue
        endpoints.extend(_extract_operations_for_path(path, operations))

    endpoints.sort(key=lambda endpoint: (endpoint[1], endpoint[0]))
    return endpoints


_HTTP_METHODS = ('get', 'post', 'put', 'delete', 'patch', 'options', 'head')


def _extract_operations_for_path(path: str, operations: dict) -> list[Endpoint]:
    endpoints: list[Endpoint] = []
    for method, operation in operations.items():
        if method.lower() not in _HTTP_METHODS:
            continue
        if not isinstance(operation, dict):
            continue
        summary = operation.get('summary', '')
        endpoints.append((method.upper(), path, summary))
    return endpoints


def _render_page(
    service_name: str,
    title: str,
    description: str,
    version: str,
    base_url: str,
    endpoints: list[Endpoint],
) -> str:
    frontmatter = (
        '---\n'
        'type: API\n'
        f'title: "{title}"\n'
        'status: stable\n'
        'tags: [api, openapi]\n'
        '---\n'
    )

    description_block = f'\n{description}\n' if description else ''

    header = (
        f'{frontmatter}'
        f'\n# {title}\n'
        f'{description_block}\n'
        f'- **Version:** {version}\n'
        f'- **Base URL:** `{base_url}`\n'
        '- **Raw spec:** [`swagger.json`](./swagger.json)\n'
        '\n<swagger-ui src="./swagger.json"/>\n'
        '\n## Endpoints\n'
        '\n| Method | Path | Summary |\n'
        '|--------|------|---------|\n'
    )

    table_rows = ''.join(f'| `{method}` | `{path}` | {summary} |\n' for method, path, summary in endpoints)

    return header + table_rows
