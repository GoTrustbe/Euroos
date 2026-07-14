#!/usr/bin/env python3
"""Minimal IPP Everywhere printer for the Metal M-7 end-to-end test.

Speaks just enough IPP-over-HTTP (RFC 8010) to let EuroOS discover the printer
(Get-Printer-Attributes) and submit a job (Print-Job): parses the operation id
+ request id from the IPP body and returns a well-formed successful-ok response
with the operation/job attribute groups EuroOS's parser expects. Received job
documents are written to <spool>/ and logged, so the test can prove the page
actually arrived — not just that a status byte came back.

Usage: python3 scripts/mock-ipp-server.py [--port 6631] [--spool DIR]
"""
import argparse
import os
import socketserver
import struct
import sys
import time

STATUS_OK = 0x0000
OP_PRINT_JOB = 0x0002
OP_GET_PRINTER_ATTRIBUTES = 0x000B

TAG_OPERATION = 0x01
TAG_JOB = 0x02
TAG_PRINTER = 0x04
TAG_END = 0x03
TAG_INTEGER = 0x21
TAG_ENUM = 0x23
TAG_KEYWORD = 0x44
TAG_NAME = 0x42
TAG_URI = 0x45
TAG_CHARSET = 0x47
TAG_LANGUAGE = 0x48
TAG_MIMETYPE = 0x49

SPOOL = "/tmp/euro-ipp-spool"
JOB_ID = [1]


def attr(tag, name, value):
    nb = name.encode()
    vb = value if isinstance(value, bytes) else value.encode()
    return bytes([tag]) + struct.pack(">H", len(nb)) + nb + struct.pack(">H", len(vb)) + vb


def ipp_response(operation, request_id, doc_len):
    """Build a successful-ok IPP response for the given operation."""
    body = bytes([2, 0]) + struct.pack(">H", STATUS_OK) + struct.pack(">I", request_id)
    body += bytes([TAG_OPERATION])
    body += attr(TAG_CHARSET, "attributes-charset", "utf-8")
    body += attr(TAG_LANGUAGE, "attributes-natural-language", "en")
    body += attr(TAG_NAME, "status-message", "successful-ok")
    if operation == OP_GET_PRINTER_ATTRIBUTES:
        body += bytes([TAG_PRINTER])
        body += attr(TAG_URI, "printer-uri-supported", "ipp://10.0.2.2:631/ipp/print")
        body += attr(TAG_KEYWORD, "printer-state-reasons", "none")
        body += attr(TAG_ENUM, "printer-state", struct.pack(">I", 3))  # idle
        body += attr(TAG_NAME, "printer-make-and-model", "EuroOS Virtual IPP")
        body += attr(TAG_KEYWORD, "document-format-supported", "text/plain")
        body += attr(TAG_KEYWORD, "ipp-versions-supported", "2.0")
    elif operation == OP_PRINT_JOB:
        body += bytes([TAG_JOB])
        body += attr(TAG_INTEGER, "job-id", struct.pack(">I", JOB_ID[0]))
        body += attr(TAG_ENUM, "job-state", struct.pack(">I", 9))  # completed
        body += attr(TAG_URI, "job-uri", f"ipp://10.0.2.2:631/jobs/{JOB_ID[0]}")
    body += bytes([TAG_END])
    return body


class Handler(socketserver.StreamRequestHandler):
    timeout = 30

    def handle(self):
        data = b""
        # Read the HTTP request headers.
        while b"\r\n\r\n" not in data:
            chunk = self.rfile.read1(4096) if hasattr(self.rfile, "read1") else self.connection.recv(4096)
            if not chunk:
                return
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        clen = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                clen = int(line.split(b":", 1)[1].strip())
        body = rest
        while len(body) < clen:
            chunk = self.connection.recv(min(65536, clen - len(body)))
            if not chunk:
                break
            body += chunk

        if len(body) < 8:
            self.wfile.write(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            return
        operation = struct.unpack(">H", body[2:4])[0]
        request_id = struct.unpack(">I", body[4:8])[0]

        # The document (if any) follows the IPP attributes (after the end tag 0x03).
        doc = b""
        end = body.find(bytes([TAG_END]), 8)
        if operation == OP_PRINT_JOB and end != -1:
            doc = body[end + 1:]
            JOB_ID[0] += 1
            os.makedirs(SPOOL, exist_ok=True)
            fn = os.path.join(SPOOL, f"job-{JOB_ID[0]-1}.txt")
            with open(fn, "wb") as f:
                f.write(doc)
            print(f"[ipp] Print-Job req={request_id} {len(doc)} bytes -> {fn}", flush=True)
            print(f"[ipp]   document: {doc[:80]!r}", flush=True)
        else:
            opname = {OP_GET_PRINTER_ATTRIBUTES: "Get-Printer-Attributes"}.get(operation, hex(operation))
            print(f"[ipp] {opname} req={request_id}", flush=True)

        resp = ipp_response(operation, request_id, len(doc))
        http = (b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: application/ipp\r\n"
                + f"Content-Length: {len(resp)}\r\n".encode()
                + b"Connection: close\r\n\r\n" + resp)
        self.wfile.write(http)


def main():
    global SPOOL
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=6631)
    ap.add_argument("--spool", default=SPOOL)
    a = ap.parse_args()
    SPOOL = a.spool
    os.makedirs(SPOOL, exist_ok=True)
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.ThreadingTCPServer(("127.0.0.1", a.port), Handler) as srv:
        print(f"[ipp] mock IPP Everywhere printer on 127.0.0.1:{a.port} spool={SPOOL}", flush=True)
        srv.serve_forever()


if __name__ == "__main__":
    main()
