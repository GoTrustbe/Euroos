# Remote desktop (RDP) on EuroOS

Sprint N4: the one legacy Windows application a workplace still needs runs
on a Windows host; EuroOS shows it in a window. The client is written from
scratch: `crates/eurordp` is the protocol (host-tested, `no_std`, no unsafe
code), `kernel/src/rdpclient.rs` the transport and the window.

## Use

In the Terminal:

```
rdp <host>[:port] <user> [password]
rdp                      # status of the open session, or the usage line
```

The remote desktop opens in a hosted window ("Remote desktop - host"). Keys
and the mouse go to the remote side while that window has focus; Ctrl+Alt+Esc
or the window's close button ends the session.

## What it does (v1)

- X.224 with the RDP negotiation (TLS requested), then EuroTLS 1.3 on the
  same connection, then MCS/GCC (one I/O channel, no virtual channels), the
  Client Info PDU with the credentials, Demand/Confirm Active with 16
  capability sets, the finalization PDUs, and then the session.
- No drawing orders are announced, so the server sends bitmap updates. The
  bitmaps come back interleaved-RLE compressed (xrdp compresses by default and
  a client cannot turn that off); EuroOS decompresses them (crates/eurordp
  rle.rs, 15/16/24/32 bpp) and converts to the window's pixels. Input:
  scancodes, unicode, pointer motion, buttons, wheel.
**Live result (2026-10-07):** against a real xrdp server (docker
`danielguerra/ubuntu-xrdp`, reached from the QEMU guest as `10.0.2.2` over
user-mode networking), `rdp test` establishes the TCP connection, negotiates
TLS in the X.224 exchange, and completes an **EuroTLS 1.3 handshake with the
live server** ("[rdp] TLS 1.3 up with 10.0.2.2"); the RDP connection
sequence then reaches the **licensing** stage. This xrdp build requires a
licensing exchange, which v1 does not implement, so the session ends there:
the remote desktop does not paint yet. Proof:
`docs/proof/2026-10-07-rdp-tls-handshake-live.png` (the client window
connected). The host tests cover the rest of the sequence (capabilities,
bitmap updates, input) against synthesized server PDUs.

## Limits, stated

- TLS 1.3 only: EuroTLS has no 1.2 client yet. xrdp with a current OpenSSL
  negotiates 1.3; Windows 10/Server 2019 RDP offers 1.2 only, Windows 11 and
  Server 2022 offer 1.3. The 1.2 client is the next rung.
- No NLA/CredSSP: a server that requires network-level authentication (the
  Windows default) refuses the connection with "hybrid required". Disable
  NLA on the host for v1 ("Allow connections from computers running any
  version of Remote Desktop"); CredSSP (NTLM/Kerberos over TLS) is planned.
- Licensing (MS-RDPELE) is implemented: new-license request (RSA-encrypted
  pre-master secret), platform challenge / response (RC4, MD5/SHA-1 MAC),
  new-license or STATUS_VALID_CLIENT. The server key is taken from either a
  PROPRIETARY certificate (xrdp) or an X.509 chain (Windows: the leaf's RSA
  key via eurotls x509). The chain's signatures are not verified (the
  session is already inside TLS). The licence is not stored; every session
  asks for a new one, which servers grant for the grace period. Host-tested
  (27 tests, incl. an openssl-made RSA certificate round trip and the RLE
  decoder). **Live proof (2026-10-07): the real XFCE desktop of an Ubuntu
  xrdp server renders in an EuroOS window** over TCP + X.224 + EuroTLS 1.3 +
  MCS + licensing + RLE bitmaps (docs/proof/2026-10-07-rdp-remote-desktop-paints.png).
- The server certificate is not validated (RDP servers are self-signed);
  trust-on-first-use with a fingerprint prompt comes with the settings UI.
- No clipboard, no drive or audio redirection; no fast-path input/output
  (slow-path only); no drawing orders (bitmap updates only).
