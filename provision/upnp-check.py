#!/usr/bin/env python3
"""Does this network's router open a port when asked by UPnP?

    python3 upnp-check.py            ask, map a test port for 2 minutes, delete it
    python3 upnp-check.py --no-map   only find the router and read its address

Answers the question the relaying design (2026-11-18) needs before Run G:
whether the friend's router, which ignores NAT-PMP, will grant a mapping some
other way. If it will, diamond can become a second reachable member, and losing
riverboat need not split the mesh.

**Only the gateway is listened to.** The discovery query is multicast, so other
devices on the network may hear it and answer. Their answers are ignored here
and, on diamond, dropped by the firewall. The mapping is made on an unused port
(40999, not the mesh's 4001), points at this machine only, expires by itself
after 120 s, and is deleted as soon as it has been read back.

Standard library only, like the probe: it runs on a volunteer's box as is.
"""
import re
import socket
import sys
import time
import urllib.parse
import urllib.request

SSDP = ("239.255.255.250", 1900)
TARGETS = (
    "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
)
CONNECTION = re.compile(r"urn:schemas-upnp-org:service:WAN(?:IP|PPP)Connection:\d")
TEST_PORT = 40999
LEASE_SECS = 120


def gateway():
    """The IPv4 default gateway, from the kernel's routing table."""
    with open("/proc/net/route") as table:
        for line in table.readlines()[1:]:
            fields = line.split()
            if fields[1] == "00000000" and int(fields[3], 16) & 0x2:
                return socket.inet_ntoa(bytes.fromhex(fields[2])[::-1])
    raise SystemExit("no IPv4 default route")


def local_address(towards):
    """The address this machine uses to reach `towards`. Nothing is sent."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.connect((towards, 1900))
        return s.getsockname()[0]


def parse_ssdp(data):
    """Headers of one SSDP answer, keys upper-cased."""
    headers = {}
    for line in data.decode(errors="replace").split("\r\n")[1:]:
        if ":" in line:
            key, value = line.split(":", 1)
            headers[key.strip().upper()] = value.strip()
    return headers


def search(gw, wait=3.0):
    """LOCATION of the gateway's description, asking unicast first, then multicast.

    Unicast is tried first because it needs no multicast at all, which a
    box isolated from its household (isolate-lan) is not allowed to send.
    Many routers answer only the multicast form, hence the second try."""
    for destination, how in (((gw, 1900), "unicast to the gateway"), (SSDP, "multicast")):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.settimeout(0.5)
            try:
                for target in TARGETS:
                    s.sendto(search_message(target), destination)
            except OSError as e:
                print(f"  {how}: could not send ({e})")
                continue
            ignored = 0
            end = time.time() + wait
            while time.time() < end:
                try:
                    data, (source, _) = s.recvfrom(8192)
                except socket.timeout:
                    continue
                if source != gw:
                    ignored += 1
                    continue
                location = parse_ssdp(data).get("LOCATION")
                if location:
                    print(f"  {how}: the gateway answered")
                    return location
            print(f"  {how}: no answer from the gateway"
                  + (f" ({ignored} from other devices, ignored)" if ignored else ""))
    return None


def search_message(target):
    return ("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n"
            'MAN: "ssdp:discover"\r\nMX: 2\r\n'
            f"ST: {target}\r\n\r\n").encode()


def control_url(description, location):
    """(service type, absolute control URL) of the WAN connection service."""
    base = re.search(r"<URLBase>\s*(.*?)\s*</URLBase>", description)
    base = base.group(1) if base else location
    for service in re.findall(r"<service>(.*?)</service>", description, re.S):
        kind = re.search(r"<serviceType>\s*(.*?)\s*</serviceType>", service)
        url = re.search(r"<controlURL>\s*(.*?)\s*</controlURL>", service)
        if kind and url and CONNECTION.fullmatch(kind.group(1)):
            return kind.group(1), urllib.parse.urljoin(base, url.group(1))
    return None


def soap(url, service, action, arguments=()):
    """(ok, {name: value}) for one call; on a fault, the router's own error."""
    args = "".join(f"<{name}>{value}</{name}>" for name, value in arguments)
    body = ('<?xml version="1.0"?>'
            '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
            's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>'
            f'<u:{action} xmlns:u="{service}">{args}</u:{action}>'
            "</s:Body></s:Envelope>").encode()
    request = urllib.request.Request(url, data=body, method="POST", headers={
        "Content-Type": 'text/xml; charset="utf-8"',
        "SOAPAction": f'"{service}#{action}"',
    })
    try:
        with urllib.request.urlopen(request, timeout=5) as answer:
            return True, fields(answer.read().decode(errors="replace"))
    except urllib.error.HTTPError as e:
        return False, fields(e.read().decode(errors="replace"))
    except OSError as e:
        return False, {"error": str(e)}


def fields(xml):
    """Leaf elements of a SOAP answer, namespace prefixes dropped."""
    return {name.split(":")[-1]: value
            for name, value in re.findall(r"<([\w:]+)>([^<>]*)</\1>", xml)}


def main(argv):
    gw = gateway()
    me = local_address(gw)
    print(f"gateway {gw}, this machine {me}")
    location = search(gw)
    if not location:
        print("RESULT: the router did not answer UPnP discovery")
        return 1
    try:
        description = urllib.request.urlopen(location, timeout=5).read().decode(errors="replace")
    except OSError as e:
        print(f"RESULT: the router answered, but its description could not be read ({e})")
        return 1
    model = re.findall(r"<(?:manufacturer|modelName)>(.*?)</", description)[:2]
    print(f"  device: {' '.join(model) or 'unnamed'}")
    found = control_url(description, location)
    if not found:
        print("RESULT: the router speaks UPnP but offers no port-mapping service")
        return 1
    service, url = found
    print(f"  service: {service}")
    ok, answer = soap(url, service, "GetExternalIPAddress")
    print(f"  external address: {answer.get('NewExternalIPAddress', answer)}")
    if "--no-map" in argv:
        print("RESULT: the router offers port mapping (not tried: --no-map)")
        return 0
    mapping = (("NewRemoteHost", ""), ("NewExternalPort", TEST_PORT), ("NewProtocol", "UDP"))
    ok, answer = soap(url, service, "AddPortMapping", mapping + (
        ("NewInternalPort", TEST_PORT), ("NewInternalClient", me), ("NewEnabled", 1),
        ("NewPortMappingDescription", "mesh upnp check"), ("NewLeaseDuration", LEASE_SECS)))
    if not ok:
        print(f"RESULT: the router REFUSED a mapping: "
              f"{answer.get('errorCode', '?')} {answer.get('errorDescription', answer)}")
        return 1
    ok, entry = soap(url, service, "GetSpecificPortMappingEntry", mapping)
    print(f"  mapped UDP {TEST_PORT} for {LEASE_SECS} s; read back: "
          f"{entry.get('NewInternalClient', entry)}:{entry.get('NewInternalPort', '')}, "
          f"lease {entry.get('NewLeaseDuration', '?')}")
    deleted, answer = soap(url, service, "DeletePortMapping", mapping)
    print(f"  deleted: {'yes' if deleted else answer} "
          f"{'' if deleted else f'(it expires by itself within {LEASE_SECS} s)'}")
    print("RESULT: the router GRANTS port mappings by UPnP" if ok
          else "RESULT: the router accepted a mapping but could not read it back")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
