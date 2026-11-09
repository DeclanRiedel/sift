"""Real cryptographic and filesystem refusal cases; payloads are synthetic bytes."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "verify-desktop-bundle.py"
spec = importlib.util.spec_from_file_location("desktop_bundle_verification", SCRIPT)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux descriptor verification")
class SignedBundleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not shutil.which("openssl"):
            raise RuntimeError("Acceptance requires OpenSSL")

    def run_openssl(self, *args):
        subprocess.run(["openssl", *map(str, args)], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True, timeout=10)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sift-bundle-acceptance-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        self.private = self.root / "disposable-private.pem"
        self.public = self.root / "operator-public.pem"
        self.signature = self.root / "bundle.sig.raw"
        self.run_openssl("genpkey", "-algorithm", "Ed25519", "-out", self.private)
        self.private.chmod(0o600)
        self.run_openssl("pkey", "-in", self.private, "-pubout", "-out", self.public)
        self.manifest = {
            "schema_version": 1, "product": "Sift", "version": "0.1.0", "target": verifier.TARGET,
            "profile": "dev", "compiler": "synthetic acceptance fixture",
            "source_commit": "a" * 40, "source_tree": "b" * 40,
            "runtime_prerequisites": ["Synthetic payloads are not executables"],
            "created_at": "2026-11-06T00:00:00Z", "files": [],
        }
        for name in sorted(verifier.PAYLOAD):
            path = self.bundle / name
            path.parent.mkdir(exist_ok=True)
            data = ("SYNTHETIC-NON-EXECUTABLE:" + name).encode()
            path.write_bytes(data)
            self.manifest["files"].append({"path": name, "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(), "authenticode": "NotSigned" if name.endswith(".exe") else None})
        self.sign()

    def sign(self, raw=None):
        (self.bundle / "bundle.json").write_bytes(
            raw if raw is not None else json.dumps(self.manifest, sort_keys=True).encode())
        self.run_openssl("pkeyutl", "-sign", "-rawin", "-inkey", self.private,
                         "-in", self.bundle / "bundle.json", "-out", self.signature)

    def verify(self):
        return verifier.verify(self.bundle, self.signature, self.public)

    def refused(self):
        with self.assertRaises((verifier.InvalidBundle, OSError)):
            self.verify()

    def test_verified_receipt_binds_exact_bytes_and_keeps_native_claims_separate(self):
        receipt = self.verify()
        self.assertEqual(receipt["payload_files"], 12)
        self.assertEqual(receipt["payload_bytes"], sum(item["bytes"] for item in self.manifest["files"]))
        self.assertEqual(receipt["manifest_sha256"], hashlib.sha256((self.bundle / "bundle.json").read_bytes()).hexdigest())
        self.assertEqual(receipt["signature"], "ed25519_verified")
        self.assertEqual(receipt["native_acceptance"], "not_performed")
        self.assertEqual(receipt["source_build_attestation"], "not_performed")
        command = [sys.executable, "-B", str(SCRIPT), "--bundle", str(self.bundle), "--signature",
                   str(self.signature), "--public-key", str(self.public), "--target", verifier.TARGET]
        result = subprocess.run(command, capture_output=True, check=True, timeout=15)
        self.assertEqual(json.loads(result.stdout), receipt)
        (self.bundle / "sift.exe").write_bytes(b"PRIVATE-SENTINEL-TAMPERED")
        result = subprocess.run(command, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertNotIn(b"PRIVATE-SENTINEL", result.stderr)

    def test_signature_key_algorithm_and_exact_manifest_bytes(self):
        original = self.signature.read_bytes()
        for invalid in [original[:-1], bytes([original[0] ^ 1]) + original[1:]]:
            with self.subTest(length=len(invalid)):
                self.signature.write_bytes(invalid)
                self.refused()
        self.signature.write_bytes(original)
        (self.bundle / "bundle.json").write_bytes((self.bundle / "bundle.json").read_bytes() + b" ")
        self.refused()
        self.sign()
        self.run_openssl("genpkey", "-algorithm", "Ed25519", "-out", self.root / "wrong-private.pem")
        self.run_openssl("pkey", "-in", self.root / "wrong-private.pem", "-pubout", "-out", self.public)
        self.refused()
        self.run_openssl("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", self.root / "ec-private.pem")
        self.run_openssl("pkey", "-in", self.root / "ec-private.pem", "-pubout", "-out", self.public)
        self.refused()

    def test_signed_but_ambiguous_or_invalid_manifest_is_refused(self):
        original = json.loads(json.dumps(self.manifest))
        for mutation in [
            lambda m: m.update(schema_version=True),
            lambda m: m.update(target="different-target"),
            lambda m: m.update(profile="unknown"),
            lambda m: m.update(source_commit="not-a-git-id"),
            lambda m: m.update(unrecognized_trust_claim=True),
            lambda m: m["files"][0].update(bytes=True),
            lambda m: m["files"][0].update(sha256="0"),
            lambda m: m["files"][0].update(path="../escape"),
            lambda m: m["files"][0].update(path="licenses\\escape"),
            lambda m: m["files"][0].update(path=m["files"][1]["path"]),
            lambda m: m["files"].pop(),
        ]:
            self.manifest = json.loads(json.dumps(original))
            mutation(self.manifest)
            self.sign()
            self.refused()
        raw = json.dumps(original).encode()
        self.sign(b'{"schema_version":1,' + raw[1:])
        self.refused()

    def test_inventory_and_file_types_cannot_substitute_signed_payload(self):
        target = self.bundle / "sift.exe"
        original = target.read_bytes()
        target.unlink()
        self.refused()
        target.write_bytes(original)
        (self.bundle / "extra.exe").write_bytes(b"extra")
        self.refused()
        (self.bundle / "extra.exe").unlink()
        target.unlink()
        external = self.root / "outside"
        external.write_bytes(original)
        target.symlink_to(external)
        self.refused()
        target.unlink()
        os.link(external, target)
        self.refused()
        target.unlink()
        os.mkfifo(target)
        self.refused()
        target.unlink()
        target.write_bytes(original)
        licenses = self.bundle / "licenses"
        licenses.rename(self.root / "outside-licenses")
        licenses.symlink_to(self.root / "outside-licenses", target_is_directory=True)
        self.refused()

    def test_bounded_inputs_and_bundle_tool_substitution_are_refused(self):
        marker = self.root / "payload-was-executed"
        fake = self.bundle / "openssl"
        fake.write_text("#!/bin/sh\n: > " + shlex.quote(str(marker)) + "\nexit 1\n")
        fake.chmod(0o700)
        with self.assertRaises(verifier.InvalidBundle):
            verifier.verify(self.bundle, self.signature, self.public, openssl=str(fake))
        self.assertFalse(marker.exists(), "No executable supplied by the bundle may run")
        fake.unlink()
        self.sign(b" " * (verifier.MAX_MANIFEST + 1))
        self.refused()
        self.sign()
        self.signature.unlink()
        os.mkfifo(self.signature)
        command = [sys.executable, "-B", str(SCRIPT), "--bundle", str(self.bundle),
                   "--signature", str(self.signature), "--public-key", str(self.public),
                   "--target", verifier.TARGET]
        result = subprocess.run(command, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")

    def test_change_after_read_cannot_pass_using_the_prechange_digest(self):
        target = self.bundle / "sift.exe"
        original = target.read_bytes()
        expected = next(entry for entry in self.manifest["files"] if entry["path"] == "sift.exe")
        real_sha256 = hashlib.sha256

        class MutatingDigest:
            def __init__(self):
                self.digest = real_sha256()

            def update(self, chunk):
                self.digest.update(chunk)
                # The original bytes have already reached the hash; length stays
                # unchanged, so the descriptor's final change check must refuse.
                target.write_bytes(b"!" + original[1:])

            def hexdigest(self):
                return self.digest.hexdigest()

        root = os.open(self.bundle, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with mock.patch.object(verifier.hashlib, "sha256", side_effect=MutatingDigest):
                with self.assertRaisesRegex(verifier.InvalidBundle, "changed during inspection"):
                    verifier.hash_payload(root, "sift.exe", expected)
        finally:
            os.close(root)

    def test_operator_key_cannot_be_supplied_from_the_bundle(self):
        embedded = self.bundle / "operator.pem"
        embedded.write_bytes(self.public.read_bytes())
        self.public = embedded
        self.refused()
        embedded.unlink()
        (self.bundle / "trust").symlink_to(self.root, target_is_directory=True)
        self.public = self.bundle / "trust" / "operator-public.pem"
        self.refused()



@unittest.skipUnless(sys.platform.startswith("linux"), "Linux descriptor verification")
class SignedLinuxArtifactTests(unittest.TestCase):
    def run_openssl(self, *args):
        subprocess.run(["openssl", *map(str, args)], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True, timeout=10)

    def setUp(self):
        if not shutil.which("openssl"):
            self.fail("Acceptance requires OpenSSL")
        temporary = tempfile.TemporaryDirectory(prefix="sift-linux-signature-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.output = self.root / "output"
        self.output.mkdir()
        self.artifact = self.output / "sift-development_0.1.0~dev.fixture-1_amd64_ubuntu25.10.deb"
        self.artifact.write_bytes(b"SYNTHETIC-PACKAGE-BYTES-NOT-NATIVE-ARCHIVE")
        self.receipt = self.artifact.with_name(self.artifact.name + ".json")
        self.private, self.public, self.signature = [self.root / name for name in
            ("disposable-private.pem", "operator-public.pem", "receipt.sig.raw")]
        self.run_openssl("genpkey", "-algorithm", "Ed25519", "-out", self.private)
        self.private.chmod(0o600)
        self.run_openssl("pkey", "-in", self.private, "-pubout", "-out", self.public)
        self.manifest = {"schema_version": 1, "format": "sift-linux-deb-v1", "product": "Sift",
            "version": "0.1.0", "target": verifier.LINUX_TARGET, "architecture": "amd64", "profile": "dev",
            "package": "sift-development", "deb_version": "0.1.0~dev.fixture-1", "compiler": "Synthetic fixture",
            "host": {"id": "ubuntu", "version_id": "25.10"}, "maintainer": "Fixture",
            "source_commit": "a" * 40, "source_tree": "b" * 40, "source_date_epoch": 1,
            "build_options": [], "dependencies": ["libc6"], "observed_runtime_packages": ["libc6=fixture"],
            "elf": {name: {"interpreter": "/lib64/ld-linux-x86-64.so.2", "needed": ["libc.so.6"]}
                    for name in verifier.BINARIES},
            "symlinks": [{"path": "usr/bin/" + name, "target": "../lib/sift/" + name}
                         for name in verifier.BINARIES],
            "native_signing": "not_performed", "source_build_attestation": "not_performed",
            "artifact": self.artifact.name, "bytes": self.artifact.stat().st_size,
            "sha256": hashlib.sha256(self.artifact.read_bytes()).hexdigest()}
        self.sign()

    def sign(self, raw=None):
        self.receipt.write_bytes(raw if raw is not None else json.dumps(self.manifest, sort_keys=True).encode())
        self.run_openssl("pkeyutl", "-sign", "-rawin", "-inkey", self.private,
                         "-in", self.receipt, "-out", self.signature)

    def verify(self, **kwargs):
        return verifier.verify_linux_artifact(self.artifact, self.receipt, self.signature,
                                              self.public, **kwargs)

    def refused(self, **kwargs):
        with self.assertRaises((verifier.InvalidBundle, OSError)):
            self.verify(**kwargs)

    def test_linux_receipt_authenticates_whole_bytes_without_native_claims(self):
        (self.output / "unrelated-build.deb").write_bytes(b"another artifact is allowed")
        result = self.verify()
        self.assertEqual(result["scope"], "signed_artifact_bytes")
        self.assertEqual(result["artifact_sha256"], self.manifest["sha256"])
        self.assertEqual(result["artifact_bytes"], self.manifest["bytes"])
        self.assertEqual(result["manifest_sha256"], hashlib.sha256(self.receipt.read_bytes()).hexdigest())
        self.assertEqual(result["signature"], "ed25519_verified")
        self.assertEqual(result["native_acceptance"], "not_performed")
        self.assertEqual(result["archive_format_validation"], "not_performed")
        command = [sys.executable, "-B", str(SCRIPT), "--artifact", str(self.artifact),
                   "--receipt", str(self.receipt), "--signature", str(self.signature),
                   "--public-key", str(self.public), "--target", verifier.LINUX_TARGET]
        self.assertEqual(json.loads(subprocess.run(command, capture_output=True, check=True, timeout=15).stdout), result)
        self.artifact.write_bytes(b"PRIVATE-SENTINEL-TAMPERED")
        failure = subprocess.run(command, capture_output=True, timeout=15)
        self.assertEqual(failure.returncode, 1)
        self.assertEqual(failure.stdout, b"")
        self.assertNotIn(b"PRIVATE-SENTINEL", failure.stderr)

    def test_linux_signature_receipt_and_artifact_are_independently_bound(self):
        signature = self.signature.read_bytes()
        self.signature.write_bytes(bytes([signature[0] ^ 1]) + signature[1:])
        self.refused()
        self.signature.write_bytes(signature)
        self.receipt.write_bytes(self.receipt.read_bytes() + b" ")
        self.refused()
        self.sign()
        self.run_openssl("genpkey", "-algorithm", "Ed25519", "-out", self.root / "wrong.pem")
        self.run_openssl("pkey", "-in", self.root / "wrong.pem", "-pubout", "-out", self.public)
        self.refused()

    def test_linux_signed_schema_target_paths_sizes_and_claims_are_checked(self):
        original = json.loads(json.dumps(self.manifest))
        for mutation in [lambda m: m.update(target=verifier.TARGET), lambda m: m.update(schema_version=True),
                         lambda m: m.update(artifact="../escape.deb"), lambda m: m.update(bytes=True),
                         lambda m: m.update(sha256="bad"), lambda m: m.update(package="sift"),
                         lambda m: m.update(source_commit="unknown"), lambda m: m.update(native_signing="trusted"),
                         lambda m: m["symlinks"][0].update(target="/outside"),
                         lambda m: m.update(unrecognized_claim=True)]:
            self.manifest = json.loads(json.dumps(original))
            mutation(self.manifest)
            self.sign()
            self.refused()
        self.sign(b'{"schema_version":1,' + json.dumps(original).encode()[1:])
        self.refused()

    def test_linux_links_special_files_and_untrusted_tools_are_refused(self):
        data = self.artifact.read_bytes()
        external = self.root / "replacement"
        external.write_bytes(data)
        for kind in ("symlink", "hardlink", "fifo"):
            self.artifact.unlink()
            if kind == "symlink":
                self.artifact.symlink_to(external)
            elif kind == "hardlink":
                os.link(external, self.artifact)
            else:
                os.mkfifo(self.artifact)
            self.refused()
            self.artifact.unlink()
            self.artifact.write_bytes(data)
        raw = self.receipt.read_bytes()
        self.receipt.unlink()
        external.write_bytes(raw)
        self.receipt.symlink_to(external)
        self.refused()
        self.receipt.unlink()
        self.receipt.write_bytes(raw)
        copied_key = self.output / "key.pem"
        shutil.copyfile(self.public, copied_key)
        with self.assertRaises(verifier.InvalidBundle):
            verifier.verify_linux_artifact(self.artifact, self.receipt, self.signature, copied_key)
        tool = self.output / "openssl"
        sentinel = self.root / "executed"
        tool.write_text("#!/bin/sh\ntouch " + shlex.quote(str(sentinel)) + "\n")
        tool.chmod(0o755)
        self.refused(openssl=str(tool))
        self.assertFalse(sentinel.exists())

    def test_linux_observed_mutation_and_bounded_receipts_are_refused(self):
        original = verifier.open_regular
        def mutate(path, directory=None):
            fd, before = original(path, directory)
            if path == self.artifact.name:
                self.artifact.write_bytes(b"x" * before.st_size)
            return fd, before
        with mock.patch.object(verifier, "open_regular", side_effect=mutate):
            self.refused()
        self.receipt.write_bytes(b"x" * (verifier.MAX_MANIFEST + 1))
        self.refused()


if __name__ == "__main__":
    unittest.main()
