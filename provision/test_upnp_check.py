"""Tests for upnp-check.py's reading of what a router says.

A misread here reports "no UPnP" for a router that offers it, and the
relaying design's Run G decision rests on that answer."""
import importlib.machinery
import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("upnp_check", os.path.join(HERE, "upnp-check.py"))
spec = importlib.util.spec_from_loader("upnp_check", loader)
upnp = importlib.util.module_from_spec(spec)
loader.exec_module(upnp)

# Two services, as real gateways list them: the one wanted is not first.
DESCRIPTION = """<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <device><manufacturer>Acme</manufacturer><modelName>Router 9</modelName>
  <serviceList>
   <service>
    <serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>
    <controlURL>/ctl/CmnIfCfg</controlURL>
   </service>
   <service>
    <serviceType>urn:schemas-upnp-org:service:WANIPConnection:2</serviceType>
    <controlURL>/ctl/IPConn</controlURL>
   </service>
  </serviceList>
 </device>
</root>"""


class ReadingTheRouter(unittest.TestCase):
    def test_finds_the_connection_service_not_the_first_one(self):
        found = upnp.control_url(DESCRIPTION, "http://192.168.1.1:5000/rootDesc.xml")
        self.assertEqual(found, ("urn:schemas-upnp-org:service:WANIPConnection:2",
                                 "http://192.168.1.1:5000/ctl/IPConn"))

    def test_a_url_base_overrides_the_location(self):
        with_base = DESCRIPTION.replace("<device>", "<URLBase>http://192.168.1.1:49152/</URLBase><device>", 1)
        _, url = upnp.control_url(with_base, "http://192.168.1.1:5000/rootDesc.xml")
        self.assertEqual(url, "http://192.168.1.1:49152/ctl/IPConn")

    def test_ppp_connections_count_too(self):
        ppp = DESCRIPTION.replace("WANIPConnection:2", "WANPPPConnection:1")
        self.assertEqual(upnp.control_url(ppp, "http://gw/")[0],
                         "urn:schemas-upnp-org:service:WANPPPConnection:1")

    def test_no_connection_service_is_none(self):
        none = DESCRIPTION.replace("WANIPConnection:2", "Layer3Forwarding:1")
        self.assertIsNone(upnp.control_url(none, "http://gw/"))

    def test_ssdp_headers_are_read_whatever_their_case(self):
        answer = (b"HTTP/1.1 200 OK\r\nCache-Control: max-age=120\r\n"
                  b"location: http://192.168.1.1:5000/rootDesc.xml\r\nST: x\r\n\r\n")
        self.assertEqual(upnp.parse_ssdp(answer)["LOCATION"], "http://192.168.1.1:5000/rootDesc.xml")

    def test_a_refusal_reports_the_routers_own_error(self):
        fault = ('<s:Envelope><s:Body><s:Fault><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0">'
                 "<errorCode>718</errorCode><errorDescription>ConflictInMappingEntry</errorDescription>"
                 "</UPnPError></detail></s:Fault></s:Body></s:Envelope>")
        self.assertEqual(upnp.fields(fault)["errorCode"], "718")
        self.assertEqual(upnp.fields(fault)["errorDescription"], "ConflictInMappingEntry")

    def test_answers_are_read_without_their_namespace_prefix(self):
        answer = ("<s:Envelope><s:Body><u:GetExternalIPAddressResponse>"
                  "<NewExternalIPAddress>75.118.77.81</NewExternalIPAddress>"
                  "</u:GetExternalIPAddressResponse></s:Body></s:Envelope>")
        self.assertEqual(upnp.fields(answer)["NewExternalIPAddress"], "75.118.77.81")


if __name__ == "__main__":
    unittest.main()
