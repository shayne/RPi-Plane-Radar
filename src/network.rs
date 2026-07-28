use std::net::IpAddr;

use thiserror::Error;
use url::Url;

const MAX_LOCAL_URL_BYTES: usize = 128;

#[derive(Debug, Error)]
pub enum HostnameError {
    #[error("hostname must be a single ASCII hostname label")]
    Invalid,
}

#[derive(Debug, Error)]
pub enum LocalUrlError {
    #[error(
        "local URL must be a bounded HTTP origin without credentials, a path, query, or fragment"
    )]
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceAddress {
    pub name: String,
    pub address: IpAddr,
}

pub fn local_url(hostname: &str) -> Result<String, HostnameError> {
    let valid = !hostname.is_empty()
        && hostname.len() <= 63
        && hostname
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && hostname
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && hostname
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    valid
        .then(|| format!("http://{hostname}.local"))
        .ok_or(HostnameError::Invalid)
}

pub fn local_url_override(value: &str) -> Result<String, LocalUrlError> {
    if value.is_empty() || value.len() > MAX_LOCAL_URL_BYTES || value.chars().any(char::is_control)
    {
        return Err(LocalUrlError::Invalid);
    }
    let url = Url::parse(value).map_err(|_| LocalUrlError::Invalid)?;
    if url.scheme() != "http"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LocalUrlError::Invalid);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
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
