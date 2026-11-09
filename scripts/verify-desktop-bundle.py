#!/usr/bin/env python3
"""Offline Ed25519 verification of Windows bundles or native Linux package bytes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile

TARGET = "x86_64-pc-windows-msvc"
LINUX_TARGET = "x86_64-unknown-linux-gnu"
BINARIES = ("sift-desktop", "sift-server", "sift-launcher", "sift-admin", "sift-remote", "sift")
PAYLOAD = frozenset(
    [f"{name}.exe" for name in (
        "sift-desktop", "sift-server", "sift-launcher", "sift-admin", "sift-remote", "sift"
    )] + ["LICENSE", "README.txt", "source.tar", "licenses/LICENSE.qlementine-icons",
         "licenses/tiberius-APACHE.txt", "licenses/tiberius-MIT.txt"]
)
MAX_MANIFEST = 1024 * 1024
MAX_FILE = 2 * 1024**3
MAX_TOTAL = 8 * 1024**3
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


class InvalidBundle(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise InvalidBundle(message)


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def open_regular(path, directory=None):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                "Inputs must be ordinary files with exactly one link")
        return fd, info
    except BaseException:
        os.close(fd)
        raise


def read_bounded(path, limit, directory=None):
    fd, before = open_regular(path, directory)
    with os.fdopen(fd, "rb") as stream:
        require(before.st_size <= limit, "Input exceeds its byte budget")
        data = stream.read(limit + 1)
        require(len(data) <= limit and len(data) == before.st_size,
                "Input length changed or exceeds its byte budget")
        require(stamp(before) == stamp(os.fstat(stream.fileno())),
                "Input changed during inspection")
        return data


def command(args):
    try:
        result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise InvalidBundle("OpenSSL is unavailable or verification timed out") from error
    require(result.returncode == 0, "OpenSSL key/signature verification failed")
    return result.stdout


def verify_signature(manifest, signature, public_key, openssl):
    require(len(signature) == 64, "An Ed25519 raw signature must contain exactly 64 bytes")
    with tempfile.TemporaryDirectory(prefix="sift-bundle-verification-") as temporary:
        root = Path(temporary)
        for name, data in [("manifest", manifest), ("signature", signature), ("public.pem", public_key)]:
            (root / name).write_bytes(data)
        der = command([openssl, "pkey", "-pubin", "-in", str(root / "public.pem"), "-outform", "DER"])
        require(len(der) == 44 and der.startswith(ED25519_SPKI_PREFIX),
                "The external trust key must be an Ed25519 public key")
        command([openssl, "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(root / "public.pem"),
                 "-in", str(root / "manifest"), "-sigfile", str(root / "signature")])
        return hashlib.sha256(der).hexdigest()


def object_without_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "Duplicate JSON object keys are forbidden")
        result[key] = value
    return result


def text(value, maximum):
    return (isinstance(value, str) and 0 < len(value) <= maximum
            and all(ord(character) >= 32 and ord(character) != 127
                    and not 0xD800 <= ord(character) <= 0xDFFF for character in value))


def reject_constant(_):
    raise InvalidBundle("Nonfinite JSON is forbidden")


def decode_manifest(raw):
    try:
        manifest = json.loads(raw.decode("utf-8"), object_pairs_hook=object_without_duplicates,
                              parse_constant=reject_constant)
    except (UnicodeError, ValueError, RecursionError) as error:
        raise InvalidBundle("Manifest must be bounded unambiguous UTF-8 JSON") from error
    return manifest


def parse_manifest(raw, target):
    manifest = decode_manifest(raw)
    fields = {"schema_version", "product", "version", "target", "profile", "compiler",
              "source_commit", "source_tree", "runtime_prerequisites", "created_at", "files"}
    require(isinstance(manifest, dict) and set(manifest) == fields, "Unsupported manifest fields")
    require(type(manifest["schema_version"]) is int and manifest["schema_version"] == 1,
            "Unsupported bundle schema")
    require(target == TARGET and manifest["target"] == target and manifest["product"] == "Sift",
            "Bundle target/product differs from the requested supported target")
    require(manifest["profile"] in ("dev", "release", "release-dev"), "Unsupported build profile")
    require(text(manifest["version"], 256) and text(manifest["compiler"], 256)
            and text(manifest["created_at"], 64), "Malformed build metadata")
    for name in ("source_commit", "source_tree"):
        require(isinstance(manifest[name], str) and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", manifest[name]),
                "Malformed source identity")
    prerequisites = manifest["runtime_prerequisites"]
    require(isinstance(prerequisites, list) and len(prerequisites) <= 64
            and all(text(item, 512) for item in prerequisites), "Malformed runtime prerequisites")
    entries = manifest["files"]
    require(isinstance(entries, list) and len(entries) == len(PAYLOAD), "Unexpected payload file count")
    files = {}
    total = 0
    for entry in entries:
        require(isinstance(entry, dict) and set(entry) == {"path", "bytes", "sha256", "authenticode"},
                "Unsupported file record")
        path = entry["path"]
        require(isinstance(path, str) and path in PAYLOAD and path not in files,
                "Unexpected, duplicate or substituted payload path")
        require(type(entry["bytes"]) is int and 0 < entry["bytes"] <= MAX_FILE, "Invalid payload size")
        require(isinstance(entry["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]),
                "Malformed payload digest")
        require(entry["authenticode"] is None or text(entry["authenticode"], 128),
                "Malformed native signature claim")
        total += entry["bytes"]
        require(total <= MAX_TOTAL, "Bundle exceeds its aggregate byte budget")
        files[path] = entry
    require(set(files) == PAYLOAD, "Payload inventory differs from the supported bundle")
    return manifest, files, total


def inventory(root):
    observed = set()

    def visit(directory, prefix):
        with os.scandir(directory) as entries:
            for entry in entries:
                path = prefix + entry.name
                if entry.is_dir(follow_symlinks=False):
                    require(path == "licenses", "Unexpected payload directory")
                    child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                    dir_fd=directory)
                    try:
                        visit(child, "licenses/")
                    finally:
                        os.close(child)
                else:
                    require(path in PAYLOAD or path == "bundle.json", "Unexpected payload entry")
                    info = entry.stat(follow_symlinks=False)
                    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                            "Payload symlinks, hardlinks and special files are forbidden")
                    observed.add(path)
    visit(root, "")
    require(observed == PAYLOAD | {"bundle.json"}, "Payload files are missing")


def hash_payload(root, path, expected):
    directory = os.dup(root)
    try:
        parts = path.split("/")
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        fd, before = open_regular(parts[-1], directory)
        with os.fdopen(fd, "rb") as stream:
            require(before.st_size == expected["bytes"], "Payload size differs from signed manifest")
            digest = hashlib.sha256()
            remaining = expected["bytes"]
            while remaining:
                chunk = stream.read(min(1024 * 1024, remaining))
                require(chunk, "Payload was truncated")
                digest.update(chunk)
                remaining -= len(chunk)
            require(not stream.read(1) and stamp(before) == stamp(os.fstat(stream.fileno())),
                    "Payload changed during inspection")
            require(digest.hexdigest() == expected["sha256"], "Payload digest differs from signed manifest")
    finally:
        os.close(directory)


def verify(bundle, signature_path, public_key_path, target=TARGET, openssl="openssl"):
    require(sys.platform.startswith("linux"), "This offline verifier currently requires Linux")
    bundle = Path(bundle)
    # Root itself must not be a symlink; all traversal below uses pinned descriptors.
    root = os.open(bundle, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        resolved = bundle.resolve()
        lexical = Path(os.path.abspath(bundle))
        executable = shutil.which(openssl)
        require(executable is not None, "OpenSSL is unavailable")
        openssl_path = Path(executable).resolve()
        require(not openssl_path.is_relative_to(resolved)
                and not Path(os.path.abspath(executable)).is_relative_to(lexical),
                "OpenSSL must be a trusted tool outside the bundle")
        openssl = str(openssl_path)
        for external in (signature_path, public_key_path):
            require(not Path(external).resolve().is_relative_to(resolved)
                    and not Path(os.path.abspath(external)).is_relative_to(lexical),
                    "Signature and operator trust key must be outside the bundle")
        raw = read_bounded("bundle.json", MAX_MANIFEST, root)
        signature = read_bounded(signature_path, 64)
        public_key = read_bounded(public_key_path, 16 * 1024)
        key_hash = verify_signature(raw, signature, public_key, openssl)
        manifest, files, total = parse_manifest(raw, target)
        inventory(root)
        for path, entry in files.items():
            hash_payload(root, path, entry)
        inventory(root)
        require(read_bounded("bundle.json", MAX_MANIFEST, root) == raw,
                "Manifest changed during inspection")
        return {"schema_version": 1, "target": target, "version": manifest["version"],
                "profile": manifest["profile"], "source_commit": manifest["source_commit"],
                "source_tree": manifest["source_tree"], "manifest_sha256": hashlib.sha256(raw).hexdigest(),
                "public_key_spki_sha256": key_hash, "signature": "ed25519_verified",
                "payload_files": len(files), "payload_bytes": total,
                "native_acceptance": "not_performed", "source_build_attestation": "not_performed"}
    finally:
        os.close(root)


def parse_linux_receipt(raw, target, artifact_name):
    manifest = decode_manifest(raw)
    fields = {"source_commit", "source_tree", "source_date_epoch", "schema_version", "format",
              "product", "version", "target", "architecture", "profile", "package", "deb_version",
              "compiler", "host", "build_options", "maintainer", "elf", "dependencies",
              "observed_runtime_packages", "source_build_attestation", "native_signing", "symlinks",
              "artifact", "bytes", "sha256"}
    require(isinstance(manifest, dict) and set(manifest) == fields, "Unsupported Linux receipt fields")
    require(type(manifest["schema_version"]) is int and manifest["schema_version"] == 1
            and manifest["format"] == "sift-linux-deb-v1", "Unsupported Linux receipt schema")
    require(target == LINUX_TARGET and manifest["target"] == target
            and manifest["architecture"] == "amd64" and manifest["product"] == "Sift",
            "Linux receipt target/product differs from the requested supported target")
    require(manifest["profile"] in ("dev", "release", "release-dev"), "Unsupported build profile")
    require(manifest["package"] == ("sift" if manifest["profile"] == "release" else "sift-development"),
            "Linux package/profile claim differs from the producer")
    for name in ("source_commit", "source_tree"):
        require(isinstance(manifest[name], str) and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", manifest[name]),
                "Malformed source identity")
    require(type(manifest["source_date_epoch"]) is int and 0 <= manifest["source_date_epoch"] < 2**63,
            "Malformed source timestamp")
    require(text(manifest["version"], 256) and text(manifest["compiler"], 256)
            and text(manifest["maintainer"], 512), "Malformed Linux build metadata")
    host = manifest["host"]
    require(isinstance(host, dict) and set(host) == {"id", "version_id"}
            and host["id"] in ("debian", "ubuntu") and isinstance(host["version_id"], str)
            and re.fullmatch(r"[0-9][0-9.]{0,63}", host["version_id"]), "Unsupported Linux host claim")
    version = manifest["deb_version"]
    require(isinstance(version, str) and re.fullmatch(r"[0-9][A-Za-z0-9.+~_-]{0,255}", version),
            "Malformed Debian version")
    expected = f"{manifest['package']}_{version}_amd64_{host['id']}{host['version_id']}.deb"
    require(manifest["artifact"] == artifact_name == expected, "Linux artifact name differs from signed receipt")
    for name, count, limit in [("dependencies", 128, 512), ("observed_runtime_packages", 128, 512),
                               ("build_options", 256, 4096)]:
        value = manifest[name]
        require(isinstance(value, list) and len(value) <= count and all(text(item, limit) for item in value),
                "Malformed Linux build claims")
    require(manifest["source_build_attestation"] == "not_performed"
            and manifest["native_signing"] == "not_performed", "Unsupported native attestation claim")
    elf = manifest["elf"]
    require(isinstance(elf, dict) and set(elf) == set(BINARIES), "Unexpected ELF claim inventory")
    for value in elf.values():
        require(isinstance(value, dict) and set(value) == {"interpreter", "needed"}
                and value["interpreter"] == "/lib64/ld-linux-x86-64.so.2"
                and isinstance(value["needed"], list) and 0 < len(value["needed"]) <= 256
                and all(isinstance(item, str) and re.fullmatch(r"[A-Za-z0-9_.+-]{1,256}", item)
                        for item in value["needed"]), "Malformed ELF claims")
    links = manifest["symlinks"]
    expected_links = {"usr/bin/" + name: "../lib/sift/" + name for name in BINARIES}
    require(isinstance(links, list) and len(links) == len(expected_links), "Unexpected link claim inventory")
    observed = {}
    for value in links:
        require(isinstance(value, dict) and set(value) == {"path", "target"}
                and isinstance(value["path"], str) and value["path"] in expected_links
                and value["path"] not in observed and value["target"] == expected_links[value["path"]],
                "Malformed link claim")
        observed[value["path"]] = value["target"]
    require(type(manifest["bytes"]) is int and 0 < manifest["bytes"] <= MAX_FILE,
            "Invalid Linux artifact size")
    require(isinstance(manifest["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", manifest["sha256"]),
            "Malformed artifact digest")
    return manifest


def verify_linux_artifact(artifact, receipt, signature_path, public_key_path,
                          target=LINUX_TARGET, openssl="openssl"):
    require(sys.platform.startswith("linux"), "This offline verifier currently requires Linux")
    artifact, receipt = Path(artifact), Path(receipt)
    require(re.fullmatch(r"[A-Za-z0-9.+~_-]+\.deb", artifact.name), "Unsafe Linux artifact name")
    require(receipt.name == artifact.name + ".json" and receipt.parent.resolve() == artifact.parent.resolve(),
            "Use the matching producer receipt beside the Linux artifact")
    root = os.open(artifact.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        before_root = os.fstat(root)
        resolved, lexical = artifact.parent.resolve(), Path(os.path.abspath(artifact.parent))
        executable = shutil.which(openssl)
        require(executable is not None, "OpenSSL is unavailable")
        trusted = Path(executable).resolve()
        require(not trusted.is_relative_to(resolved)
                and not Path(os.path.abspath(executable)).is_relative_to(lexical),
                "OpenSSL must be a trusted tool outside the artifact directory")
        for path in (signature_path, public_key_path):
            require(not Path(path).resolve().is_relative_to(resolved)
                    and not Path(os.path.abspath(path)).is_relative_to(lexical),
                    "Signature and operator trust key must be outside the artifact directory")
        raw = read_bounded(receipt.name, MAX_MANIFEST, root)
        key_hash = verify_signature(raw, read_bounded(signature_path, 64),
                                    read_bounded(public_key_path, 16 * 1024), str(trusted))
        manifest = parse_linux_receipt(raw, target, artifact.name)
        fd, before = open_regular(artifact.name, root)
        with os.fdopen(fd, "rb") as stream:
            require(before.st_size == manifest["bytes"], "Artifact size differs from signed receipt")
            digest = hashlib.sha256()
            remaining = before.st_size
            while remaining:
                chunk = stream.read(min(1024 * 1024, remaining))
                require(chunk, "Artifact was truncated")
                digest.update(chunk)
                remaining -= len(chunk)
            require(not stream.read(1) and stamp(before) == stamp(os.fstat(stream.fileno())),
                    "Artifact changed during inspection")
            require(digest.hexdigest() == manifest["sha256"], "Artifact digest differs from signed receipt")
        require(stamp(before) == stamp(os.stat(artifact.name, dir_fd=root, follow_symlinks=False)),
                "Artifact path changed during inspection")
        require(read_bounded(receipt.name, MAX_MANIFEST, root) == raw, "Receipt changed during inspection")
        after_root = os.stat(artifact.parent, follow_symlinks=False)
        require((before_root.st_dev, before_root.st_ino) == (after_root.st_dev, after_root.st_ino),
                "Artifact directory changed during inspection")
        return {"schema_version": 1, "format": manifest["format"], "scope": "signed_artifact_bytes",
                "target": target, "version": manifest["version"], "profile": manifest["profile"],
                "source_commit": manifest["source_commit"], "source_tree": manifest["source_tree"],
                "manifest_sha256": hashlib.sha256(raw).hexdigest(), "public_key_spki_sha256": key_hash,
                "signature": "ed25519_verified", "artifact": artifact.name,
                "artifact_bytes": before.st_size, "artifact_sha256": digest.hexdigest(),
                "archive_format_validation": "not_performed", "native_acceptance": "not_performed",
                "source_build_attestation": "not_performed"}
    finally:
        os.close(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--bundle", type=Path)
    inputs.add_argument("--artifact", type=Path, help="Native Linux .deb artifact")
    parser.add_argument("--receipt", type=Path, help="Matching Linux producer .deb.json receipt")
    parser.add_argument("--signature", required=True, type=Path, help="External raw Ed25519 signature (64 bytes)")
    parser.add_argument("--public-key", required=True, type=Path, help="Operator-selected external public PEM")
    parser.add_argument("--target", required=True, choices=[TARGET, LINUX_TARGET])
    parser.add_argument("--openssl", default="openssl")
    args = parser.parse_args()
    try:
        if args.artifact:
            require(args.receipt is not None, "Linux artifact verification requires its producer receipt")
            result = verify_linux_artifact(args.artifact, args.receipt, args.signature,
                                           args.public_key, args.target, args.openssl)
        else:
            require(args.receipt is None, "Windows bundle verification does not use a Linux receipt")
            result = verify(args.bundle, args.signature, args.public_key, args.target, args.openssl)
        print(json.dumps(result, sort_keys=True))
    except (InvalidBundle, OSError) as error:
        # Never forward OpenSSL diagnostics, payload content or public-key bytes.
        message = str(error) if isinstance(error, InvalidBundle) else "Bundle input could not be opened safely"
        print(f"Bundle verification refused: {message}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
