import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import time
import unittest
from http_runner import JsonClient, throughput


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class RunnerTests(unittest.TestCase):
    def test_connection_reuse(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        client = JsonClient("http://127.0.0.1:" + str(server.server_port) + "/search")
        try:
            self.assertEqual(client.post({"q": 1}), {"q": 1})
            socket = client.connection.sock
            self.assertEqual(client.post({"q": 2}), {"q": 2})
            self.assertIs(client.connection.sock, socket)
        finally:
            client.close()
            server.shutdown()
            server.server_close()
            thread.join()

    def test_workers_are_really_concurrent_and_errors_counted(self):
        lock = threading.Lock()
        active = [0, 0]
        class Client:
            def close(self):
                pass
        def operation(client, query):
            with lock:
                active[0] += 1
                active[1] = max(active)
            time.sleep(.01)
            with lock:
                active[0] -= 1
            raise ValueError("injected")
        row = throughput(Client, ["q"], operation, workers=8, seconds=.05)
        self.assertEqual(active[1], 8)
        self.assertEqual(row["requests"], 0)
        self.assertGreaterEqual(row["errors"], 8)
        self.assertGreaterEqual(row["driver_cpu_seconds"], 0)


if __name__ == "__main__":
    unittest.main()
