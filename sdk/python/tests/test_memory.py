"""Optional SDK conformance. The server under test is the Rust executable."""
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import unittest

from kageha_client import MemoryClient, ServiceError

class ClientTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="kageha-python-sdk-")
        root = Path(self.directory.name)
        self.token = "optional-python-client-test-operator-token"
        (root / "token").write_text(self.token)
        binary = Path(os.environ.get("KAGEHA_MEMORY_BIN", str(Path(__file__).resolve().parents[3] / "target/debug/kageha-memory")))
        self.process = subprocess.Popen([str(binary), "--state", str(root / "memory.sqlite"), "--bind", "127.0.0.1:0", "--token-file", str(root / "token")], stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, text=True)
        selector = selectors.DefaultSelector()
        selector.register(self.process.stderr, selectors.EVENT_READ)
        if not selector.select(5):
            self.process.kill()
            raise RuntimeError("Rust memory service did not start")
        line = self.process.stderr.readline()
        selector.close()
        self.endpoint = "http://" + line.split("memory listening on ", 1)[1].strip()
        self.client = MemoryClient(self.endpoint, self.token)

    def tearDown(self):
        self.process.terminate()
        self.process.wait(timeout=5)
        self.process.stderr.close()
        self.directory.cleanup()

    def test_roundtrip_and_receipts(self):
        access = {"namespace": "sdk-test", "actor": "python-client"}
        request = {"request_id": "sdk-save", "access": access, "id": "note", "kind": "note", "body": "portable client evidence", "expected_revision": 0}
        saved = self.client.call({"operation": "put", "request": request})
        self.assertEqual(self.client.call({"operation": "put", "request": request}), saved)
        self.assertEqual(self.client.call({"operation": "get", "access": access, "id": "note"}), saved)
        self.assertEqual(len(self.client.call({"operation": "search", "access": access, "query": "evidence", "limit": 10})), 1)
        self.assertIn("memory.idempotency", self.client.info()["capabilities"])
        self.client.call({"operation": "delete", "request": {"request_id": "sdk-delete", "access": access, "id": "note", "expected_revision": saved["revision"]}})
        self.assertIsNone(self.client.call({"operation": "get", "access": access, "id": "note"}))

    def test_authentication(self):
        with self.assertRaises(ServiceError) as caught:
            MemoryClient(self.endpoint, "wrong-operator-token-at-least-32-bytes").info()
        self.assertEqual(caught.exception.code, "unauthorized")

    def test_transport_validation(self):
        for endpoint in ["http://untrusted.example", "https://user:secret@example.com", "https://example.com?token=secret"]:
            with self.assertRaises(ValueError):
                MemoryClient(endpoint, self.token)

    def test_writer_fencing(self):
        first = self.client.claim_writer("python-owner", 0, "python-claim")
        self.assertEqual(self.client.claim_writer("python-owner", 0, "python-claim"), first)
        writer = self.client.with_writer(first)
        access = {"namespace": "fence-test", "actor": "client"}
        request = {"request_id": "fenced-put", "access": access, "id": "fenced", "kind": "note", "body": "confirmed"}
        writer.call({"operation": "put", "request": request})
        self.client.retire_writer(first)
        with self.assertRaises(ServiceError) as caught:
            writer.call({"operation": "put", "request": request})
        self.assertEqual(caught.exception.code, "stale_generation")
        self.assertTrue(self.client.writer_status()["retired"])

if __name__ == "__main__":
    unittest.main()
