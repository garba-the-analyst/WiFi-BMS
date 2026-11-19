# Site setup

1. Admin panel: create a node for the site, copy its `nt_` token once.
2. Install `site-agent` (systemd unit in `deploy/`), fill `/etc/wifi-bms/site-agent.env`.
3. Install FreeRADIUS, apply `deploy/freeradius-rest.conf`.
4. Run `deploy/routeros-hotspot.rsc` on the router (adjust).
5. Test: `curl -H 'X-Agent-Key: ...' localhost:8081/healthz`, then connect with a test code.
6. (Recommended) Turn on device monitoring: see below.

Needs hardware validation: FreeRADIUS rlm_rest JSON shape, radclient CoA/Disconnect, RouterOS REST.
The agent does not yet send TTL/connection observations, so tether detection currently relies on device slots only.

## Device monitoring (the Devices page)

**What you get.** Admin → **Devices** lists every CPE radio, access point and the router, each with a status
(online / needs attention / offline), signal strength, link quality, CPU, memory, load, latency, and the customers connected
to it (matched to their account and plan). Tap a device for its last 24 hours, its connected clients and its recent
problems. Every role can open it; it is the first place to look when a customer says "the Wi-Fi is bad at the market".

**How it works.** The site agent reads your hardware every minute and sends one snapshot to the platform. It only reads;
it cannot affect customer logins. Without `DEVICES_FILE` it is off and the page stays empty.

1. Copy `deploy/devices.example.json` (or `deploy/devices.standalone.example.json`) to `/etc/wifi-bms/devices.json`,
   keep only the devices you have, then `chmod 600` it (it holds passwords) and `chown wifibms`.
2. In `/etc/wifi-bms/site-agent.env` set `DEVICES_FILE=/etc/wifi-bms/devices.json`.
3. `systemctl restart site-agent`. The devices appear within a minute. A mistake in the file stops the agent
   from starting with a message saying which entry is wrong (`journalctl -u site-agent`).

`interval_s` (10-3600, default 60) is how often everything is read. At most 100 devices and 500 clients per device are accepted.

### What each source gives

| Source | Use it for | Gives |
|---|---|---|
| `ping` | anything with an IP | up/down, latency, packet loss |
| `snmp` | CPE radios, switches, APs that speak SNMP | ping, plus any number you map (signal, noise, link quality, CPU, memory, load, speed) |
| `routeros` | the MikroTik router | CPU, memory, uptime, version, and every hotspot client |
| `omada` | the TP-Link access points | per-AP health, channel, load, and the clients on each AP with their signal |

A source that fails (wrong password, unreachable) does not stop the others: you get one **hardware.collector_failed** entry in
the Error log and that source's devices show as not reporting.

### Ping
`{ "name": "Market AP 1", "ip": "192.168.88.21", "kind": "ap" }`. `kind` is one of `router`, `ap`, `cpe`, `switch`, `controller`, `other`.
`peer` (CPE only) names the radio at the other end of the link. The agent needs the system `ping` command (package `iputils-ping`).

### SNMP (CPE signal strength)
Turn SNMP v2c on in the device (for TP-Link Pharos CPEs: System → SNMP), choose a community other than `public`,
and allow only the mini-PC's address if the device lets you. Then find the numbers to read **from the mini-PC**:

```
sudo apt install snmp
snmpwalk -v2c -c <community> 192.168.88.10 .1.3.6.1.4.1      # vendor-specific values
snmpwalk -v2c -c <community> 192.168.88.10 .1.3.6.1.2.1      # standard values (uptime, interfaces)
```

Look for the line showing the current signal (for example a value near -60) and note its OID. Put it in `metrics`:
`"metrics": { "signal_dbm": "1.3.6.1.4.1.…" }`. Valid names: `signal_dbm`, `noise_dbm`, `link_quality_pct`, `cpu_pct`,
`mem_pct`, `util_pct`, `tx_mbps`, `rx_mbps`. If the device reports tenths (-600 for -60.0), add `"scale": { "signal_dbm": 0.1 }`.
OIDs are digits and dots only; the agent reads each with a single GET, so give the full OID including the final `.0` where the walk shows one.
**Not verified on real hardware:** CPE vendors differ, so the signal OID has to be found on your own device. If it cannot be found, ping alone still tells you when a radio is down.

### RouterOS
On the MikroTik create a read-only user for the agent and make sure the REST API (`www-ssl`, RouterOS 7) is enabled:

```
/user group add name=readonly-api policy=read,api,rest-api,!write,!policy,!test,!sensitive,!password
/user add name=agent group=readonly-api password=<long-random>
```

`url` is the router's LAN address (`https://192.168.88.1`). Its self-signed certificate is accepted on the LAN.
The agent reads `system/resource` and `ip/hotspot/active`.

### Omada (access points and their clients)
You do not need the OC200 hardware. TP-Link's **Omada Software Controller** is free: install it on the site mini-PC
(or any always-on machine on the LAN), adopt the access points into it, and the agent reads it the same way. Only the `url` differs
(`https://<mini-pc>:8043`). In the controller: Settings → Platform Integration → Open API → Add App (client credentials mode),
note the **Client ID** and **Client Secret**; the **Omada ID** is shown on that page, and the **site id** is in the browser address
while viewing the site. Give the app read-only rights.
**Not verified against a live controller:** field names vary a little between controller versions; the agent accepts the common variants.
If APs or clients do not appear, check the Error log for `hardware.collector_failed`, which gives the reason the collector reported.

**Standalone APs (no controller).** Use `deploy/devices.standalone.example.json`: each AP is pinged (and read by SNMP if its firmware offers it). You
see up/down, latency and loss per AP, and the router's hotspot list gives the total clients, but there is no per-AP client list.
That list needs a controller.

### When something is flagged
A device is marked **needs attention** (and one Error-log entry is written when that starts, one more when it recovers) when:

| Signal | Threshold |
|---|---|
| CPE signal | -75 dBm or weaker (CPE radios only) |
| Link quality | below 60 % |
| CPU / memory / load | 90 % / 92 % / 80 % or more |
| Packet loss / latency | 5 % or more / 200 ms or more |

It is **offline** when it does not answer, or when the agent stops reporting it for 3 minutes. If the agent itself goes silent, the page shows
every device as offline after 3 minutes, but only the site-level "agent silent" alert is raised (not one alert per device), since the cause is the agent or the site's power or link, not the devices.
Samples are kept 7 days, devices not seen for 30 days are removed.
