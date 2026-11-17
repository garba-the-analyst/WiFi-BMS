# RouterOS v7 hotspot + RADIUS. Adjust interface names and addresses.
/radius add service=hotspot address=192.168.88.2 secret="radius-shared-secret" timeout=3s
/radius incoming set accept=yes port=3799
/ip hotspot profile set [find default] use-radius=yes radius-accounting=yes radius-interim-update=1m login-by=http-pap
# Walled garden: portal, Paystack checkout and its assets
/ip hotspot walled-garden add dst-host=portal.example.com
/ip hotspot walled-garden add dst-host=*.paystack.com
/ip hotspot walled-garden add dst-host=*.paystack.co
# Enable REST for DHCP hints (optional): /ip service enable www-ssl
