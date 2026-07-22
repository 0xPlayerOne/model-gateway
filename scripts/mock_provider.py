#!/usr/bin/env python3
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/v1/models":
            self.send_error(404)
            return
        self.send_json({"object": "list", "data": [{"id": "upstream-smoke"}]})

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(length))
        self.record_request(request)
        required_key = os.environ.get("MOCK_PROVIDER_API_KEY")
        if required_key and self.headers.get("Authorization") != f"Bearer {required_key}":
            self.send_json({"error": {"message": "unauthorized"}}, status=401)
            return
        content = "smoke-ok" if request.get("tools") else "missing-tools"
        if request.get("stream"):
            chunks = [
                {
                    "id": "chatcmpl-smoke",
                    "object": "chat.completion.chunk",
                    "model": request["model"],
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"role": "assistant", "content": content},
                            "finish_reason": None,
                        }
                    ],
                },
                {
                    "id": "chatcmpl-smoke",
                    "object": "chat.completion.chunk",
                    "model": request["model"],
                    "choices": [
                        {"index": 0, "delta": {}, "finish_reason": "stop"}
                    ],
                },
            ]
            body = "".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks)
            body += "data: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(body.encode())
            return
        self.send_json(
            {
                "id": "chatcmpl-smoke",
                "object": "chat.completion",
                "model": request["model"],
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": content},
                        "finish_reason": "stop",
                    }
                ],
            }
        )

    def record_request(self, request):
        path = os.environ.get("MOCK_PROVIDER_LOG")
        if not path:
            return
        with open(path, "a", encoding="utf-8") as log:
            log.write(
                json.dumps(
                    {
                        "model": request.get("model"),
                        "stream": bool(request.get("stream")),
                        "tools": bool(request.get("tools")),
                    }
                )
                + "\n"
            )

    def send_json(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    host = os.environ.get("MOCK_PROVIDER_HOST", "127.0.0.1")
    ThreadingHTTPServer((host, int(sys.argv[1])), Handler).serve_forever()
