# EuroOS "Try it live in your browser"

Give a visitor a private, throw-away EuroOS desktop over the browser for 30
minutes. They enter an e-mail, we boot a sealed QEMU VM and stream its screen
via noVNC; after 30 minutes (or when they leave) the VM is killed and its disk
overlay discarded. The e-mail is logged so we can see who is trying EuroOS.

## Architecture

```
browser ──POST /api/launch {email}──► nginx ──► orchestrator.py (127.0.0.1:6070)
                                                   │  validate e-mail, enforce
                                                   │  caps + per-IP cooldown,
                                                   │  spawn QEMU (qcow2 overlay,
                                                   │  NO guest network, seccomp),
                                                   │  write token file, log e-mail,
                                                   │  arm 30-min reaper
        ◄──────── { url: /vnc/<token> } ──────────┘

browser opens /vnc/<token>
   └─► nginx 302 ─► /vnc/vnc.html?…path=vnc/websockify?token=<token>
          └─► noVNC (served by websockify) opens wss://…/vnc/websockify?token=<token>
                 └─► nginx ─► websockify (127.0.0.1:6080, TokenFile plugin)
                        └─► looks up token ─► 127.0.0.1:<vnc port> ─► QEMU VNC
```

Everything binds to `127.0.0.1`; nginx (existing certbot TLS) is the only public
front. No third-party services. Python orchestrator is standard-library only.

## Files

| file | installed to | purpose |
|------|--------------|---------|
| `orchestrator.py` | `/opt/eurovnc/orchestrator.py` | session manager + reaper + e-mail log |
| `eurovnc-orchestrator.service` | `/etc/systemd/system/` | runs the orchestrator as user `eurovnc` |
| `eurovnc-websockify.service` | `/etc/systemd/system/` | noVNC token proxy (`--web /usr/share/novnc`) |
| `live-index.html` | `/var/www/euro-os.eu/live/index.html` | the e-mail form / landing page |
| `nginx-confd-eurovnc.conf` | `/etc/nginx/conf.d/eurovnc.conf` | `map $http_upgrade` + launch rate-limit zone |
| `nginx-snippet-eurovnc.conf` | `/etc/nginx/snippets/eurovnc.conf` | `/api/launch`, `/vnc/<token>`, `/vnc/` routes |
| `deploy-eurovnc.sh` | — | one-shot installer (idempotent) |

## Deploy / update

```
sudo bash /home/user/eurokernel/web-live/deploy-eurovnc.sh
```

Re-run any time (e.g. after rebuilding the download image) — it re-copies the
base VM image from `/var/www/euro-os.eu/download/euroos-x86_64-uefi.img.gz`,
reinstalls units/config idempotently, and reloads. The base VM image is the
*same* image visitors can download, so "try live" == "what you'd run locally".

## Tunables (env in `eurovnc-orchestrator.service`)

| var | default | meaning |
|-----|---------|---------|
| `EUROVNC_MAX_SESSIONS` | 3 | global concurrent VMs (host has 4 cores / no KVM) |
| `EUROVNC_TTL` | 1800 | session lifetime in seconds (30 min) |
| `EUROVNC_IP_COOLDOWN` | 120 | seconds between launches per IP (1 active/IP always) |
| `EUROVNC_DISPLAY_MIN/MAX` | 1 / 9 | VNC display range (TCP 5901–5909) |

No KVM on this host → each VM runs under TCG (software emulation, ~1 core each).
Keep `MAX_SESSIONS` at or below `cores − 1`.

## Operations

```
systemctl status eurovnc-orchestrator eurovnc-websockify
curl -s 127.0.0.1:6070/api/status            # active sessions + remaining time
cat  /var/lib/eurovnc/signups.log            # who tried: ts <tab> ip <tab> email <tab> token <tab> UA
journalctl -u eurovnc-orchestrator -f        # launches / reaps / errors
```

Sessions and their qcow2 overlays live in `/run/eurovnc/` (tmpfs) and are wiped
on reap and on service restart. Guest VMs have **no network path out**.
