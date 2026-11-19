import io
import json
import unittest
from email.message import Message
from kageha_client import AgentClient, AgentError
from kageha_client.agent import LIMIT

TOKEN = "python-agent-sdk-operator-test-token"
RECORD = {"spec": {"id": "run", "objective": "héllo"}, "state": "running"}
def frame(sequence=4, event_id=None, state="running"):
    record = {**RECORD, "state": state}
    return ("id: %s\r\nevent: run\r\ndata: %s\r\n\r\n" % (sequence if event_id is None else event_id, json.dumps({"sequence": sequence, "record": record}, ensure_ascii=False))).encode()

class Response(io.BytesIO):
    def __init__(self, data, status=200, content_type="text/event-stream"):
        super().__init__(data)
        self.status = status
        self.headers = Message()
        self.headers["Content-Type"] = content_type
    def read1(self, count):
        return self.read(min(count, 1))

class AgentTests(unittest.TestCase):
    def client(self, response):
        client = AgentClient("http://localhost:1234", TOKEN)
        client._open = lambda *args, **kwargs: response
        return client

    def test_endpoint_and_scope_validation(self):
        for endpoint in ["http://remote.example", "https://user:pass@example.com", "https://example.com?q=1", "https://example.com#secret"]:
            with self.assertRaises(AgentError): AgentClient(endpoint, TOKEN)
        with self.assertRaises(AgentError): AgentClient("http://localhost", "short")
        with self.assertRaises(AgentError): AgentClient("http://localhost", TOKEN).status("..")
        with self.assertRaises(AgentError): AgentClient("http://localhost", TOKEN).events("run", after=-1)

    def test_fragmentation_unicode_and_terminal_event(self):
        response = Response(b": keepalive\r\n\r\n" + frame() + frame(7, state="succeeded"))
        with self.client(response).events("run") as events:
            received = list(events)
            self.assertEqual([v["sequence"] for v in received], [4, 7])
            self.assertEqual(received[0]["record"]["spec"]["objective"], "héllo")
            self.assertEqual(events.last_sequence, 7)
        self.assertTrue(response.closed)

    def test_failed_or_truncated_streams_close(self):
        for data in [b"event: error\ndata: failed\n\n", frame(4, 5), frame()+frame(), frame()[:-4], b"event: run\ndata: \xff\n\n"]:
            response = Response(data)
            with self.assertRaises(AgentError):
                with self.client(response).events("run") as events: list(events)
            self.assertTrue(response.closed)

    def test_explicit_close_keeps_only_delivered_cursor(self):
        response = Response(frame() + frame(7))
        events = self.client(response).events("run")
        self.assertEqual(next(events)["sequence"], 4)
        events.close()
        self.assertEqual(events.last_sequence, 4)
        self.assertTrue(response.closed)
        self.assertEqual(list(events), [])

    def test_plain_http_error_and_bounded_json(self):
        for status in [401, 302]:
            response = Response(b"plain error", status)
            with self.assertRaises(AgentError) as raised: self.client(response).info()
            self.assertEqual(raised.exception.status, status)
            self.assertTrue(response.closed)
        response = Response(b"x"*(LIMIT+1))
        with self.assertRaises(AgentError): self.client(response).info()
        self.assertTrue(response.closed)

if __name__ == "__main__": unittest.main()
