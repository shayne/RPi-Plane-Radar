use std::net::IpAddr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceAddress {
    pub name: String,
    pub address: IpAddr,
}

pub fn discover_ip_url(
    route_table: &str,
    interfaces: impl Iterator<Item = InterfaceAddress>,
) -> Option<String> {
    let addresses: Vec<_> = interfaces
        .filter(
            |interface| matches!(interface.address, IpAddr::V4(address) if !address.is_loopback()),
        )
        .collect();
    let route_interface = default_route_interface(route_table);
    let address = route_interface
        .as_deref()
        .and_then(|name| addresses.iter().find(|address| address.name == name))
        .or_else(|| addresses.first())?;
    match address.address {
        IpAddr::V4(address) => Some(format!("http://{address}")),
        IpAddr::V6(_) => None,
    }
}

pub fn current_interfaces() -> Result<Vec<InterfaceAddress>, nix::Error> {
    Ok(nix::ifaddrs::getifaddrs()?
        .filter_map(|interface| {
            let address = interface.address?.as_sockaddr_in()?.ip();
            Some(InterfaceAddress {
                name: interface.interface_name,
                address: IpAddr::V4(address),
            })
        })
        .collect())
}

fn default_route_interface(route_table: &str) -> Option<String> {
    route_table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let destination = u32::from_str_radix(fields.next()?, 16).ok()?;
            let _gateway = fields.next()?;
            let flags = u32::from_str_radix(fields.next()?, 16).ok()?;
            let _ref_count = fields.next()?;
            let _use_count = fields.next()?;
            let metric = fields.next()?.parse::<u32>().ok()?;
            (destination == 0 && flags & 1 != 0).then(|| (metric, name.to_owned()))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, name)| name)
}
