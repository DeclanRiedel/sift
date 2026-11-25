import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "desktop_channel", Path(__file__).resolve().parents[1] / "build-desktop-channel.py")
CHANNEL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHANNEL)


class DesktopChannelTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        source = self.root / "fixture.c"
        source.write_text("int main(void) { return 0; }\n")
        self.binary = self.root / "fixture"
        subprocess.run(["cc", str(source), "-o", str(self.binary)], check=True, capture_output=True)
        for name in CHANNEL.PAYLOADS:
            shutil.copyfile(self.binary, self.dist / f"{name}-linux-x86_64")

    def manifest(self, **overrides):
        values = dict(dist=self.dist, origin="https://github.com/example/sift/releases/download/desktop-dev-1001",
                      sequence=1001, version="0.1.0-dev.1001", commit="a" * 40, protocol=5)
        values.update(overrides)
        return CHANNEL.build_manifest(**values)

    def test_native_sibling_inventory_has_exact_lengths_hashes_and_immutable_urls(self):
        manifest = self.manifest()
        self.assertEqual(manifest["source_commit"], "a" * 40)
        self.assertEqual(manifest["channel"], "development")
        files = manifest["targets"][0]["files"]
        self.assertEqual({f["name"] for f in files}, set(CHANNEL.PAYLOADS))
        import hashlib
        for payload in files:
            path = self.dist / f'{payload["name"]}-linux-x86_64'
            self.assertEqual(payload["bytes"], path.stat().st_size)
            self.assertEqual(payload["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertTrue(payload["url"].endswith("/desktop-dev-1001/" + path.name))

    def test_missing_aliased_and_oversized_inputs_are_rejected(self):
        path = self.dist / "sift-desktop-linux-x86_64"
        path.unlink()
        with self.assertRaises(ValueError): self.manifest()
        path.symlink_to(self.binary)
        with self.assertRaises(ValueError): self.manifest()
        path.unlink()
        path.hardlink_to(self.binary)
        with self.assertRaises(ValueError): self.manifest()
        path.unlink()
        with path.open("wb") as stream:
            stream.truncate(512 * 1024 * 1024 + 1)
        with self.assertRaises(ValueError): self.manifest()

    def test_private_runtime_paths_are_rejected_without_executing_payload(self):
        path = self.dist / "sift-desktop-linux-x86_64"
        subprocess.run(["cc", str(self.root / "fixture.c"), "-Wl,-rpath,/nix/store/fixture", "-o", str(path)],
                       check=True, capture_output=True)
        with self.assertRaises(ValueError): self.manifest()

    def test_channel_identity_and_transport_are_bounded(self):
        for overrides in [dict(origin="http://updates.invalid"), dict(origin="https://secret@updates.invalid"),
                          dict(sequence=0), dict(sequence=2**64), dict(commit="../bad"),
                          dict(version="0.1.0"), dict(protocol=0)]:
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.manifest(**overrides)


if __name__ == "__main__":
    unittest.main()
