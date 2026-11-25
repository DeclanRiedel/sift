#!/usr/bin/env python3
"""Describe a fixed native desktop payload for signing by the release job."""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess
from urllib.parse import urlsplit


PAYLOADS = ("sift-desktop", "sift-server", "sift-launcher", "sift-admin", "sift-remote", "sift", "sift-desktop-launcher")


def build_manifest(dist, origin, sequence, version, commit, protocol, now=None):
    url = urlsplit(origin)
    if (url.scheme != "https" or not url.hostname or url.username or url.password
            or url.query or url.fragment):
        raise ValueError("release origin must be an explicit HTTPS asset directory")
    if not (0 < sequence < 2**64 and 0 < protocol < 2**32):
        raise ValueError("invalid release sequence or protocol")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid release commit")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+-dev\.[1-9][0-9]*", version):
        raise ValueError("development releases require a unique prerelease version")
    files = []
    for name in PAYLOADS:
        path = dist / f"{name}-linux-x86_64"
        if path.is_symlink() or not path.is_file() or path.stat().st_nlink != 1:
            raise ValueError("release payload must be an ordinary unaliased file")
        size = path.stat().st_size
        if not 0 < size <= 512 * 1024 * 1024:
            raise ValueError("release payload exceeds supported bounds")
        header = subprocess.run(["readelf", "-h", str(path)], check=True,
                                capture_output=True, text=True).stdout
        dynamic = subprocess.run(["readelf", "-d", str(path)], check=True,
                                 capture_output=True, text=True).stdout
        segments = subprocess.run(["readelf", "-l", str(path)], check=True,
                                  capture_output=True, text=True).stdout
        if ("ELF64" not in header or "Advanced Micro Devices X86-64" not in header
                or "[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]" not in segments
                or re.search(r"\((?:RPATH|RUNPATH)\)", dynamic)):
            raise ValueError("desktop channel requires native x86-64 Linux executables")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        files.append({"name": name, "url": f"{origin.rstrip('/')}/{path.name}",
                      "bytes": size, "sha256": digest})
    if sum(item["bytes"] for item in files) > 2 * 1024**3:
        raise ValueError("desktop bundle exceeds supported bounds")
    now = now or datetime.now(timezone.utc)
    return {"schema_version": 1, "product": "sift-desktop", "channel": "development",
            "sequence": sequence, "version": version, "source_commit": commit,
            "published_at": now.isoformat(), "expires_at": (now + timedelta(days=30)).isoformat(),
            "minimum_launcher_version": 1, "protocol": {"minimum": protocol, "maximum": protocol},
            "targets": [{"target": "linux-x86_64", "files": files}]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", type=Path, required=True)
    parser.add_argument("--origin", required=True)
    parser.add_argument("--sequence", type=int, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--protocol", type=int, required=True)
    args = parser.parse_args()
    manifest = build_manifest(args.dist, args.origin, args.sequence, args.version, args.commit, args.protocol)
    (args.dist / "desktop-manifest.json").write_text(json.dumps(manifest, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
