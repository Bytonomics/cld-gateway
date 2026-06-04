"""Command-line interface for building cld-gateway package directories."""

import argparse
import tempfile
from pathlib import Path

from .archive import write_archive
from .layout import build_package_dir
from .layout import prepare_package_dir
from .layout import validate_package_dir
from .targets import TARGET_SPECS
from .targets import default_target
from .targets import resolve_input_path
from .version import read_workspace_version


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a canonical cld-gateway package directory and optional archive.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--target",
        default=argparse.SUPPRESS,
        choices=sorted(TARGET_SPECS),
        help=(
            "Rust target triple for the package. Defaults to the release target "
            "for this host platform."
        ),
    )
    parser.add_argument(
        "--package-dir",
        type=Path,
        default=argparse.SUPPRESS,
        help=(
            "Output directory to create as the package root. "
            "Defaults to a new temporary directory."
        ),
    )
    parser.add_argument(
        "--archive-output",
        type=Path,
        action="append",
        default=[],
        help=(
            "Optional archive output path. May be repeated. "
            "Supported suffixes: .tar.gz, .tgz."
        ),
    )
    parser.add_argument(
        "--entrypoint-bin",
        type=Path,
        required=True,
        help="Prebuilt cld-gateway binary to package.",
    )
    parser.add_argument(
        "--cargo-profile",
        default="release",
        help="Cargo profile label recorded in package metadata (informational only).",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an existing package directory or archive output.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    spec = TARGET_SPECS[getattr(args, "target", None) or default_target()]
    package_dir_arg = getattr(args, "package_dir", None)
    package_dir = (
        package_dir_arg.resolve()
        if package_dir_arg is not None
        else Path(tempfile.mkdtemp(prefix="cld-gateway-package-")).resolve()
    )

    entrypoint_bin = resolve_input_path(
        args.entrypoint_bin,
        "prebuilt cld-gateway binary",
        "--entrypoint-bin",
    )
    version = read_workspace_version()

    prepare_package_dir(package_dir, force=args.force)
    build_package_dir(package_dir, version, spec, entrypoint_bin)
    validate_package_dir(package_dir, spec)

    for archive_output in args.archive_output:
        archive_path = archive_output.resolve()
        write_archive(package_dir, archive_path, force=args.force)
        print(f"Built cld-gateway package archive at {archive_path}")

    print(f"Built cld-gateway package directory at {package_dir}")
    return 0
