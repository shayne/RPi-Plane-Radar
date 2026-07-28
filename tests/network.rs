use std::net::{IpAddr, Ipv4Addr};

use planeradar::network::{InterfaceAddress, discover_ip_url, local_url};

fn address(name: &str, octets: [u8; 4]) -> InterfaceAddress {
    InterfaceAddress {
        name: name.to_owned(),
        address: IpAddr::V4(Ipv4Addr::from(octets)),
    }
}

#[test]
fn discovers_default_route_interface_before_other_addresses() {
    let routes = "Iface Destination Gateway Flags RefCnt Use Metric Mask\nwlan0 00000000 0101A8C0 0003 0 0 600 00000000\n";
    let url = discover_ip_url(
        routes,
        [
            address("eth0", [192, 168, 1, 12]),
            address("wlan0", [10, 0, 0, 8]),
        ]
        .into_iter(),
    );
    assert_eq!(url.as_deref(), Some("http://10.0.0.8"));
}

#[test]
fn excludes_loopback_and_falls_back_to_first_usable_ipv4_address() {
    let url = discover_ip_url(
        "Iface Destination Gateway Flags RefCnt Use Metric Mask\n",
        [
            address("lo", [127, 0, 0, 1]),
            address("eth0", [192, 168, 50, 4]),
            address("wlan0", [10, 0, 0, 8]),
        ]
        .into_iter(),
    );
    assert_eq!(url.as_deref(), Some("http://192.168.50.4"));
}

#[test]
fn selects_lowest_metric_up_default_route_and_ignores_malformed_rows() {
    let routes = "garbage\nIface Destination Gateway Flags RefCnt Use Metric Mask\neth0 00000000 00000000 0001 0 0 400 00000000\nwlan0 00000000 00000000 0003 0 0 100 00000000\nbad 0x0 00000000 0003 0 0 1 00000000\n";
    let url = discover_ip_url(
        routes,
        [
            address("eth0", [192, 168, 1, 12]),
            address("wlan0", [10, 0, 0, 8]),
        ]
        .into_iter(),
    );
    assert_eq!(url.as_deref(), Some("http://10.0.0.8"));
}

#[test]
fn builds_local_url_from_a_valid_hostname() {
    assert_eq!(local_url("planeradar").unwrap(), "http://planeradar.local");
    assert_eq!(local_url("hangar-2").unwrap(), "http://hangar-2.local");
}

#[test]
fn rejects_hostname_text_that_could_change_the_authority() {
    for value in [
        "",
        ".local",
        "radar.local",
        "radar/evil",
        "radar:80",
        "-radar",
    ] {
        assert!(local_url(value).is_err(), "{value}");
    }
}
