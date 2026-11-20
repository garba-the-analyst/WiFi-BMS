#!/usr/bin/env bash
# Configure FreeRADIUS 3.x (Ubuntu/Debian/Linux Lite) for WiFi-BMS. Safe to run again.
#   sudo bash deploy/install-freeradius.sh <LOCAL_KEY> <ROUTER_IP> <RADIUS_SECRET>
# LOCAL_KEY = the LOCAL_KEY in /etc/wifi-bms/site-agent.env; RADIUS_SECRET = the secret on the router's /radius entry.
# Use letters and digits only (openssl rand -hex 16 is fine). Validated with FreeRADIUS 3.2: config check, accept/reject
# mapping with rate limit, accounting start/interim/stop, and a fail-closed reject when the agent is down.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "Run with sudo."; exit 1; }
[ $# = 3 ] || { echo "Usage: sudo bash $0 <LOCAL_KEY> <ROUTER_IP> <RADIUS_SECRET>"; exit 1; }
KEY=$1; ROUTER=$2; SECRET=$3
[[ $KEY =~ ^[A-Za-z0-9]{16,}$ ]] || { echo "LOCAL_KEY: 16+ letters/digits only"; exit 1; }
[[ $SECRET =~ ^[A-Za-z0-9]{8,}$ ]] || { echo "RADIUS_SECRET: 8+ letters/digits only"; exit 1; }
[[ $ROUTER =~ ^[0-9.]+$ ]] || { echo "ROUTER_IP must look like 192.168.88.1"; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd); D=/etc/freeradius/3.0
[ -d $D ] || { echo "FreeRADIUS 3 not found. Install: sudo apt install -y freeradius freeradius-rest freeradius-utils"; exit 1; }
# Backups of the originals live OUTSIDE sites-enabled (FreeRADIUS would load a copy there as a second server).
B=$D/wifibms-backup; mkdir -p $B
[ -f $B/default.orig ] || cp -L $D/sites-enabled/default $B/default.orig
[ -f $B/clients.conf.orig ] || cp $D/clients.conf $B/clients.conf.orig

install -m 640 -o root -g freerad "$HERE/freeradius-rest.conf" $D/mods-available/rest
ln -sf $D/mods-available/rest $D/mods-enabled/rest
sed "s/CHANGE_ME_LOCAL_KEY/$KEY/" "$HERE/freeradius-wifibms.policy" > $D/policy.d/wifibms
chown root:freerad $D/policy.d/wifibms; chmod 640 $D/policy.d/wifibms

python3 - "$D" "$ROUTER" "$SECRET" <<'PY'
import sys
d, router, secret = sys.argv[1:4]
p = d + "/sites-enabled/default"
s = open(p).read()
if "wifibms_rest" not in s:
    if "authorize {\n" not in s or "accounting {\n" not in s:
        sys.exit("Unexpected sites-enabled/default layout: no authorize/accounting section found")
    s = s.replace("authorize {\n", "authorize {\n\twifibms_rest\n\tupdate control {\n\t\tAuth-Type := Accept\n\t}\n", 1)
    s = s.replace("accounting {\n", "accounting {\n\twifibms_rest\n", 1)
    open(p, "w").write(s)
c = d + "/clients.conf"
t = open(c).read()
if "client wifibms-router" in t:
    import re
    t = re.sub(r"client wifibms-router \{.*?\n\}\n", "", t, flags=re.S)
t += "\nclient wifibms-router {\n\tipaddr = %s\n\tsecret = %s\n}\n" % (router, secret)
open(c, "w").write(t)
PY
echo "Checking the configuration..."
freeradius -XC > /tmp/freeradius-check.log 2>&1 && tail -1 /tmp/freeradius-check.log || { tail -15 /tmp/freeradius-check.log; echo "FAILED: see above"; exit 1; }
systemctl enable freeradius >/dev/null 2>&1 || true
systemctl restart freeradius 2>/dev/null && echo "FreeRADIUS restarted." || echo "(could not restart through systemd here; restart it yourself)"
echo "Done. Expected: 'Configuration appears to be OK' above."
