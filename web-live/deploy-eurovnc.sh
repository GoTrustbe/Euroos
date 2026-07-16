#!/usr/bin/env bash
# Deploy the EuroOS "try it live in your browser" service on euro-os.eu.
#
#   sudo bash /home/user/eurokernel/web-live/deploy-eurovnc.sh
#
# Installs noVNC + websockify, a dedicated service user, the orchestrator, the
# base VM image, two systemd services, the /live/ page, and the nginx routing.
# Idempotent: safe to re-run (e.g. after rebuilding the download image).
set -euo pipefail

SRC=/home/user/eurokernel/web-live
WEBROOT=/var/www/euro-os.eu
IMG_GZ="$WEBROOT/download/euroos-x86_64-uefi.img.gz"
TS=$(date -u +%Y%m%d-%H%M%S)

[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

echo "==> 1/9 packages (novnc, websockify, ovmf)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq novnc websockify ovmf qemu-system-x86 >/dev/null
command -v websockify >/dev/null || { echo "websockify missing after install"; exit 1; }
[ -f /usr/share/novnc/vnc.html ] || { echo "noVNC not at /usr/share/novnc"; exit 1; }

echo "==> 2/9 service user 'eurovnc'"
id -u eurovnc >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin eurovnc

echo "==> 3/9 directories + orchestrator"
install -d -o eurovnc -g eurovnc -m 0755 /opt/eurovnc
install -d -o eurovnc -g eurovnc -m 0750 /var/lib/eurovnc
install -o root -g root -m 0644 "$SRC/orchestrator.py" /opt/eurovnc/orchestrator.py
# /run/eurovnc (tmpfs) is created every boot via tmpfiles.d
cat > /etc/tmpfiles.d/eurovnc.conf <<'EOF'
d /run/eurovnc          0750 eurovnc eurovnc -
d /run/eurovnc/tokens   0750 eurovnc eurovnc -
d /run/eurovnc/sessions 0750 eurovnc eurovnc -
EOF
systemd-tmpfiles --create /etc/tmpfiles.d/eurovnc.conf

echo "==> 4/9 base VM image (from the published download image)"
[ -f "$IMG_GZ" ] || { echo "missing $IMG_GZ (publish the download first)"; exit 1; }
gunzip -c "$IMG_GZ" > /opt/eurovnc/base.img.new
mv -f /opt/eurovnc/base.img.new /opt/eurovnc/base.img
chown eurovnc:eurovnc /opt/eurovnc/base.img
chmod 0644 /opt/eurovnc/base.img
echo "    base.img: $(du -h /opt/eurovnc/base.img | cut -f1)"

echo "==> 5/9 systemd services"
install -m 0644 "$SRC/eurovnc-orchestrator.service" /etc/systemd/system/eurovnc-orchestrator.service
install -m 0644 "$SRC/eurovnc-websockify.service"  /etc/systemd/system/eurovnc-websockify.service
systemctl daemon-reload
systemctl enable --now eurovnc-websockify.service
systemctl enable --now eurovnc-orchestrator.service
systemctl restart eurovnc-websockify.service eurovnc-orchestrator.service
sleep 2
for s in eurovnc-websockify eurovnc-orchestrator; do
  systemctl is-active --quiet "$s" && echo "    $s: active" || { echo "    $s FAILED:"; systemctl --no-pager -l status "$s" | tail -20; exit 1; }
done

echo "==> 6/9 orchestrator health"
curl -fsS --max-time 5 http://127.0.0.1:6070/api/health && echo || { echo "health check failed"; exit 1; }

echo "==> 7/9 /live/ page"
install -d -o www-data -g www-data -m 0755 "$WEBROOT/live"
[ -f "$WEBROOT/live/index.html" ] && cp -a "$WEBROOT/live/index.html" "$WEBROOT/live/index.html.bak.$TS" || true
install -o www-data -g www-data -m 0644 "$SRC/live-index.html" "$WEBROOT/live/index.html"

echo "==> 7b/9 discoverability: 'Try it live' CTA on the /try/ page"
TRYPAGE="$WEBROOT/try/index.html"
if [ -f "$TRYPAGE" ] && ! grep -q 'href="/live/"' "$TRYPAGE"; then
  cp -a "$TRYPAGE" "$TRYPAGE.bak.$TS"
  python3 - "$TRYPAGE" <<'PY'
import sys
p = sys.argv[1]
lines = open(p).read().splitlines(keepends=True)
cta = ('  <p style="margin-top:18px"><a class="btn" href="/live/">'
       '▶ Try it live in your browser — no install</a>'
       '<span style="font-size:13px;color:var(--ink-faint);margin-left:10px">'
       'boots a private session for 30 minutes</span></p>\n')
out, done = [], False
for ln in lines:
    out.append(ln)
    if not done and 'not yet a daily-driver OS.</p>' in ln:
        out.append(cta); done = True
open(p, 'w').write(''.join(out))
print('    CTA inserted' if done else '    WARNING: anchor not found; CTA NOT added')
PY
  chown www-data:www-data "$TRYPAGE"
else
  echo "    CTA already present (or /try/ missing)"
fi

echo "==> 8/9 nginx config (conf.d map + zone, server snippet, include)"
install -m 0644 "$SRC/nginx-confd-eurovnc.conf"   /etc/nginx/conf.d/eurovnc.conf
install -d -m 0755 /etc/nginx/snippets
install -m 0644 "$SRC/nginx-snippet-eurovnc.conf" /etc/nginx/snippets/eurovnc.conf
SITE=/etc/nginx/sites-available/euro-os.eu
if ! grep -q 'snippets/eurovnc.conf' "$SITE"; then
  cp -a "$SITE" "$SITE.bak.$TS"
  # Insert the include just after 'root /var/www/euro-os.eu;' (inside the :443 server block).
  python3 - "$SITE" <<'PY'
import sys
p = sys.argv[1]
lines = open(p).read().splitlines(keepends=True)
out, done = [], False
for ln in lines:
    out.append(ln)
    if not done and ln.strip() == 'root /var/www/euro-os.eu;':
        out.append('\n    # EuroOS live-try (VNC-in-browser) routes\n')
        out.append('    include /etc/nginx/snippets/eurovnc.conf;\n')
        done = True
open(p, 'w').write(''.join(out))
print('    include inserted' if done else '    WARNING: anchor not found; include NOT added')
PY
else
  echo "    include already present"
fi

echo "==> 9/9 validate + reload nginx"
nginx -t
systemctl reload nginx

echo
echo "==> DONE. Live at https://euro-os.eu/live/"
echo "    signups log: /var/lib/eurovnc/signups.log"
echo "    sessions:    systemctl status eurovnc-orchestrator ; curl -s 127.0.0.1:6070/api/status"
