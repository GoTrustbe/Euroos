#!/usr/bin/env python3
"""Minimal eSCL / AirScan scanner for the Metal M7-2 end-to-end test.

Speaks just enough of the eSCL REST/XML protocol to let EuroOS discover the
scanner and pull a page:
  GET  /eSCL/ScannerCapabilities -> XML capabilities
  POST /eSCL/ScanJobs            -> 201 Created + Location: <job>
  GET  <job>/NextDocument        -> the scanned image bytes (a tiny JPEG)

Runs on a high port (no privilege needed), unlike the IPP :631 case. Usage:
  python3 scripts/mock-escl-server.py [--port 8631]
"""
import argparse
import socketserver
import sys

# A 1x1 JPEG (smallest valid baseline JPEG) as the "scanned page".
SCAN_JPEG = bytes([
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01,
    0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43,
    0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09,
    0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12,
    0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
    0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29,
    0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32,
    0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xD9,
])

CAPS_XML = (
    '<?xml version="1.0" encoding="UTF-8"?>'
    '<scan:ScannerCapabilities xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" '
    'xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">'
    '<pwg:Version>2.6</pwg:Version>'
    '<pwg:MakeAndModel>EuroScan Virtual 3000</pwg:MakeAndModel>'
    '<scan:Platen><scan:PlatenInputCaps><scan:SettingProfiles><scan:SettingProfile>'
    '<pwg:DocumentFormat>image/jpeg</pwg:DocumentFormat>'
    '<scan:DocumentFormatExt>application/pdf</scan:DocumentFormatExt>'
    '<scan:ColorModes><scan:ColorMode>RGB24</scan:ColorMode>'
    '<scan:ColorMode>Grayscale8</scan:ColorMode></scan:ColorModes>'
    '<scan:SupportedResolutions><scan:DiscreteResolutions>'
    '<scan:DiscreteResolution><scan:XResolution>300</scan:XResolution>'
    '<scan:YResolution>300</scan:YResolution></scan:DiscreteResolution>'
    '<scan:DiscreteResolution><scan:XResolution>600</scan:XResolution>'
    '<scan:YResolution>600</scan:YResolution></scan:DiscreteResolution>'
    '</scan:DiscreteResolutions></scan:SupportedResolutions>'
    '</scan:SettingProfile></scan:SettingProfiles></scan:PlatenInputCaps></scan:Platen>'
    '</scan:ScannerCapabilities>'
).encode()

JOB = ["9d2a1f00"]
HOST_PORT = [8631]


class Handler(socketserver.StreamRequestHandler):
    timeout = 30

    def _read_request(self):
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = self.connection.recv(4096)
            if not chunk:
                return None, b""
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        clen = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                clen = int(line.split(b":", 1)[1].strip())
        body = rest
        while len(body) < clen:
            c = self.connection.recv(min(65536, clen - len(body)))
            if not c:
                break
            body += c
        return head.decode(errors="replace"), body

    def _send(self, status, headers, body=b""):
        h = f"HTTP/1.1 {status}\r\n"
        for k, v in headers.items():
            h += f"{k}: {v}\r\n"
        h += f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n"
        self.wfile.write(h.encode() + body)

    def handle(self):
        head, body = self._read_request()
        if head is None:
            return
        line = head.split("\r\n", 1)[0]
        parts = line.split()
        if len(parts) < 2:
            self._send("400 Bad Request", {})
            return
        method, path = parts[0], parts[1]
        print(f"[escl] {method} {path}", flush=True)

        if method == "GET" and path.endswith("/ScannerCapabilities"):
            self._send("200 OK", {"Content-Type": "text/xml"}, CAPS_XML)
        elif method == "POST" and path.endswith("/ScanJobs"):
            job = JOB[0]
            loc = f"http://10.0.2.2:{HOST_PORT[0]}/eSCL/ScanJobs/{job}"
            print(f"[escl]   ScanSettings ({len(body)} bytes) -> job {job}", flush=True)
            self._send("201 Created", {"Location": loc})
        elif method == "GET" and path.endswith("/NextDocument"):
            print(f"[escl]   NextDocument -> {len(SCAN_JPEG)} bytes JPEG", flush=True)
            self._send("200 OK", {"Content-Type": "image/jpeg"}, SCAN_JPEG)
        else:
            self._send("404 Not Found", {})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8631)
    a = ap.parse_args()
    HOST_PORT[0] = a.port
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.ThreadingTCPServer(("127.0.0.1", a.port), Handler) as srv:
        print(f"[escl] mock eSCL scanner on 127.0.0.1:{a.port}", flush=True)
        srv.serve_forever()


if __name__ == "__main__":
    main()
