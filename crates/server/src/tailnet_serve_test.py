"""Real ownership/confirmation behaviour with mocked remote commands; no SSH."""
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import tailnet_serve as serve


class ServeOwnershipTests(unittest.TestCase):
    def test_empty_daemon_configuration(self):
        self.assertEqual(serve.parse_status("null"), {})
        self.assertEqual(serve.parse_status('{"TCP":null}'), {})
        with self.assertRaises(ValueError):
            serve.parse_status("[]")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = patch.object(Path, "home", return_value=Path(self.temp.name))
        self.home.start()
        self.addCleanup(self.home.stop)
        self.config = {}
        self.status = patch.object(serve, "status", side_effect=lambda: copy.deepcopy(self.config))
        self.status.start()
        self.addCleanup(self.status.stop)
        self.commands = patch.object(serve.subprocess, "run", side_effect=self.command)
        self.run = self.commands.start()
        self.addCleanup(self.commands.stop)

    def command(self, args, **kwargs):
        if args[-1] == "off":
            del self.config["TCP"]["5432"]
        else:
            self.config.setdefault("TCP", {})["5432"] = {"TCPForward": "127.0.0.1:5432"}

    def invoke(self, action, acknowledge=True, revision=None, owner="fixture"):
        request = dict(port=5432, owner=owner, action=action, acknowledge_exposure=acknowledge,
                       expected_revision=revision or serve.digest(self.config))
        output = io.StringIO()
        with patch.object(serve.sys, "stdin", io.StringIO(json.dumps(request))), patch.object(serve.sys, "stdout", output):
            serve.main()
        return json.loads(output.getvalue())

    def test_apply_inspect_remove_preserves_unrelated_rules(self):
        self.config = {"TCP": {"8443": {"TCPForward": "127.0.0.1:8443"}}}
        self.assertFalse(self.invoke("preview")["owned"])
        self.assertTrue(self.invoke("apply")["owned"])
        self.assertTrue(self.invoke("inspect")["owned"])
        self.assertFalse(self.invoke("remove")["configured"])
        self.assertEqual(self.config["TCP"], {"8443": {"TCPForward": "127.0.0.1:8443"}})

    def test_confirmation_and_revision_required(self):
        with self.assertRaisesRegex(ValueError, "acknowledgement"):
            self.invoke("apply", acknowledge=False)
        with self.assertRaisesRegex(ValueError, "changed"):
            self.invoke("apply", revision="stale")
        self.run.assert_not_called()

    def test_existing_changed_and_other_instance_rules_are_not_owned(self):
        self.config = {"TCP": {"5432": {"TCPForward": "127.0.0.1:5432"}}}
        with self.assertRaisesRegex(ValueError, "overwrite"):
            self.invoke("apply")
        self.config = {}
        self.invoke("apply")
        with self.assertRaisesRegex(ValueError, "ownership"):
            self.invoke("remove", owner="another-instance")
        self.config["TCP"]["5432"]["TCPForward"] = "127.0.0.1:15432"
        with self.assertRaisesRegex(ValueError, "ownership"):
            self.invoke("remove")

    def test_interrupted_apply_is_not_adopted(self):
        self.run.side_effect = TimeoutError()
        with self.assertRaises(TimeoutError):
            self.invoke("apply")
        self.assertIn("Interrupted", self.invoke("inspect")["message"])
        with self.assertRaisesRegex(ValueError, "journal"):
            self.invoke("apply")


if __name__ == "__main__":
    unittest.main()
