import argparse
import hashlib
import json
from pathlib import Path


def source_paths(root):
    fixed = [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "scripts/g5-source-manifest.py",
    ]
    crates = sorted(
        path.relative_to(root).as_posix()
        for path in (root / "crates").rglob("*")
        if path.is_file()
        and (path.suffix in (".rs", ".toml") or path.name == "README.md")
    )
    return fixed + crates


def manifest(root, paths, normalize):
    lines = []
    for relative in paths:
        content = (root / relative).read_bytes()
        content.decode("utf-8", errors="strict")
        if normalize:
            content = content.replace(b"\r\n", b"\n")
        digest = hashlib.sha256(content).hexdigest()
        lines.append(f"{digest}  {relative}\n")
    return "".join(lines).encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description="Reproducible G5 source manifests; no build or CLI execution")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    paths = source_paths(arguments.root)
    normalized = manifest(arguments.root, paths, normalize=True)
    raw = manifest(arguments.root, paths, normalize=False)
    metadata = {
        "format": "moe-g5-source-manifest-v1",
        "files": len(paths),
        "normalization": "replace CRLF with LF only; preserve BOM and bare CR",
        "ordering": "fixed root list, then ordinal forward-slash relative path strings",
        "encoding": "UTF-8 without BOM; LF lines; one final newline",
        "lf_sha256": hashlib.sha256(normalized).hexdigest(),
        "raw_sha256": hashlib.sha256(raw).hexdigest(),
    }
    encoded_metadata = (json.dumps(metadata, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    if arguments.output is not None:
        arguments.output.mkdir(parents=True, exist_ok=True)
        (arguments.output / "source.lf.sha256").write_bytes(normalized)
        (arguments.output / "source.raw.sha256").write_bytes(raw)
        (arguments.output / "source-manifest.json").write_bytes(encoded_metadata)
    print(encoded_metadata.decode("utf-8"), end="")


if __name__ == "__main__":
    main()
