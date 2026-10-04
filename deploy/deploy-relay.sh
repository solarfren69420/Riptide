#!/bin/sh
# Deploy the Riptide relay to the SolarFren VPS: wss://solarfren.com/riptide/ws.
#   HOST=root@your.server deploy/deploy-relay.sh
# Installs a release under /opt/riptide-relay, the riptide-relay systemd service (127.0.0.1:3020,
# own user, 128 MB cap) and an nginx snippet included next to Togas and Tea's. nginx is tested
# and reloaded (never restarted); any failure restores the backed-up site file.
set -eu
cd "$(dirname "$0")/.."
HOST=${HOST:?set HOST=user@server}
ssh="ssh -o BatchMode=yes $HOST"
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target} cargo build --release -p riptide-server
bin=${CARGO_TARGET_DIR:-target}/release/riptide-server
ts=$(date -u +%Y%m%dT%H%M%SZ)
$ssh "mkdir -p /opt/riptide-relay/releases/$ts"
scp -o BatchMode=yes -q "$bin" "$HOST:/opt/riptide-relay/releases/$ts/riptide-server"
scp -o BatchMode=yes -q deploy/riptide-relay.service "$HOST:/etc/systemd/system/riptide-relay.service"
scp -o BatchMode=yes -q deploy/nginx-riptide.conf "$HOST:/etc/nginx/snippets/riptide.conf"
$ssh sh -s "$ts" <<'REMOTE'
set -eu
ts=$1
id riptide >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin riptide
chmod 755 /opt/riptide-relay/releases/$ts/riptide-server
ln -sfn /opt/riptide-relay/releases/$ts /opt/riptide-relay/current
systemctl daemon-reload
systemctl enable riptide-relay >/dev/null 2>&1
systemctl restart riptide-relay
sleep 1
systemctl is-active riptide-relay
# Include the snippet beside Togas and Tea's (same HTTPS server block), once.
site=$(grep -l 'snippets/togas-and-tea.conf' /etc/nginx/sites-enabled/* /etc/nginx/conf.d/* 2>/dev/null | head -1)
[ -n "$site" ] || { echo "no site includes togas-and-tea.conf: add 'include snippets/riptide.conf;' by hand"; exit 1; }
site=$(readlink -f "$site")
if ! grep -q 'snippets/riptide.conf' "$site"; then
    cp "$site" "$site.bak-riptide-$ts"
    sed -i 's|^\([[:space:]]*\)include snippets/togas-and-tea.conf;|&\n\1include snippets/riptide.conf;|' "$site"
fi
if nginx -t 2>/dev/null; then
    systemctl reload nginx
else
    [ -f "$site.bak-riptide-$ts" ] && cp "$site.bak-riptide-$ts" "$site"
    nginx -t
    echo "nginx test failed: site file restored"; exit 1
fi
echo "deployed $ts"
REMOTE
echo "check: curl -si -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' https://solarfren.com/riptide/ws | head -1"
