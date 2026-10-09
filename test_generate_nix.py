import importlib.machinery
import importlib.util
import copy
import json
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest import mock


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
        self.proxy_upstreams = {"cache.example": {"policy": "nix-cache"}, "forge.example": {"policy": "git"}}
        (self.root / "proxy.json").write_text(json.dumps(self.proxy_upstreams))
        (self.root / "public-ca.crt").write_text("public certificate")
        (self.root / "private-ca.key").write_text("DO NOT EXPORT")
        self.compose = {"services": {
            "git": {"build": {"context": ".", "dockerfile": "read-only-git/Dockerfile"}, "command": ["--host-git-listener-uds", "/run/changed.sock_9003", "--host-upstreams-config", "/registry.json"], "volumes": [{"type": "bind", "source": "git.json", "target": "/registry.json"}]},
            "proxy": {"build": {"context": ".", "dockerfile": "connect-proxy/Dockerfile"}, "command": ["--host-proxy-listener-uds", "/run/changed.sock_9004", "--host-ca-certificate", "/root.crt", "--host-upstreams-config", "/registry.json"], "volumes": [{"type": "bind", "source": "public-ca.crt", "target": "/root.crt"}, {"type": "bind", "source": "private-ca.key", "target": "/ca.key"}, {"type": "bind", "source": "proxy.json", "target": "/registry.json"}]},
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
        self.assertEqual(deployment["proxy"]["nixSubstituters"], ["https://cache.example"])
        self.assertEqual((self.output / "connect-proxy/ca.crt").read_text(), "public certificate")
        all_content = b"".join(p.read_bytes() for p in self.output.rglob("*") if p.is_file())
        self.assertNotIn(b"DO NOT EXPORT", all_content)
        self.assertNotIn(str(self.root).encode(), all_content)
        self.assertIn(b"./deployment.json", all_content)
        self.upstreams["another"] = "https://another.example/"
        (self.root / "git.json").write_text(json.dumps(self.upstreams))
        generator.export(self.compose, self.output)
        self.assertEqual(json.loads((self.output / "deployment.json").read_text())["git"]["upstreams"], self.upstreams)

    @unittest.skipUnless(generator.shutil.which("docker-compose") or generator.shutil.which("docker"), "Compose CLI unavailable")
    def test_guest_generation_needs_no_host_environment_daemon_or_secret_files(self):
        if not generator.shutil.which("docker-compose") and generator.subprocess.call(
            ["docker", "compose", "version"], stdout=generator.subprocess.DEVNULL, stderr=generator.subprocess.DEVNULL
        ) != 0:
            self.skipTest("Docker has no Compose plugin")
        (self.root / "compose.yaml").write_bytes((SOURCE / "compose.yaml").read_bytes())
        (self.root / "read-only-git.json").write_text(json.dumps(self.upstreams))
        (self.root / "connect-proxy.json").write_bytes((SOURCE / "connect-proxy.json").read_bytes())
        (self.root / "connect-proxy/ca.crt").write_text("public certificate")
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("FIRECRACKER_", "HOST_BRIDGE_", "GATEWAY_"))}
        environment["DOCKER_HOST"] = "unix:///missing-docker-daemon.sock"
        with mock.patch.dict(os.environ, environment, clear=True):
            compose = generator.compose_config()
            for volume in compose["services"]["connect-proxy"]["volumes"]:
                if volume["target"] == "/ca.crt":
                    volume["source"] = "connect-proxy/ca.crt"
            generator.export(compose, self.output)
        deployment = json.loads((self.output / "deployment.json").read_text())
        self.assertEqual(deployment["read-only-git"]["upstreams"], self.upstreams)
        self.assertNotIn("ssh-bridge", deployment)
        self.assertNotIn("tame-rpc-bridge", deployment)
        self.assertEqual(deployment["connect-proxy"]["vsockPort"], 5004)
        self.assertEqual(deployment["inference"]["vsockPort"], 5001)
        self.assertEqual(deployment["connect-proxy"]["nixSubstituters"], ["https://cache.nixos.org"])
        self.assertNotIn("nix-cache", deployment)
        self.assertEqual((self.output / "connect-proxy/ca.crt").read_text(), "public certificate")
        self.assertFalse((self.root / ".env").exists())
        self.assertFalse((self.root / "connect-proxy/ca.key").exists())
        self.assertFalse((self.root / "inference.json").exists())

    def test_missing_public_ca_does_not_write_half_an_export(self):
        (self.root / "public-ca.crt").unlink()
        with self.assertRaises(OSError):
            generator.export(self.compose, self.output)
        self.assertFalse(self.output.exists())

    def test_native_vsock_bridge_removes_the_previous_adapter_import(self):
        generator.export(self.compose, self.output)
        self.assertIn('deployment."ssh"', (self.output / "guest.nix").read_text())
        del self.compose["services"]["ssh"]["labels"]
        for labels in [None, {}]:
            if labels is not None:
                self.compose["services"]["ssh"]["labels"] = labels
            generator.export(self.compose, self.output)
            self.assertNotIn("ssh", json.loads((self.output / "deployment.json").read_text()))
            self.assertNotIn('deployment."ssh"', (self.output / "guest.nix").read_text())

    def test_git_redirect_bases_stay_on_the_host(self):
        self.upstreams["tangled"] = {"url": "https://tangled.org/", "redirect_bases": ["https://knot1.tangled.sh/"]}
        (self.root / "git.json").write_text(json.dumps(self.upstreams))
        generator.export(self.compose, self.output)
        deployment = json.loads((self.output / "deployment.json").read_text())
        self.assertEqual(deployment["git"]["upstreams"]["tangled"], "https://tangled.org/")
        self.assertNotIn("knot1.tangled.sh", json.dumps(deployment))

    def test_packaged_script_reads_the_requested_checkout(self):
        packaged = self.root / "store-bin"
        packaged.mkdir()
        script = packaged / "generate-nix"
        script.write_bytes((SOURCE / "generate-nix").read_bytes())
        compose = self.root / "compose.json"
        compose.write_text(json.dumps(self.compose))
        subprocess.run([sys.executable, str(script), "--checkout", str(self.root), str(self.output), "--compose-json", str(compose)], cwd=packaged, check=True, capture_output=True)
        self.assertEqual(json.loads((self.output / "deployment.json").read_text())["git"]["upstreams"], self.upstreams)
        self.assertEqual((self.output / "connect-proxy/ca.crt").read_text(), "public certificate")

    def test_source_directories_and_duplicate_adapter_ports_are_rejected(self):
        for target in [self.root, self.root / "read-only-git", self.root / "connect-proxy/subdir", self.root / "host-runtime/subdir"]:
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

    def test_only_nix_cache_policies_configure_substituters_and_track_registry(self):
        self.proxy_upstreams["another-cache.example"] = {"policy": "nix-cache"}
        (self.root / "proxy.json").write_text(json.dumps(self.proxy_upstreams))
        generator.export(self.compose, self.output)
        deployment = json.loads((self.output / "deployment.json").read_text())
        self.assertEqual(deployment["proxy"]["nixSubstituters"], ["https://another-cache.example", "https://cache.example"])
        (self.root / "proxy.json").write_text(json.dumps({"forge.example": {"policy": "git"}}))
        generator.export(self.compose, self.output)
        self.assertEqual(json.loads((self.output / "deployment.json").read_text())["proxy"]["nixSubstituters"], [])

    def test_second_connect_proxy_cannot_replace_exported_trust(self):
        generator.export(self.compose, self.output)
        before = {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob("*") if p.is_file()}
        (self.root / "other-ca.crt").write_text("another public CA")
        proxy = copy.deepcopy(self.compose["services"]["proxy"])
        proxy["command"][1] = "/run/changed.sock_9006"
        proxy["volumes"][0]["source"] = "other-ca.crt"
        self.compose["services"]["other-proxy"] = proxy
        with self.assertRaisesRegex(ValueError, "only one CONNECT proxy"):
            generator.export(self.compose, self.output)
        self.assertEqual(before, {p.relative_to(self.output): p.read_bytes() for p in self.output.rglob("*") if p.is_file()})


if __name__ == "__main__":
    unittest.main()
