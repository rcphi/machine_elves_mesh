#!/usr/bin/env python3
"""A UPnP gateway for nat-rig.sh, which really forwards the ports it grants.

    fake-igd.py --inside 10.1.0.1 --external 203.0.113.1 --wan-if ao

Runs inside the rig's NAT namespace. It answers SSDP discovery, unicast and
multicast, serves the device and service descriptions igd-next reads, and on
AddPortMapping installs a DNAT rule in that namespace's `ip nat pre` chain, so
the node that asked becomes reachable from outside exactly as it would behind
a real router. DeletePortMapping removes the rule. Standard library only.
"""
import argparse
import re
import socket
import struct
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SERVICE = "urn:schemas-upnp-org:service:WANIPConnection:1"
DEVICE = "urn:schemas-upnp-org:device:InternetGatewayDevice:1"
HTTP_PORT = 5000

ROOT = f"""<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <specVersion><major>1</major><minor>0</minor></specVersion>
 <device>
  <deviceType>{DEVICE}</deviceType><friendlyName>nat-rig gateway</friendlyName>
  <manufacturer>nat-rig</manufacturer><modelName>fake-igd</modelName>
  <UDN>uuid:00000000-0000-0000-0000-000000000001</UDN>
  <serviceList><service>
   <serviceType>{SERVICE}</serviceType><serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
   <SCPDURL>/scpd.xml</SCPDURL><controlURL>/ctl/IPConn</controlURL><eventSubURL>/evt/IPConn</eventSubURL>
  </service></serviceList>
 </device>
</root>"""


def action(name, ins, outs=()):
    args = "".join(f"<argument><name>{a}</name><direction>in</direction></argument>" for a in ins)
    args += "".join(f"<argument><name>{a}</name><direction>out</direction></argument>" for a in outs)
    return f"<action><name>{name}</name><argumentList>{args}</argumentList></action>"


MAPPING = ("NewRemoteHost", "NewExternalPort", "NewProtocol")
SCPD = ('<?xml version="1.0"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>'
        + action("GetExternalIPAddress", (), ("NewExternalIPAddress",))
        + action("AddPortMapping", MAPPING + ("NewInternalPort", "NewInternalClient", "NewEnabled",
                                              "NewPortMappingDescription", "NewLeaseDuration"))
        + action("DeletePortMapping", MAPPING)
        + action("GetSpecificPortMappingEntry", MAPPING, ("NewInternalPort", "NewInternalClient"))
        + "</actionList></scpd>")


def envelope(body):
    return ('<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
            's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>'
            f"{body}</s:Body></s:Envelope>")


class Gateway:
    def __init__(self, external, wan_if):
        self.external, self.wan_if = external, wan_if
        self.rules = {}  # (protocol, external port) -> (nft handle, client, internal port)
        self.lock = threading.Lock()

    def add(self, protocol, ext, client, internal):
        with self.lock:
            self.delete(protocol, ext)
            out = subprocess.run(
                ["nft", "--echo", "--handle", "add", "rule", "ip", "nat", "pre", "iif", self.wan_if,
                 protocol.lower(), "dport", str(ext), "dnat", "to", f"{client}:{internal}"],
                capture_output=True, text=True, check=True).stdout
            handle = re.search(r"# handle (\d+)", out).group(1)
            self.rules[(protocol, ext)] = (handle, client, internal)
            print(f"mapped {protocol} {ext} -> {client}:{internal} (handle {handle})", flush=True)

    def delete(self, protocol, ext):
        rule = self.rules.pop((protocol, ext), None)
        if rule:
            subprocess.run(["nft", "delete", "rule", "ip", "nat", "pre", "handle", rule[0]], check=False)
            print(f"unmapped {protocol} {ext}", flush=True)
        return rule


def handler_for(gateway):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, code, body, kind="text/xml"):
            data = body.encode()
            self.send_response(code)
            self.send_header("Content-Type", f'{kind}; charset="utf-8"')
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            pages = {"/rootDesc.xml": ROOT, "/scpd.xml": SCPD}
            self.reply(200, pages[self.path]) if self.path in pages else self.reply(404, "")

        def do_POST(self):
            name = self.headers.get("SOAPAction", "").strip('"').split("#")[-1]
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode(errors="replace")
            arg = lambda key: (re.search(rf"<{key}>([^<]*)</{key}>", body) or [None, ""])[1]  # noqa: E731
            u = f'xmlns:u="{SERVICE}"'
            if name == "GetExternalIPAddress":
                self.reply(200, envelope(f"<u:GetExternalIPAddressResponse {u}>"
                                         f"<NewExternalIPAddress>{gateway.external}</NewExternalIPAddress>"
                                         "</u:GetExternalIPAddressResponse>"))
            elif name == "AddPortMapping":
                gateway.add(arg("NewProtocol"), int(arg("NewExternalPort")),
                            arg("NewInternalClient"), int(arg("NewInternalPort")))
                self.reply(200, envelope(f"<u:AddPortMappingResponse {u}/>"))
            elif name == "DeletePortMapping":
                gateway.delete(arg("NewProtocol"), int(arg("NewExternalPort")))
                self.reply(200, envelope(f"<u:DeletePortMappingResponse {u}/>"))
            elif name == "GetSpecificPortMappingEntry":
                rule = gateway.rules.get((arg("NewProtocol"), int(arg("NewExternalPort") or 0)))
                if rule:
                    self.reply(200, envelope(f"<u:GetSpecificPortMappingEntryResponse {u}>"
                                             f"<NewInternalPort>{rule[2]}</NewInternalPort>"
                                             f"<NewInternalClient>{rule[1]}</NewInternalClient>"
                                             "</u:GetSpecificPortMappingEntryResponse>"))
                else:
                    self.reply(500, envelope("<s:Fault><detail><UPnPError><errorCode>714</errorCode>"
                                             "<errorDescription>NoSuchEntryInArray</errorDescription>"
                                             "</UPnPError></detail></s:Fault>"))
            else:
                self.reply(500, envelope("<s:Fault><detail><UPnPError><errorCode>401</errorCode>"
                                         "</UPnPError></detail></s:Fault>"))
    return Handler


def ssdp(inside):
    """Answers M-SEARCH sent to this address or to the multicast group."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("0.0.0.0", 1900))
    group = struct.pack("4s4s", socket.inet_aton("239.255.255.250"), socket.inet_aton(inside))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, group)
    answer = ("HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nEXT:\r\n"
              f"LOCATION: http://{inside}:{HTTP_PORT}/rootDesc.xml\r\nSERVER: nat-rig UPnP/1.1 fake-igd/1\r\n"
              f"ST: {DEVICE}\r\nUSN: uuid:00000000-0000-0000-0000-000000000001::{DEVICE}\r\n\r\n").encode()
    while True:
        data, source = s.recvfrom(4096)
        if data.startswith(b"M-SEARCH"):
            s.sendto(answer, source)
            print(f"answered discovery from {source[0]}", flush=True)


def main(argv):
    parser = argparse.ArgumentParser()
    parser.add_argument("--inside", required=True)
    parser.add_argument("--external", required=True)
    parser.add_argument("--wan-if", required=True)
    args = parser.parse_args(argv)
    gateway = Gateway(args.external, args.wan_if)
    threading.Thread(target=ssdp, args=(args.inside,), daemon=True).start()
    ThreadingHTTPServer((args.inside, HTTP_PORT), handler_for(gateway)).serve_forever()


if __name__ == "__main__":
    main(sys.argv[1:])
