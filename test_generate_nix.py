import importlib.machinery
import importlib.util
import copy
import json
from pathlib import Path
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parent
loader = importlib.machinery.SourceFileLoader("generate_nix", str(SOURCE / "generate-nix"))
spec = importlib.util.spec_from_loader(loader.name, loader)
generator = importlib.util.module_from_spec(spec)
loader.exec_module(generator)


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        generator.ROOT = self.root
        self.output = self.root / "export"
        (self.root / "vsock-guest.nix").write_bytes((SOURCE / "vsock-guest.nix").read_bytes())
        for name in generator.GATEWAYS:
            (self.root / name).mkdir()
            (self.root / name / "guest.nix").write_bytes((SOURCE / name / "guest.nix").read_bytes())
        self.upstreams = {"forge": "https://forge.example/group/"}
        (self.root / "git.json").write_text(json.dumps(self.upstreams))
        (self.root / "public-ca.crt").write_text("public certificate")
        (self.root / "private-ca.key").write_text("DO NOT EXPORT")
        self.compose = {"services": {
            "git": {"build": {"context": ".", "dockerfile": "read-only-git/Dockerfile"}, "command": ["--host-git-listener-uds", "/run/changed.sock_9003", "--host-upstreams-config", "/registry.json"], "volumes": [{"type": "bind", "source": "git.json", "target": "/registry.json"}]},
            "proxy": {"build": {"context": ".", "dockerfile": "connect-proxy/Dockerfile"}, "command": ["--host-proxy-listener-uds", "/run/changed.sock_9004", "--host-ca-certificate", "/root.crt"], "volumes": [{"type": "bind", "source": "public-ca.crt", "target": "/root.crt"}, {"type": "bind", "source": "private-ca.key", "target": "/ca.key"}]},
            "ssh": {"build": {"context": "./guest-bridge", "dockerfile": "Dockerfile"}, "command": ["--guest-listener-vsock-port", "2222"], "labels": {"ggez.guest-tcp-port": "22"}},
        }}

    def tearDown(self):
        generator.ROOT = SOURCE
        self.temp.cleanup()

    def test_export_tracks_compose_and_upstream_config_and_is_relocatable(self):
        generator.export(self.compose, self.output)
        deployment = json.loads((self.output / "deployment.json").read_text())
        self.assertEqual(deployment["git"]["upstreams"], self.upstreams)
        self.assertEqual(deployment["git"]["vsockPort"], 9003)
        self.assertEqual(deployment["ssh"]["guestTcpPort"], 22)
        self.assertEqual(deployment["ssh"]["vsockPort"], 2222)
        self.assertEqual((self.output / "connect-proxy/ca.crt").read_text(), "public certificate")
        all_content = b"".join(p.read_bytes() for p in self.output.rglob("*") if p.is_file())
        self.assertNotIn(b"DO NOT EXPORT", all_content)
        self.assertNotIn(str(self.root).encode(), all_content)
        self.assertIn(b"./deployment.json", all_content)
        self.upstreams["another"] = "https://another.example/"
        (self.root / "git.json").write_text(json.dumps(self.upstreams))
        generator.export(self.compose, self.output)
        self.assertEqual(json.loads((self.output / "deployment.json").read_text())["git"]["upstreams"], self.upstreams)

    def test_missing_public_ca_does_not_write_half_an_export(self):
        (self.root / "public-ca.crt").unlink()
        with self.assertRaises(OSError):
            generator.export(self.compose, self.output)
        self.assertFalse(self.output.exists())

    def test_source_directories_and_duplicate_adapter_ports_are_rejected(self):
        for target in [self.root, self.root / "read-only-git", self.root / "connect-proxy/subdir"]:
            with self.assertRaises(ValueError):
                generator.export(self.compose, target)
        self.compose["services"]["proxy"]["labels"] = {"ggez.guest-tcp-port": "9003"}
        with self.assertRaises(ValueError):
            generator.export(self.compose, self.output)

    def test_duplicate_vsock_listeners_and_reserved_port_are_rejected(self):
        self.compose["services"]["second-ssh"] = copy.deepcopy(self.compose["services"]["ssh"])
        with self.assertRaisesRegex(ValueError, "duplicate guest vsock listener"):
            generator.export(self.compose, self.output)
        del self.compose["services"]["second-ssh"]
        self.compose["services"]["ssh"]["command"][-1] = str(2**32 - 1)
        with self.assertRaisesRegex(ValueError, "invalid guest port"):
            generator.export(self.compose, self.output)


if __name__ == "__main__":
    unittest.main()
