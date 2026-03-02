#!/usr/bin/env python3
"""Dev server for stet WASM frontend.

Serves static files with CORS and SharedArrayBuffer headers.
Run from the web/ directory: python3 serve.py
"""

import sys
from http.server import HTTPServer, SimpleHTTPRequestHandler


class Handler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        self.send_header('Cache-Control', 'no-store, must-revalidate')
        self.send_header('Pragma', 'no-cache')
        self.send_header('Expires', '0')
        super().end_headers()

    def log_message(self, format, *args):
        # Quieter logging — skip .wasm and .js fetches
        msg = format % args
        if '.wasm' not in msg and '.js' not in msg:
            sys.stderr.write(f'{self.address_string()} - {msg}\n')


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
server = HTTPServer(('localhost', port), Handler)
print(f'Serving at http://localhost:{port}')
server.serve_forever()
