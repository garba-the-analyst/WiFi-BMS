# Installation guide: hardware first, then software

Follow the parts in order. One site is described; repeat Parts 1 and 3 for each extra location (Part 2, the central platform, is done once).
Each step says **where to click**. Menu names follow the current vendor software; if your firmware version labels something slightly differently,
look for the same word nearby: the order of the steps is what matters.

> **Honesty note.** The platform is tested against a real database and in a browser. The router, FreeRADIUS, CPE and access-point steps follow the
> vendors' documented behaviour but have **not been run on your hardware yet**. Do the first site on a table with one AP and one phone before mounting anything.

**Have ready:** a laptop with an Ethernet port, Winbox (mikrotik.com/download), a phone, a notebook for the address sheet, the Paystack account, the VPS login.

## 0. The plan on one page

| Network | Range | Used by |
|---|---|---|
| Management (untagged) | `192.168.88.0/24` | router `.1`, mini-PC `.2`, switch `.5`, CPE radios `.10-.19`, access points `.21-.29` |
| Customers (VLAN 10) | `10.10.0.0/22` | phones and laptops; router `10.10.0.1` |

Customers get VLAN 10 so they cannot reach your equipment. One secret (`RADIUS_SECRET`, you invent it) is used in three places and must be identical.
Write on your sheet: `RADIUS_SECRET`, `LOCAL_KEY` (`openssl rand -hex 24`), each device's MAC address and IP.

---

## Part 1. Connect and configure the hardware

### 1.1 Power and protection
UPS on a solid shelf; surge protector into the UPS; Starlink supply, router, switch, mini-PC and PoE injectors into the surge protector.
Earth every outdoor mast with a wire to a driven rod (lightning is the main cause of dead outdoor Wi-Fi gear).

### 1.2 Starlink to the router
1. **Starlink app → Settings → Advanced → Bypass mode** (the menu name can vary by Starlink hardware; Gen 2 needs the Starlink Ethernet adapter). Switch bypass **on**
   so only your MikroTik hands out addresses.
2. Cable: Starlink Ethernet → MikroTik **ether1**. Starlink is behind carrier NAT; fine, the agent only calls out.

### 1.3 MikroTik router (RouterOS 7)
**Start clean** (the factory config would clash with ours):
1. Plug the laptop into **ether2**. Open Winbox → **Neighbors** tab → click the router's MAC address → Connect (login `admin`, blank password).
2. **System → Reset Configuration** → tick **No Default Configuration** and **Do Not Backup** → Reset. Reconnect by MAC the same way.
3. **System → Users → admin → Password**: set a long password.

Then open **New Terminal** (left menu) and paste, adjusting if you use other ports:
```
/ip dhcp-client add interface=ether1 disabled=no
/ip firewall nat add chain=srcnat out-interface=ether1 action=masquerade
/ip dns set allow-remote-requests=yes servers=1.1.1.1,8.8.8.8
/ip address add address=192.168.88.1/24 interface=ether2
/interface vlan add name=vlan10 vlan-id=10 interface=ether2
/ip address add address=10.10.0.1/22 interface=vlan10
/ip firewall filter add chain=forward in-interface=vlan10 dst-address=192.168.88.0/24 action=drop comment="customers cannot reach management"
/ip firewall filter add chain=input in-interface=vlan10 protocol=tcp dst-port=22,23,8291,8728,8729 action=drop comment="no router admin from customers"
```
Where each lives in Winbox: **IP → DHCP Client**, **IP → Firewall → NAT**, **IP → DNS**, **IP → Addresses**, **Interfaces → VLAN**, **IP → Firewall → Filter Rules**.
Do **not** create a DHCP server or pool by hand: the hotspot wizard below makes them.

**Lock down services:** **IP → Services**: disable `telnet`, `ftp`, `www`, `api`, `api-ssl`. Double-click `winbox`, `ssh` and `www-ssl` and set **Available From** to `192.168.88.0/24`.
For `www-ssl` the certificate can stay blank for now; the agent reads RouterOS through it on the LAN (Part 3.6).

**Hotspot:**
1. **IP → Hotspot → Hotspot Setup** (button). Answer: interface `vlan10`; local address `10.10.0.1/22`, tick *Masquerade network*; address pool `10.10.0.10-10.10.3.250`;
   SSL certificate `none`; SMTP `0.0.0.0`; DNS servers `1.1.1.1,8.8.8.8`; DNS name blank; user `admin`, password blank.
2. **IP → Hotspot → Server Profiles** → double-click `hsprof1`: tab **Login** → tick only **HTTP PAP**; tab **RADIUS** → tick **Use RADIUS**, **Accounting**, **Interim Update** `00:01:00`.
   (If the wizard named the profile differently, use that name; check with `/ip hotspot profile print`.)
3. **RADIUS** (left menu) → **+**: Service `hotspot`, Address `192.168.88.2`, Secret = your `RADIUS_SECRET`, Timeout `3s`. Click the **Incoming** button: tick **Accept**, Port `3799`.
4. **IP → Hotspot → Walled Garden** tab → **+** (Action allow), one entry each for Dst. Host: `wifi.waste2light.com`, `*.paystack.com`, `*.paystack.co`, `js.paystack.co`, `checkout.paystack.com`.
   (`deploy/routeros-hotspot.rsc` does steps 2-4 in one paste; edit the secret first.)
5. **Files** (left menu): drag `deploy/routeros-login.html` into the window, then rename it to `hotspot/login.html` (replace the existing one). Open it first and set `site=site-1` to your site name.
6. **IP → Hotspot → Server Profiles → hsprof1 → General**: if the portal page looks half-loaded, confirm *HTML Directory* is `hotspot`.

### 1.4 Switch and devices
1. Router **ether2** → switch. Mini-PC → switch port. Each AP → a **PoE** port (EAP225-Outdoor: 802.3af, or passive 24 V; max 100 m; outdoor-rated Cat6; seal connectors).
2. **Managed switch** (open its web page, normally via its management IP): router port and every AP port = **trunk/tagged**: VLAN 1 untagged (management), VLAN 10 tagged.
   An unmanaged switch passes tagged frames and needs no setup.
3. Label every cable and write each MAC next to its planned IP.

### 1.5 TP-Link CPE radios (remote spots, 600 m+)
1. Power each through its **passive 24 V PoE injector** (never a standard 802.3af/at port). Injector **LAN** → your switch, injector **POE** → the radio.
2. Set the laptop to a fixed `192.168.0.100/24`, plug it into the same switch, browse to the radio's default address (see the label; commonly `http://192.168.0.254`).
   Log in; the first time it forces you to set the admin password.
3. Run the **Quick Setup** wizard. Hub radio: operation mode **Access Point**, SSID `link-1`, WPA2 password, LAN IP `192.168.88.10`, gateway `192.168.88.1`.
   Remote radio: **Client** (station) mode with the same SSID and password, LAN IP `192.168.88.11`. Keep **bridge** mode so VLAN 10 passes through.
4. Align: use the firmware's **antenna alignment** page (under *Tools* or *Wireless*, name varies) and adjust the aim until signal is **better than -65 dBm**.
   The platform flags a CPE at -75 dBm or worse.
5. Set the laptop back to automatic addressing.

### 1.6 Access points and the controller
Your APs are Omada products and you did not buy the OC200, so use the **free Omada Software Controller** on the mini-PC (otherwise per-AP client lists are not possible):
1. After Part 3.1 (mini-PC prepared), install the Omada Software Controller from TP-Link's download page (search "Omada Software Controller", Linux .deb).
   It needs the dependencies its page lists. Then browse to `https://192.168.88.2:8043` from the laptop and finish the setup wizard (controller name, admin account, region).
2. Power the APs. In the controller: **Devices → Pending** → select each AP → **Adopt** (wait for *Connected*). They are found automatically because they sit on the same network.
3. Click an AP in **Devices** → **Config → Network** (or *IP Settings*) → **Static** → `192.168.88.21` (then `.22`...), gateway `192.168.88.1`.
4. **Settings → Wireless Networks → Create New Wireless Network**: name `Waste2Light Wi-Fi`, security **None** (open). Open **Advanced Settings**: tick **VLAN** and enter `10`;
   turn on **Client Isolation** (or *Guest isolation*). Apply. Every adopted AP receives it.
5. **Settings → Wireless Networks → Wireless settings (per AP)**: 2.4 GHz channels 1, 6, 11 so neighbouring APs differ; lower transmit power on APs close together.
6. Controller account for the Devices page: **Settings → Platform Integration → Open API → Add New App** → mode **Client**, read-only role. Copy the **Client ID**, **Client Secret**, and the **Omada ID** shown on that page.
   The **site id** is the long code in the browser address while viewing your site.

### 1.7 Check before software
From the laptop on `192.168.88.0/24`: ping `192.168.88.1`, each AP and CPE. In Winbox **New Terminal**: `/ping 8.8.8.8`. Join the Wi-Fi with the phone: it gets a `10.10.x.x` address and the login page opens (it will not work yet; the portal comes next).

---

## Part 2. Central platform (the VPS, once)

1. **Cloudflare** (dash.cloudflare.com → your domain `waste2light.com`): **DNS → Records → Add record**: Type `A`, Name `wifi`, IPv4 = VPS address, Proxy on.
   **SSL/TLS → Overview** → **Full (strict)**. On the VPS firewall allow 80, 443 and SSH from your address only.
2. On the VPS (Docker with Compose v2 installed):
   ```
   git clone https://github.com/garba-the-analyst/WiFi-BMS.git && cd WiFi-BMS && cp .env.example .env
   nano .env
   ```
   In `.env` set: `PUBLIC_HOST=wifi.waste2light.com`, `OWNER_DB_PASSWORD` and `APP_DB_PASSWORD` (long random, different), `PLATFORM_MASTER_KEY` (output of `openssl rand -hex 32`).
   **Back up `PLATFORM_MASTER_KEY` offline**: losing it makes saved Paystack keys unreadable. Optional: `SMTP_URL`/`MAIL_FROM`, `TERMII_API_KEY`/`TERMII_SENDER_ID` for emailed/SMS codes.
3. ```
   docker compose up -d db
   docker compose --profile tools run --rm migrate
   docker compose run --rm api create-tenant waste2light "Waste2Light Wi-Fi" site-1 "Main site"
   docker compose up -d --build api caddy
   ADMIN_PASSWORD='a long passphrase' docker compose run --rm -e ADMIN_PASSWORD api create-admin waste2light you@example.com system_admin
   ```
   `deploy/config.json` already says tenant `waste2light`, site `site-1`; edit it before the build if you chose other names. The first build takes several minutes.
4. Open `https://wifi.waste2light.com/admin`, sign in, scan the QR code with an authenticator app (this is the first-sign-in 2-step step).
5. In the admin panel (left menu **Settings**):
   - **Business details**: name, tagline, support phone, colour.
   - **Paystack secret key**: paste `sk_test_...` first (switch to live later).
   - **Site agent keys**: Site `site-1`, Label `Site 1 PC` → **Create key**. **Copy the `nt_...` token now: shown once.**
   Left menu **Plans**: create your plans. **Staff**: add the CEO and others.
6. **Paystack dashboard → Settings → API Keys & Webhooks** → Webhook URL: `https://wifi.waste2light.com/v1/t/waste2light/webhooks/paystack`.

---

## Part 3. Site software (the mini-PC)

### 3.1 Prepare the machine
1. Install Ubuntu Server 24.04 LTS. Static address: edit `/etc/netplan/*.yaml` (address `192.168.88.2/24`, gateway `192.168.88.1`, DNS `1.1.1.1`), then `sudo netplan apply`.
2. ```
   sudo apt update && sudo apt install -y freeradius freeradius-rest freeradius-utils iputils-ping snmp ufw
   sudo useradd --system --home /var/lib/wifi-bms --shell /usr/sbin/nologin wifibms
   sudo mkdir -p /etc/wifi-bms && sudo chown root:wifibms /etc/wifi-bms && sudo chmod 750 /etc/wifi-bms
   sudo ufw allow from 192.168.88.1 to any port 1812,1813 proto udp
   sudo ufw allow from 192.168.88.0/24 to any port 8043 proto tcp
   sudo ufw allow ssh && sudo ufw enable
   ```
   The agent listens on `127.0.0.1:8081` only; do not open it.
3. Install the Omada Software Controller now if you skipped it in Part 1.6.

### 3.2 Site agent
Build on any x86-64 Linux with Rust (or on the mini-PC): `cargo build --release -p site-agent`; copy `target/release/site-agent` to the mini-PC as `/usr/local/bin/site-agent` and `sudo chmod 755` it.
```
sudo cp deploy/site-agent.service /etc/systemd/system/
sudo cp deploy/site-agent.env.example /etc/wifi-bms/site-agent.env
sudo chown root:wifibms /etc/wifi-bms/site-agent.env && sudo chmod 640 /etc/wifi-bms/site-agent.env
sudo nano /etc/wifi-bms/site-agent.env
```
Set `CENTRAL_URL=https://wifi.waste2light.com`, `NODE_TOKEN=nt_...` (Part 2 step 5), `LOCAL_KEY` (from your sheet), `COA_ADDR=192.168.88.1:3799`,
`COA_SECRET` = your `RADIUS_SECRET`, `RADCLIENT_BIN=radclient`. Then:
`sudo systemctl daemon-reload && sudo systemctl enable --now site-agent`.
Check: `curl -H "X-Agent-Key: <LOCAL_KEY>" localhost:8081/healthz` and `journalctl -u site-agent -f`.

### 3.3 FreeRADIUS
1. `sudo cp deploy/freeradius-rest.conf /etc/freeradius/3.0/mods-available/rest` and edit both `X-Agent-Key` lines to your `LOCAL_KEY`.
   Enable: `sudo ln -s /etc/freeradius/3.0/mods-available/rest /etc/freeradius/3.0/mods-enabled/rest`.
2. `sudo nano /etc/freeradius/3.0/sites-enabled/default`: in the `authorize { }` section add `rest` and `update control { Auth-Type := Accept }`; in `accounting { }` add `rest`.
   Comment out modules you do not need there (`chap`, `files`, `pap`...).
3. `sudo nano /etc/freeradius/3.0/clients.conf`, append:
   ```
   client mikrotik {
       ipaddr = 192.168.88.1
       secret = <your RADIUS_SECRET>
   }
   ```
4. Test in the foreground: `sudo systemctl stop freeradius && sudo freeradius -X`. Look for `rest` loading without errors and, when a phone tries a login, the request reaching the agent.
   This step is the most likely to need small field-name changes. Then `sudo systemctl enable --now freeradius`.

### 3.4 Router logins (recheck)
The same `RADIUS_SECRET` must be in: Winbox **RADIUS** entry, FreeRADIUS `clients.conf`, and `COA_SECRET` in the agent file.

### 3.5 Device monitoring (the Devices page)
1. On the router create a read-only monitoring user. **System → Users → Groups → +**: name `readonly-api`, policies `read`, `api`, `rest-api` only. **Users → +**: name `agent`, group `readonly-api`, a long password.
2. `sudo cp deploy/devices.example.json /etc/wifi-bms/devices.json` then `sudo nano` it: keep only what you own (see `docs/site-setup.md`).
   Fill `routeros` (url `https://192.168.88.1`, user `agent`, password) and `omada` (url `https://192.168.88.2:8043`, the IDs and secret from Part 1.6); list each CPE in `ping`.
3. `sudo chown root:wifibms /etc/wifi-bms/devices.json && sudo chmod 640 /etc/wifi-bms/devices.json`. Add `DEVICES_FILE=/etc/wifi-bms/devices.json` to `/etc/wifi-bms/site-agent.env`;
   `sudo systemctl restart site-agent`. Devices appear within a minute at **admin → Devices**.
4. CPE signal strength needs SNMP: on each CPE enable SNMP (in its web menu, usually under *System* or *Management* → **SNMP**), set a community, then from the mini-PC run
   `snmpwalk -v2c -c <community> 192.168.88.10 .1.3.6.1.4.1` and look for the line that shows the current signal; put its OID in `metrics` (see `docs/site-setup.md`).
   Until then the CPE still shows up/down, latency and loss.

---

## Part 4. Test end to end (in this order)
1. **admin → Network / Devices**: site online, devices listed.
2. **admin → Vouchers → Sell a plan (cash or transfer)** → *Make a code* → Sell plan → copy the code.
3. Phone: join `Waste2Light Wi-Fi`, the login page opens the portal, enter the code → internet works. It appears in **Customers**, and under **Devices** on its AP.
4. **Payment**: buy a plan on the portal with the Paystack **test** key; confirm the order turns paid (**Orders & refunds**); then paste the live key in Settings.
5. **Disconnect**: let a short plan end, or **Vouchers → Issued vouchers → Revoke**; the phone loses internet within about a minute.
6. **Failure drill**: unplug an AP: after about 3 minutes **Devices** shows it Offline and **Error log** gets a critical entry; replug: a recovery entry.
7. **Power drill**: switch off mains with the UPS connected; everything must stay up.

## Part 5. Before opening to the public
Change every default password (router, switch, APs, CPEs, Omada, mini-PC), keep a sealed copy plus `PLATFORM_MASTER_KEY` offline; replace `web/public/terms.html`
with reviewed terms and privacy notice (NDPA 2023, NCC rules); back up the database (`docs/deployment.md`); keep a spare AP and power supply.

## If something does not work
| Symptom | Look at |
|---|---|
| Phone gets no `10.10.x.x` address | hotspot wizard created DHCP on `vlan10`? switch port tagged for VLAN 10? SSID VLAN = 10? |
| Login page never appears | **IP → Hotspot → Walled Garden**, `hotspot/login.html` uploaded, **IP → Hotspot → Servers** shows `vlan10` enabled |
| Code rejected / no internet after code | **admin → Error log** (source radius/agent), `journalctl -u site-agent`, `freeradius -X`, the three secrets match |
| Paid but not connected | Paystack webhook URL, **Error log** (source payments) |
| Devices page empty | `DEVICES_FILE` set? `journalctl -u site-agent`; **Error log** `hardware.collector_failed` |
| CPE signal weak | realign, check line of sight and connector sealing |
