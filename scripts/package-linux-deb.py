#!/usr/bin/env python3
"""Build a native amd64 Debian desktop package from a clean Sift checkout."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile

BINARIES = ("sift-desktop", "sift-server", "sift-launcher", "sift-admin", "sift-remote", "sift")
RUNTIME_PACKAGES = ("libvulkan1", "libwayland-client0", "libxkbcommon0", "libxcb1")
APP_ID = "dev.sift.Sift"
LOW_MEMORY_PACKAGES = ("sift-server", "sift-ui", "sift-workspace-ui", "sift-desktop")


class PackagingError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise PackagingError(message)


def run(args, cwd=None, capture=True, env=None):
    result = subprocess.run(list(map(str, args)), cwd=cwd, env=env,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None, check=False)
    if result.returncode:
        if capture and result.stderr:
            sys.stderr.write(result.stderr.decode("utf-8", errors="replace")[-8000:])
        raise PackagingError(f"{args[0]} failed (exit {result.returncode})")
    return result.stdout.decode().strip() if capture else ""


def build_binaries(args, repo, package_ids):
    artifacts = {}
    with subprocess.Popen([*args, "--message-format=json-render-diagnostics"], cwd=repo,
                          stdout=subprocess.PIPE, text=True, start_new_session=True) as process:
        try:
            for line in process.stdout:
                record = json.loads(line)
                if record.get("reason") == "compiler-message":
                    rendered = record.get("message", {}).get("rendered")
                    if rendered:
                        sys.stderr.write(rendered)
                if (record.get("reason") == "compiler-artifact"
                        and record.get("package_id") in package_ids
                        and "bin" in record.get("target", {}).get("kind", [])
                        and record.get("executable")):
                    name = record["target"]["name"]
                    if name in BINARIES:
                        path = Path(record["executable"])
                        require(name not in artifacts or artifacts[name] == path,
                                "Cargo returned ambiguous executable artifacts")
                        artifacts[name] = path
            require(process.wait() == 0, "Cargo binary build failed")
        except BaseException:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            raise
    require(set(artifacts) == set(BINARIES), "Cargo did not return all six executable artifacts")
    return artifacts


def native_env():
    env = os.environ.copy()
    for key in ("LD_LIBRARY_PATH", "LD_PRELOAD"):
        env.pop(key, None)
    env["LC_ALL"] = "C"
    env["PATH"] = "/usr/bin:/bin"
    return env


def control_text(value):
    return isinstance(value, str) and 0 < len(value) <= 512 and all(ord(c) >= 32 and ord(c) != 127 for c in value)


def source_identity(repo):
    require(Path(run(["git", "rev-parse", "--show-toplevel"], repo)).resolve() == repo,
            "Package from the repository root")
    require(not run(["git", "ls-files", "--", ".env", ".sift", ".cache", "target"], repo),
            "Runtime/build data is tracked; refusing to archive it")
    require(not run(["git", "status", "--porcelain", "--untracked-files=normal"], repo),
            "Packaging requires a clean source checkout")
    return {"source_commit": run(["git", "rev-parse", "HEAD"], repo),
            "source_tree": run(["git", "rev-parse", "HEAD^{tree}"], repo),
            "source_date_epoch": int(run(["git", "show", "-s", "--format=%ct", "HEAD"], repo))}


def host_release():
    values = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        if line.startswith(("ID=", "VERSION_ID=")):
            name, value = line.split("=", 1)
            values[name] = value.strip('"')
    require(values.get("ID") in ("debian", "ubuntu")
            and re.fullmatch(r"[0-9][0-9.]*", values.get("VERSION_ID", "")),
            "Only native Debian/Ubuntu host releases are supported")
    return {"id": values["ID"], "version_id": values["VERSION_ID"]}


def package_version(version, profile, tree, revision):
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version),
            "Unsupported Cargo version")
    require(re.fullmatch(r"[1-9][0-9]*", revision), "Revision must be a positive integer")
    result = version.replace("-", "~", 1)
    if profile != "release":
        result += "~" + profile.replace("-", ".") + "." + tree[:12]
    return result + "-" + revision


def inspect_elf(path):
    require(path.is_file() and not path.is_symlink(), "Build binaries must be ordinary files")
    env = native_env()
    header = run(["readelf", "-h", path], env=env)
    require("ELF64" in header and "Advanced Micro Devices X86-64" in header,
            "Package requires native x86-64 ELF binaries")
    segments = run(["readelf", "-l", path], env=env)
    require("[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]" in segments,
            "Package requires the system GNU/Linux x86-64 loader")
    dynamic = run(["readelf", "-d", path], env=env)
    require(not re.search(r"\((?:RPATH|RUNPATH)\)", dynamic),
            "Private/Nix runtime search paths are not supported in native Debian packages")
    needed = re.findall(r"\(NEEDED\).*\[([^\]]+)\]", dynamic)
    require(needed and all(re.fullmatch(r"[A-Za-z0-9_.+-]+", name) for name in needed),
            "Invalid native shared-library metadata")
    resolved = run(["ldd", path], env=env)
    require("not found" not in resolved and "/nix/store/" not in resolved,
            "Native runtime libraries must resolve without developer/Nix paths")
    for line in resolved.splitlines():
        for name in re.findall(r"(?:=>\s+|^\s*)(/[^\s]+)", line):
            location = Path(name).resolve()
            require(location.is_relative_to("/usr/lib") or location.is_relative_to("/lib"),
                    "Native runtime libraries must come from system library directories")
    return {"interpreter": "/lib64/ld-linux-x86-64.so.2", "needed": needed}


def copy_file(source, target, mode=0o644):
    require(source.is_file() and not source.is_symlink(), "Payload source must be an ordinary file")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(mode)


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def payload_files(stage):
    return [{"path": path.relative_to(stage).as_posix(), "bytes": path.stat().st_size,
             "sha256": digest(path), "mode": oct(path.stat().st_mode & 0o777)}
            for path in sorted((stage / "usr").rglob("*"))
            if path.is_file() and not path.is_symlink()]


def normalize_modes(stage):
    stage.chmod(0o755)
    for path in stage.rglob("*"):
        if not path.is_symlink():
            path.chmod(0o755 if path.is_dir() or path.parent == stage / "usr/lib/sift" else 0o644)


def build_stage(repo, binaries, stage, metadata, maintainer):
    require(control_text(maintainer), "A single-line maintainer identity is required")
    package = metadata["package"]
    doc = stage / "usr/share/doc" / package
    lib = stage / "usr/lib/sift"
    binary_info = {}
    for name in BINARIES:
        copy_file(binaries[name], lib / name, 0o755)
        binary_info[name] = inspect_elf(lib / name)
        link = stage / "usr/bin" / name
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to("../lib/sift/" + name)
    for source, target in [
        ("LICENSE", "LICENSE"),
        ("crates/ui/assets/icons/LICENSE.qlementine-icons", "licenses/LICENSE.qlementine-icons"),
        ("vendor/tiberius/LICENSE-APACHE.txt", "licenses/tiberius-APACHE.txt"),
        ("vendor/tiberius/LICENSE-MIT.txt", "licenses/tiberius-MIT.txt"),
    ]:
        copy_file(repo / source, doc / target)
    run(["git", "archive", "--format=tar", "--prefix=sift-source/", "--output", doc / "source.tar",
         metadata["source_commit"]], repo)
    desktop = stage / "usr/share/applications" / (APP_ID + ".desktop")
    desktop.parent.mkdir(parents=True, exist_ok=True)
    desktop.write_text("[Desktop Entry]\nType=Application\nName=" +
                       ("Sift" if package == "sift" else "Sift Development") +
                       "\nGenericName=Database IDE\nComment=SQL workspace for PostgreSQL, SQL Server and SQLite\n"
                       "Exec=/usr/bin/sift-desktop\nIcon=dev.sift.Sift\nTerminal=false\n"
                       "Categories=Development;Database;\nStartupNotify=true\nStartupWMClass=" + APP_ID + "\n")
    run(["desktop-file-validate", desktop], env=native_env())
    copy_file(repo / "crates/desktop/assets/sift-icon.png", stage / "usr/share/pixmaps" / (APP_ID + ".png"))
    doc.mkdir(parents=True, exist_ok=True)
    (doc / "README.txt").write_text(
        "Sift native Debian desktop package\n\n"
        "Launch sift-desktop. All six binaries remain siblings in /usr/lib/sift.\n"
        "Runtime files belong in the selected user instance, outside installed program files.\n"
        "A working X11/Wayland session, Vulkan ICD and user credential/session services are\n"
        "separate runtime prerequisites. This host-specific build is not a universal Linux binary.\n"
        "bundle.json records exact source/build claims and payload hashes. Native signing,\n"
        "clean-machine graphical acceptance and desktop updates remain separate gates.\n")
    control = stage / "DEBIAN/control"
    control.parent.mkdir(parents=True)
    opposite = "sift-development" if package == "sift" else "sift"
    base = (f"Package: {package}\nVersion: {metadata['deb_version']}\nArchitecture: amd64\n"
            f"Maintainer: {maintainer}\nSection: database\nPriority: optional\n"
            f"Conflicts: {opposite}\nReplaces: {opposite}\n"
            "Description: Native Sift SQL workspace\n PostgreSQL, SQL Server and SQLite workspace with bundled local helpers.\n")
    # dpkg-shlibdeps requires a source control file, independent of binary control.
    with tempfile.TemporaryDirectory(prefix="sift-shlibs-") as temporary:
        work = Path(temporary)
        (work / "debian").mkdir()
        (work / "debian/control").write_text(f"Source: sift\nMaintainer: {maintainer}\n\n" + base)
        generated = run(["dpkg-shlibdeps", "-O", *["-e" + str(lib / name) for name in BINARIES]], work,
                        env=native_env())
    require(generated.startswith("shlibs:Depends=") and "\n" not in generated,
            "Unexpected native dependency output")
    dependencies = generated.removeprefix("shlibs:Depends=").split(", ")
    require(dependencies and all(control_text(item) for item in dependencies), "Empty native dependencies")
    observed_runtime = []
    for name in RUNTIME_PACKAGES:
        value = run(["dpkg-query", "-W", "-f=${db:Status-Abbrev} ${binary:Package}=${Version}", name + ":amd64"],
                    env=native_env())
        require(value.startswith("ii "), "Required dynamically loaded runtime package is not installed")
        observed_runtime.append(value[3:].strip())
        if not any(re.match(re.escape(name) + r"(?:\s|:|$)", dependency) for dependency in dependencies):
            dependencies.append(name)
    metadata = dict(metadata, elf=binary_info, dependencies=dependencies,
                    observed_runtime_packages=observed_runtime,
                    source_build_attestation="not_performed", native_signing="not_performed")
    normalize_modes(stage)
    metadata["files"] = payload_files(stage)
    metadata["symlinks"] = [{"path": path.relative_to(stage).as_posix(), "target": os.readlink(path)}
                            for path in sorted((stage / "usr").rglob("*")) if path.is_symlink()]
    # This manifest excludes itself; the outer receipt hashes the complete .deb.
    (doc / "bundle.json").write_text(json.dumps(metadata, sort_keys=True, indent=2) + "\n")
    (doc / "bundle.json").chmod(0o644)
    size = sum(path.stat().st_size for path in (stage / "usr").rglob("*")
               if path.is_file() and not path.is_symlink())
    control.write_text(base + "Depends: " + ", ".join(dependencies) +
                       "\nRecommends: mesa-vulkan-drivers | vulkan-icd\nInstalled-Size: " + str((size + 1023) // 1024) + "\n")
    control.chmod(0o644)
    epoch = metadata["source_date_epoch"]
    for path in sorted(stage.rglob("*"), reverse=True):
        os.utime(path, (epoch, epoch), follow_symlinks=False)
    os.utime(stage, (epoch, epoch))
    return metadata


def write_package(stage, destination, epoch):
    env = native_env()
    env["SOURCE_DATE_EPOCH"] = str(epoch)
    run(["dpkg-deb", "--build", "--root-owner-group", "--uniform-compression", "-Zgzip", stage, destination],
        env=env)


def publish(output, staged, name, receipt):
    targets = [output / name, output / (name + ".sha256"), output / (name + ".json")]
    require(not any(path.exists() or path.is_symlink() for path in targets), "Package output already exists")
    checksum = digest(staged)
    receipt = dict(receipt, artifact=name, bytes=staged.stat().st_size, sha256=checksum)
    staged_receipt = staged.with_suffix(".receipt")
    staged_sum = staged.with_suffix(".sha256")
    staged_receipt.write_text(json.dumps(receipt, sort_keys=True, indent=2) + "\n")
    staged_sum.write_text(checksum + "  " + name + "\n")
    published = []
    try:
        for source, target in zip([staged, staged_sum, staged_receipt], targets):
            os.link(source, target)  # no replacement, including concurrent publishers
            published.append((target, target.stat().st_ino))
    except BaseException:
        for target, inode in reversed(published):
            if target.exists() and not target.is_symlink() and target.stat().st_ino == inode:
                target.unlink()
        raise
    return targets[0]


def produce(repo, output, profile, revision, maintainer, low_memory):
    require(sys.platform.startswith("linux") and platform.machine() == "x86_64", "Use a native x86-64 Linux host")
    require(profile in ("dev", "release", "release-dev"), "Unsupported build profile")
    require(not low_memory or profile == "dev", "Low-memory overrides are explicit dev-profile options only")
    repo = repo.resolve()
    output = output.resolve()
    require(not output.is_relative_to(repo), "Keep package output outside the source checkout")
    identity = source_identity(repo)
    require(run(["dpkg", "--print-architecture"], env=native_env()) == "amd64", "Use a native amd64 Debian host")
    host = host_release()
    compiler = run(["rustc", "--version"], repo)
    require("host: x86_64-unknown-linux-gnu" in run(["rustc", "-vV"], repo), "Use native GNU/Linux Rust")
    cargo_metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked", "--offline"], repo))
    desktops = [package for package in cargo_metadata["packages"] if package["name"] == "sift-desktop"]
    require(len(desktops) == 1, "Cannot identify the desktop Cargo package")
    version = desktops[0]["version"]
    deb_version = package_version(version, profile, identity["source_tree"], revision)
    run(["dpkg", "--validate-version", deb_version], env=native_env())
    package = "sift" if profile == "release" else "sift-development"
    name = f"{package}_{deb_version}_amd64_{host['id']}{host['version_id']}.deb"
    output.mkdir(parents=True, exist_ok=True)
    require(not any((output / (name + suffix)).exists() or (output / (name + suffix)).is_symlink()
                    for suffix in ("", ".sha256", ".json")), "Package output already exists")
    if maintainer is None:
        maintainer = run(["git", "config", "user.name"], repo) + " <" + run(["git", "config", "user.email"], repo) + ">"
    require(control_text(maintainer), "Configure a maintainer identity or supply --maintainer")
    options = []
    if low_memory:
        for package_name in LOW_MEMORY_PACKAGES:
            options += ["--config", f"profile.dev.package.{package_name}.codegen-units=256",
                        "--config", f"profile.dev.package.{package_name}.debug=0"]
    build = ["cargo", "build", "--locked", "--offline", "--profile", profile,
             "-p", "sift-desktop", "-p", "sift-server", "--bins", *options]
    package_ids = {package["id"] for package in cargo_metadata["packages"]
                   if package["name"] in ("sift-server", "sift-desktop")}
    require(len(package_ids) == 2, "Cannot identify native executable packages")
    binaries = build_binaries(build, repo, package_ids)
    require(source_identity(repo) == identity, "Source changed during compilation")
    with tempfile.TemporaryDirectory(prefix=".sift-deb-", dir=output) as temporary:
        root = Path(temporary)
        stage = root / "stage"
        stage.mkdir()
        metadata = dict(identity, schema_version=1, format="sift-linux-deb-v1", product="Sift", version=version,
                        target="x86_64-unknown-linux-gnu", architecture="amd64", profile=profile,
                        package=package, deb_version=deb_version, compiler=compiler, host=host,
                        build_options=options, maintainer=maintainer)
        metadata = build_stage(repo, binaries, stage, metadata, maintainer)
        require(source_identity(repo) == identity, "Source changed during staging")
        archive = root / "package.deb"
        write_package(stage, archive, identity["source_date_epoch"])
        require(source_identity(repo) == identity, "Source changed during packaging")
        receipt = {key: value for key, value in metadata.items() if key != "files"}
        result = publish(output, archive, name, receipt)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-directory", required=True, type=Path)
    parser.add_argument("--profile", choices=["dev", "release", "release-dev"], default="release")
    parser.add_argument("--revision", default="1")
    parser.add_argument("--maintainer")
    parser.add_argument("--low-memory", action="store_true")
    args = parser.parse_args()
    try:
        print(produce(Path(__file__).resolve().parents[1], args.output_directory, args.profile,
                      args.revision, args.maintainer, args.low_memory))
    except (PackagingError, OSError, json.JSONDecodeError) as error:
        print("Packaging refused: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
