# Installation guide: hardware first, then software

Follow the parts in order. One site is described; repeat Parts 1, 3 and 4 for each additional location
(the central platform in Part 2 is done once).

> **Honesty note.** The platform code is tested against a real database and in a browser. The router, FreeRADIUS and
> access-point steps below follow the vendors' documented behaviour but have **not been run on your hardware yet**.
> Do the first site on the bench with one AP and one phone before you mount anything on a pole.

Hardware assumed (from your quotes): Starlink, MikroTik router (RB5009 or RB951/hEX class), managed or unmanaged PoE switch,
a mini-PC (site agent + FreeRADIUS), TP-Link EAP225-Outdoor access points, TP-Link CPE710/CPE510 radios for remote spots,
UPS 1.5 kVA, surge protector, outdoor Cat6.

---

## Part 1. Connect the hardware

### 1.1 Plan the addresses first (write them on a sheet)

| Network | Range | Used by |
|---|---|---|
| Management (untagged) | `192.168.88.0/24` | router `.1`, mini-PC `.2`, CPE radios `.10-.19`, access points `.21-.29`, switch `.5` |
| Customers (VLAN 10) | `10.10.0.0/22` | customer phones and laptops, router `10.10.0.1` |

Why two networks: customers must not be able to reach your access points, radios or mini-PC. Customer traffic travels on
VLAN 10; management traffic is untagged. If you skip the VLAN (single network), it still works but customers share the network with
your equipment: not recommended for a paid service.

### 1.2 Power and protection
1. Put the **UPS** on a solid shelf, away from rain and heat. Plug the **surge protector** into the UPS output, and plug everything
   (Starlink power supply, router, switch, mini-PC, PoE injectors) into the surge protector. The UPS keeps customers online through short outages
   and shuts the mini-PC down cleanly.
2. Earth the poles/masts for outdoor radios and APs (a ground wire to a driven rod). Lightning is the commonest killer of outdoor Wi-Fi gear in Nigeria.

### 1.3 Starlink to the router
1. Mount the Starlink dish with a clear sky view, route its cable to the Starlink router/adapter indoors.
2. Put the Starlink router in **bypass mode** (Starlink app → Settings → Advanced → Bypass mode; Gen 2 routers need the
   Ethernet adapter) so only your MikroTik hands out addresses. Otherwise you get two routers, "double NAT" and hotspot problems.
3. Cable: Starlink Ethernet port → MikroTik **ether1** (WAN). Starlink uses carrier-grade NAT, which is fine: the site agent only calls
   out to the platform, nothing connects in.

### 1.4 Router to switch to devices
1. MikroTik **ether2** → switch (this single cable carries management untagged and customers on VLAN 10).
2. Mini-PC → a switch port (untagged).
3. Each access point → a **PoE** switch port (or PoE injector). Check the label: the EAP225-Outdoor takes 802.3af PoE or passive 24 V.
   Max cable length 100 m. Use outdoor-rated Cat6 outdoors and seal the connectors.
4. Each CPE radio (CPE710/CPE510) uses **passive 24 V PoE** from its supplied injector. **Never** plug it into a standard 802.3af/at switch port.
   Plug the injector's LAN port into the switch, its POE port to the radio.
5. If you use a managed switch, set the router port and every AP port as **trunk**: management untagged, VLAN 10 tagged.
   An unmanaged switch simply passes tagged frames and needs no setup.
6. Label every cable and write each device's MAC address and planned IP on your sheet.

### 1.5 Radio links to remote spots (600 m+)
1. Hub radio at the main site and remote radio at the far spot must have **line of sight**. Aim the dish-side of each CPE at the other.
2. Configure them as a pair: one **Access Point** mode (hub, SSID e.g. `link-1`), one **Client/Station** mode (remote), same SSID and a WPA2 password.
   Set their management IPs to `192.168.88.10` (hub) and `.11` (remote), gateway `192.168.88.1`.
3. Use each radio's alignment tool (signal bar/beeper) until signal is stronger than **-65 dBm**. The platform flags a CPE at -75 dBm or worse.
4. At the remote spot, cable the remote radio's LAN port to a switch/injector and then to the local access point(s).
   Tagged VLAN 10 must pass through the radio link: enable "bridge/transparent" mode on both radios (default in client/AP bridge mode).

### 1.6 Access points
1. Give each AP a static management IP (`192.168.88.21`, `.22`, ...). Do this on the bench: connect a laptop to the AP's PoE port, browse to its
   default address (see the label), set the IP, gateway `192.168.88.1`.
2. Create **one SSID** for customers (for example `Waste2Light Wi-Fi`), **open (no password)**: login is by the portal. Tag it to **VLAN 10**.
   Turn on **client isolation** so customers cannot see each other. Lower the transmit power on APs close together; use channels 1, 6, 11 on 2.4 GHz.
3. Controller choice. Your APs are Omada products, and you did not buy the OC200:
   - **Recommended:** install the free **Omada Software Controller** on the mini-PC (Part 3.2). You get all APs in one screen, shared SSID settings and,
     importantly, per-AP client lists on the Devices page.
   - **Standalone:** configure each AP's web page by hand (same SSID/VLAN on each). The Devices page then shows up/down, latency and loss per AP but no per-AP client list.

### 1.7 Test the hardware before software
From a laptop on `192.168.88.0/24`: you can ping `192.168.88.1`, each AP, each CPE. From the router: `/ping 8.8.8.8` works. Fix this before continuing.

---

## Part 2. Central platform (the VPS, once)

You need a VPS (1 vCPU / 1 GB is enough to start), your domain `wifi.waste2light.com` and Docker with Compose v2.
Full detail is in `docs/deployment.md`; the short version:

1. Cloudflare: `A  wifi  →  <VPS IP>` (proxied is fine), SSL/TLS **Full (strict)**. Firewall the VPS to ports 80/443 and SSH from your address.
2. On the VPS: `git clone https://github.com/garba-the-analyst/WiFi-BMS.git && cd WiFi-BMS && cp .env.example .env`.
   Edit `.env`: `PUBLIC_HOST=wifi.waste2light.com`, two long random database passwords, and
   `PLATFORM_MASTER_KEY=$(openssl rand -hex 32)`. **Back the master key up offline** (losing it makes saved Paystack keys unreadable).
3. ```
   docker compose up -d db
   docker compose --profile tools run --rm migrate
   docker compose run --rm api create-tenant waste2light "Waste2Light Wi-Fi" site-1 "Main site"
   ```
   `deploy/config.json` already says tenant `waste2light`, site `site-1`; edit it if you chose other names.
4. `docker compose up -d --build api caddy` (builds the web app and fetches the TLS certificate; the first build takes several minutes).
5. Create your account:
   `ADMIN_PASSWORD='a long passphrase' docker compose run --rm -e ADMIN_PASSWORD api create-admin waste2light you@example.com system_admin`
6. Open `https://wifi.waste2light.com/admin`, sign in and enrol 2-step verification (authenticator app). Then:
   Settings → add the **Paystack** secret key, business details; **Plans** → create your plans; **Staff** → add the CEO and others.
7. Paystack dashboard → webhook URL `https://wifi.waste2light.com/v1/t/waste2light/webhooks/paystack`.
8. Settings → **Site agent keys** → create a key for the site. **Copy the `nt_...` token now: it is shown once.**

---

## Part 3. Site software (the mini-PC)

### 3.1 Prepare the machine
1. Install Ubuntu Server 24.04 LTS. Set a static IP `192.168.88.2/24`, gateway `192.168.88.1`, DNS `1.1.1.1`.
2. ```
   sudo apt update && sudo apt install -y freeradius freeradius-rest freeradius-utils iputils-ping snmp ufw
   sudo useradd --system --home /var/lib/wifi-bms --shell /usr/sbin/nologin wifibms
   sudo mkdir -p /etc/wifi-bms && sudo chown root:wifibms /etc/wifi-bms && sudo chmod 750 /etc/wifi-bms
   ```
3. Firewall: `sudo ufw allow from 192.168.88.1 to any port 1812,1813 proto udp && sudo ufw allow ssh && sudo ufw enable`.
   (The agent listens on `127.0.0.1:8081` only; do not open it.)

### 3.2 Omada Software Controller (if you chose the controller route)
Download the Linux package from TP-Link's Omada Software Controller page, install it per TP-Link's instructions (it needs Java and MongoDB, which the
package guide covers), then open `https://192.168.88.2:8043`, create the controller account, and **adopt** each AP (they appear under Devices; use
the APs' management IPs). Create the customer SSID/VLAN 10 once and it is pushed to every AP. Then create the Open API app for the Devices page (docs/site-setup.md).

### 3.3 Build and install the site agent
On any computer with Rust (or on the mini-PC): `cargo build --release -p site-agent`, then copy `target/release/site-agent` to the mini-PC
as `/usr/local/bin/site-agent` (`sudo chmod 755`). Same CPU type (x86-64) is required if you build elsewhere.

```
sudo cp deploy/site-agent.service /etc/systemd/system/
sudo cp deploy/site-agent.env.example /etc/wifi-bms/site-agent.env
sudo chown root:wifibms /etc/wifi-bms/site-agent.env && sudo chmod 640 /etc/wifi-bms/site-agent.env
sudo nano /etc/wifi-bms/site-agent.env
```
Set: `CENTRAL_URL=https://wifi.waste2light.com`, `NODE_TOKEN=nt_...` (from step 2.8), `LOCAL_KEY` (a long random string, `openssl rand -hex 24`),
`COA_ADDR=192.168.88.1:3799`, `COA_SECRET` (the same value as the RADIUS secret you set on the router and in FreeRADIUS, steps 3.4 and 3.6), and
`RADCLIENT_BIN=radclient`. Then `sudo systemctl daemon-reload && sudo systemctl enable --now site-agent`.
Check: `curl -H "X-Agent-Key: <LOCAL_KEY>" localhost:8081/healthz` and `journalctl -u site-agent -f`.

### 3.4 FreeRADIUS
1. Replace `/etc/freeradius/3.0/mods-available/rest` with `deploy/freeradius-rest.conf`, changing the `X-Agent-Key` header value to your `LOCAL_KEY`.
   `sudo ln -s /etc/freeradius/3.0/mods-available/rest /etc/freeradius/3.0/mods-enabled/rest`.
2. In `/etc/freeradius/3.0/sites-enabled/default`: in `authorize { }` add `rest` and `update control { Auth-Type := Accept }`; in `accounting { }` add `rest`
   (as the comment at the bottom of the file says). Remove or comment other authentication modules (`pap`, `chap`, `files`) you do not need.
3. In `/etc/freeradius/3.0/clients.conf` add the router:
   ```
   client mikrotik {
       ipaddr = 192.168.88.1
       secret = radius-shared-secret
   }
   ```
   Use your own secret; it must equal the one in the router's `/radius` entry (3.6).
4. Test in debug mode: `sudo systemctl stop freeradius && sudo freeradius -X`. Watch for `rest` loading without errors. This is the step most likely
   to need small field-name changes, because it has not run on your machine yet. Then `sudo systemctl enable --now freeradius`.

### 3.5 Device monitoring (Devices page)
Follow **docs/site-setup.md → Device monitoring**: copy `deploy/devices.example.json` to `/etc/wifi-bms/devices.json` (keep only what you have),
`sudo chown root:wifibms ... && chmod 640`, set `DEVICES_FILE=/etc/wifi-bms/devices.json`, restart the agent. Create a read-only user on the router for it (shown there).

### 3.6 Router (RouterOS 7)
Connect with Winbox or the terminal. Adjust names to your router.

```
# WAN and basic internet
/ip dhcp-client add interface=ether1 disabled=no
/ip firewall nat add chain=srcnat out-interface=ether1 action=masquerade
/ip dns set allow-remote-requests=yes servers=1.1.1.1,8.8.8.8

# Management network on ether2 (untagged) - the mini-PC, APs and radios live here
/ip address add address=192.168.88.1/24 interface=ether2

# Customer network on VLAN 10
/interface vlan add name=vlan10 vlan-id=10 interface=ether2
/ip address add address=10.10.0.1/22 interface=vlan10
/ip pool add name=hs-pool ranges=10.10.0.10-10.10.3.250
/ip dhcp-server add name=hs-dhcp interface=vlan10 address-pool=hs-pool disabled=no
/ip dhcp-server network add address=10.10.0.0/22 gateway=10.10.0.1 dns-server=10.10.0.1

# Keep customers away from your equipment
/ip firewall filter add chain=forward in-interface=vlan10 dst-address=192.168.88.0/24 action=drop comment="customers cannot reach management"
/ip firewall filter add chain=input in-interface=vlan10 dst-port=22,23,8291,8728,8729 protocol=tcp action=drop comment="no router admin from customers"
```
Make sure the router's admin password is changed and that Winbox/SSH are reachable only from the management network.
Do not block ports 80/443 on the router: the hotspot login page is served there.

Hotspot:
```
/ip hotspot setup        # interface=vlan10, address 10.10.0.1/22, pool hs-pool, no SSL certificate, DNS as above, no local users
```
Then apply `deploy/routeros-hotspot.rsc` (edit the secret to match step 3.4.3; it points RADIUS at the mini-PC `192.168.88.2`, turns on accounting and
CoA on port 3799, and sets the walled garden for the portal and Paystack). Upload `deploy/routeros-login.html` to **Files → hotspot/login.html**
(replacing the default; set `site=site-1`). In the walled garden also allow `js.paystack.co` and `checkout.paystack.com` as listed in docs/deployment.md.

Important: the `/ip hotspot profile set [find default]` line in the .rsc assumes the profile the wizard created is the default one. If your wizard named it otherwise,
change `[find default]` to that profile's name. Also confirm `/radius incoming` is enabled. One secret is used in three places and must be identical: the router's `/radius` entry, FreeRADIUS `clients.conf`, and `COA_SECRET` in the agent env
(RouterOS accepts CoA with the `/radius` entry's secret).

---

## Part 4. Test end to end

1. **Agent online:** admin → Dashboard/Network shows the site; Devices shows your devices within a minute.
2. **Access code:** admin → Vouchers → sell a manual voucher (or create a special one) → you get a code.
3. **Phone test:** connect to the Wi-Fi SSID → the login page redirects to the portal → enter the code → the phone gets internet. Check that
   the session appears under Customers and in the Devices page's client list.
4. **Payment test:** buy a plan on the portal with a Paystack **test** key first; confirm the webhook marks the order paid, then switch to the live key.
5. **Disconnect:** let a short plan end (or revoke the voucher): the phone must lose internet within about a minute (router CoA).
6. **Failure drills:** unplug an AP (Devices shows it offline and the Error log gets a critical entry in ~3 minutes), then plug it in again (recovery entry).
7. **Power drill:** pull mains power with the UPS in place: router, switch, mini-PC and APs should stay up.

## Part 5. Before opening to the public
- Change every default password (router, APs, CPEs, controller, mini-PC); keep a sealed copy of the passwords and `PLATFORM_MASTER_KEY` offline.
- Replace `web/public/terms.html` with your reviewed terms and privacy notice (NDPA 2023, NCC rules).
- Back up the database (docs/deployment.md → Backups).
- Mount a spare AP and a spare power supply; label everything.

## If something does not work
| Symptom | Look at |
|---|---|
| Login page never appears | walled garden, `login.html` upload, hotspot enabled on `vlan10` |
| Portal loads but "can't connect" after a code | admin → Error log (source `radius`/`agent`), `journalctl -u site-agent`, `freeradius -X` |
| Paid but not connected | Paystack webhook URL, Error log (source `payments`) |
| Devices page empty | `DEVICES_FILE` set? `journalctl -u site-agent`; Error log `hardware.collector_failed` |
| CPE shows weak signal | realign the dish; check the Fresnel zone and connectors |
