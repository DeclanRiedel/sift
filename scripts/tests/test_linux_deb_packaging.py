"""Real native Debian construction with synthetic payload identity; no Sift runtime claim."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "package-linux-deb.py"
spec = importlib.util.spec_from_file_location("linux_deb_packaging", SCRIPT)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


@unittest.skipUnless(sys.platform.startswith("linux") and platform.machine() == "x86_64", "Native amd64 Linux")
class DebianPackagingTests(unittest.TestCase):
    def command(self, *args, **kwargs):
        return subprocess.run(list(map(str, args)), check=True, capture_output=True, **kwargs).stdout

    def setUp(self):
        for name in ("git", "readelf", "ldd", "dpkg-deb", "dpkg-shlibdeps", "desktop-file-validate"):
            if not shutil.which(name):
                self.fail("Acceptance requires native tool: " + name)
        self.temporary = tempfile.TemporaryDirectory(prefix="sift-deb-acceptance-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.command("git", "init", "-q", self.repo)
        self.command("git", "-C", self.repo, "config", "user.name", "Fixture")
        self.command("git", "-C", self.repo, "config", "user.email", "fixture@example.invalid")
        for name in ("LICENSE", "crates/ui/assets/icons/LICENSE.qlementine-icons",
                     "vendor/tiberius/LICENSE-APACHE.txt", "vendor/tiberius/LICENSE-MIT.txt"):
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("Synthetic notice\n")
        image = SCRIPT.parents[1] / "crates/desktop/assets/sift-icon.png"
        (self.repo / "crates/desktop/assets").mkdir(parents=True)
        shutil.copyfile(image, self.repo / "crates/desktop/assets/sift-icon.png")
        (self.repo / ".gitignore").write_text(".env\n.sift/\ntarget/\n")
        (self.repo / ".env").write_text("SYNTHETIC-RUNTIME-DATA\n")
        self.command("git", "-C", self.repo, "add", ".")
        self.command("git", "-C", self.repo, "-c", "commit.gpgsign=false", "commit", "-qm", "fixture")
        self.identity = packager.source_identity(self.repo)
        binary_directory = self.root / "binaries"
        binary_directory.mkdir()
        self.binaries = {}
        for name in packager.BINARIES:
            self.binaries[name] = binary_directory / name
            shutil.copyfile("/usr/bin/true", self.binaries[name])
        self.metadata = dict(self.identity, schema_version=1, format="sift-linux-deb-v1",
                             maintainer="Fixture <fixture@example.invalid>", product="Sift", version="0.1.0",
                             target="x86_64-unknown-linux-gnu", architecture="amd64", profile="dev",
                             package="sift-development", deb_version="0.1.0~dev.fixture-1",
                             compiler="Synthetic ELF fixture", host=packager.host_release(), build_options=[])

    def stage(self, name="stage"):
        path = self.root / name
        path.mkdir()
        metadata = packager.build_stage(self.repo, self.binaries, path, self.metadata,
                                       "Fixture <fixture@example.invalid>")
        return path, metadata

    def test_native_archive_layout_dependencies_modes_hashes_and_source_exclusion(self):
        old_mask = os.umask(0o077)
        try:
            stage, metadata = self.stage()
        finally:
            os.umask(old_mask)
        archive = self.root / "fixture.deb"
        packager.write_package(stage, archive, self.identity["source_date_epoch"])
        self.assertEqual(self.command("dpkg-deb", "-f", archive, "Package").strip(), b"sift-development")
        depends = self.command("dpkg-deb", "-f", archive, "Depends").decode()
        for name in packager.RUNTIME_PACKAGES:
            self.assertIn(name, depends)
        self.assertIn("libc6", depends)
        self.assertIn("libfontconfig1", depends, "Runtime-loaded Fontconfig is absent from ELF NEEDED")
        control = self.root / "control"
        self.command("dpkg-deb", "-e", archive, control)
        self.assertEqual({p.name for p in control.iterdir()}, {"control"})
        extracted = self.root / "extracted"
        self.command("dpkg-deb", "-x", archive, extracted)
        for name in packager.BINARIES:
            self.assertEqual(os.readlink(extracted / "usr/bin" / name), "../lib/sift/" + name)
            self.assertEqual((extracted / "usr/lib/sift" / name).stat().st_mode & 0o777, 0o755)
        for entry in metadata["files"]:
            path = extracted / entry["path"]
            self.assertEqual(path.stat().st_size, entry["bytes"])
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), entry["sha256"])
            self.assertEqual(oct(path.stat().st_mode & 0o777), entry["mode"])
        doc = extracted / "usr/share/doc/sift-development"
        stored = json.loads((doc / "bundle.json").read_text())
        self.assertEqual(stored["source_commit"], self.identity["source_commit"])
        self.assertEqual(stored["source_build_attestation"], "not_performed")
        with tarfile.open(doc / "source.tar") as source:
            self.assertNotIn("sift-source/.env", source.getnames())
        self.command("desktop-file-validate", extracted / "usr/share/applications/dev.sift.Sift.desktop")
        # Package construction repeats exactly for identical staged bytes/metadata.
        second = self.root / "second.deb"
        packager.write_package(stage, second, self.identity["source_date_epoch"])
        self.assertEqual(packager.digest(archive), packager.digest(second))

    def test_published_native_archive_receipt_supports_external_ed25519_verification(self):
        if not shutil.which("openssl"):
            self.fail("Signature acceptance requires OpenSSL")
        script = SCRIPT.with_name("verify-desktop-bundle.py")
        spec = importlib.util.spec_from_file_location("native_receipt_verifier", script)
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        stage, metadata = self.stage()
        name = f"{metadata['package']}_{metadata['deb_version']}_amd64_{metadata['host']['id']}{metadata['host']['version_id']}.deb"
        output = self.root / "published"
        output.mkdir()
        with tempfile.TemporaryDirectory(dir=self.root) as temporary:
            archive = Path(temporary) / "package.deb"
            packager.write_package(stage, archive, self.identity["source_date_epoch"])
            artifact = packager.publish(output, archive, name, {key: value for key, value in metadata.items() if key != "files"})
        private, public, signature = [self.root / name for name in ("private.pem", "public.pem", "receipt.sig.raw")]
        self.command("openssl", "genpkey", "-algorithm", "Ed25519", "-out", private)
        private.chmod(0o600)
        self.command("openssl", "pkey", "-in", private, "-pubout", "-out", public)
        receipt = artifact.with_name(artifact.name + ".json")
        self.command("openssl", "pkeyutl", "-sign", "-rawin", "-inkey", private,
                     "-in", receipt, "-out", signature)
        result = verifier.verify_linux_artifact(artifact, receipt, signature, public)
        self.assertEqual(result["artifact_sha256"], packager.digest(artifact))
        self.assertEqual(result["scope"], "signed_artifact_bytes")
        self.assertEqual(result["native_acceptance"], "not_performed")

    def test_unprivileged_alternate_root_install_remove_preserves_other_data(self):
        stage, _ = self.stage()
        archive = self.root / "lifecycle.deb"
        packager.write_package(stage, archive, self.identity["source_date_epoch"])
        native_root = self.root / "alternate-root"
        admin = native_root / "var/lib/dpkg"
        admin.mkdir(parents=True)
        log = native_root / "var/log/dpkg.log"
        log.parent.mkdir(parents=True)
        base = ["dpkg", "--force-not-root", "--force-depends", "--root=" + str(native_root),
                "--admindir=" + str(admin), "--log=" + str(log)]
        self.command(*base, "--install", archive)
        for name in packager.BINARIES:
            self.assertTrue((native_root / "usr/lib/sift" / name).is_file())
        unrelated = native_root / "usr/lib/sift/unrelated"
        unrelated.write_text("retain unrelated data")
        runtime = native_root / "runtime/sentinel"
        runtime.parent.mkdir()
        runtime.write_text("retain runtime data")
        self.command(*base, "--remove", "sift-development")
        self.assertEqual(unrelated.read_text(), "retain unrelated data")
        self.assertEqual(runtime.read_text(), "retain runtime data")
        for name in packager.BINARIES:
            self.assertFalse((native_root / "usr/lib/sift" / name).exists())
        self.assertTrue(log.is_file())

    def test_dirty_source_tracked_runtime_and_in_tree_output_refuse(self):
        (self.repo / "untracked").write_text("draft")
        with self.assertRaises(packager.PackagingError):
            packager.source_identity(self.repo)
        (self.repo / "untracked").unlink()
        self.command("git", "-C", self.repo, "add", "-f", ".env")
        with self.assertRaisesRegex(packager.PackagingError, "Runtime/build data is tracked"):
            packager.source_identity(self.repo)
        self.command("git", "-C", self.repo, "reset", "-q", "HEAD", "--", ".env")
        with self.assertRaisesRegex(packager.PackagingError, "outside"):
            packager.produce(self.repo, self.repo / "artifact", "dev", "1", "Fixture", True)
        with self.assertRaisesRegex(packager.PackagingError, "dev-profile"):
            packager.produce(self.repo, self.root / "output", "release", "1", "Fixture", True)

    def test_private_search_paths_and_changed_binary_types_refuse(self):
        source = self.root / "tiny.c"
        source.write_text("int main(void) { return 0; }\n")
        binary = self.root / "private-rpath"
        self.command("cc", source, "-Wl,-rpath,/nix/store/unsupported-runtime", "-o", binary)
        with self.assertRaisesRegex(packager.PackagingError, "Private/Nix"):
            packager.inspect_elf(binary)
        linked = self.root / "alias"
        linked.symlink_to("/usr/bin/true")
        with self.assertRaises(packager.PackagingError):
            packager.inspect_elf(linked)

    def test_existing_and_concurrently_created_outputs_are_preserved(self):
        output = self.root / "output"
        output.mkdir()
        staged = self.root / "staged.deb"
        staged.write_bytes(b"Synthetic publication bytes")
        receipt = output / "artifact.deb.json"
        receipt.write_bytes(b"KEEP")
        with self.assertRaises(packager.PackagingError):
            packager.publish(output, staged, "artifact.deb", {})
        self.assertEqual(receipt.read_bytes(), b"KEEP")
        receipt.unlink()
        original_link = os.link

        def race(source, destination):
            if Path(destination) == receipt:
                receipt.write_bytes(b"COMPETING RECEIPT")
            return original_link(source, destination)

        with mock.patch.object(packager.os, "link", side_effect=race):
            with self.assertRaises(FileExistsError):
                packager.publish(output, staged, "artifact.deb", {})
        self.assertEqual(receipt.read_bytes(), b"COMPETING RECEIPT")
        self.assertFalse((output / "artifact.deb").exists())
        self.assertFalse((output / "artifact.deb.sha256").exists())

    def test_development_versions_sort_before_release_and_input_is_bounded(self):
        dev = packager.package_version("1.2.3", "dev", "a" * 40, "1")
        self.command("dpkg", "--compare-versions", dev, "lt", "1.2.3-1")
        prerelease = packager.package_version("1.2.3-beta.1", "release", "a" * 40, "1")
        self.command("dpkg", "--compare-versions", prerelease, "lt", "1.2.3-1")
        for version, revision in [("version\nDepends: anything", "1"), ("1.2.3", "0")]:
            with self.assertRaises(packager.PackagingError):
                packager.package_version(version, "dev", "a" * 40, revision)


if __name__ == "__main__":
    unittest.main()
