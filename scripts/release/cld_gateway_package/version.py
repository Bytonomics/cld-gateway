"""Version discovery for cld-gateway packages."""

from .targets import REPO_ROOT


def read_workspace_version() -> str:
    version_file = REPO_ROOT / "VERSION"
    if not version_file.is_file():
        raise RuntimeError(f"Missing version file: {version_file}")
    version = version_file.read_text(encoding="utf-8").strip()
    if not version:
        raise RuntimeError(f"Version file is empty: {version_file}")
    return version
