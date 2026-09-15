use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::OnceLock,
};

use serde::{Serialize, Serializer};

const DATABASE_MAGIC: &[u8; 8] = b"IUNAGEO2";
// Compile-time input: the bytes become part of every executable that uses this crate.
const DATABASE: &[u8] = include_bytes!("ip_geolocation/embedded/ip-country.bin");

static BUNDLED_GEOLOCATION: OnceLock<IpGeolocation> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CountryCode([u8; 2]);

impl CountryCode {
    fn new(bytes: [u8; 2]) -> Option<Self> {
        bytes
            .iter()
            .all(u8::is_ascii_uppercase)
            .then_some(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("country codes contain ASCII only")
    }
}

impl Serialize for CountryCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Copy, Default)]
struct PrefixGroup {
    offset: u32,
    count: u32,
}

struct EmbeddedDatabase {
    bytes: &'static [u8],
    ipv4: [PrefixGroup; 33],
    ipv6: [PrefixGroup; 129],
}

impl EmbeddedDatabase {
    fn parse(bytes: &'static [u8]) -> Result<Self, &'static str> {
        const HEADER_SIZE: usize = 8 + (33 + 129) * 4;
        if bytes.len() < HEADER_SIZE || &bytes[..8] != DATABASE_MAGIC {
            return Err("invalid database header");
        }

        let mut counts_cursor = 8;
        let mut read_count = || {
            let count =
                u32::from_le_bytes(bytes[counts_cursor..counts_cursor + 4].try_into().unwrap());
            counts_cursor += 4;
            count
        };
        let ipv4_counts = std::array::from_fn::<_, 33, _>(|_| read_count());
        let ipv6_counts = std::array::from_fn::<_, 129, _>(|_| read_count());
        let mut data_cursor = HEADER_SIZE;
        let mut ipv4 = [PrefixGroup::default(); 33];
        let mut ipv6 = [PrefixGroup::default(); 129];

        for (group, count) in ipv4.iter_mut().zip(ipv4_counts) {
            *group = prefix_group(data_cursor, count, 6, bytes.len())?;
            data_cursor += count as usize * 6;
        }
        for (group, count) in ipv6.iter_mut().zip(ipv6_counts) {
            *group = prefix_group(data_cursor, count, 18, bytes.len())?;
            data_cursor += count as usize * 18;
        }
        if data_cursor != bytes.len() {
            return Err("trailing database data");
        }
        Ok(Self { bytes, ipv4, ipv6 })
    }

    fn lookup_ipv4(&self, address: Ipv4Addr) -> Option<CountryCode> {
        let address = u32::from(address);
        for prefix in (0..=32).rev() {
            let network = address & prefix_mask_u32(prefix);
            let group = self.ipv4[prefix as usize];
            if let Some(country) = binary_search_group(self.bytes, group, 4, u128::from(network)) {
                return Some(country);
            }
        }
        None
    }

    fn lookup_ipv6(&self, address: Ipv6Addr) -> Option<CountryCode> {
        let address = u128::from(address);
        for prefix in (0..=128).rev() {
            let network = address & prefix_mask_u128(prefix);
            let group = self.ipv6[prefix as usize];
            if let Some(country) = binary_search_group(self.bytes, group, 16, network) {
                return Some(country);
            }
        }
        None
    }
}

fn prefix_group(
    offset: usize,
    count: u32,
    record_size: usize,
    database_len: usize,
) -> Result<PrefixGroup, &'static str> {
    let byte_len = (count as usize)
        .checked_mul(record_size)
        .ok_or("database size overflow")?;
    offset
        .checked_add(byte_len)
        .filter(|end| *end <= database_len)
        .ok_or("truncated database")?;
    Ok(PrefixGroup {
        offset: offset.try_into().map_err(|_| "database offset overflow")?,
        count,
    })
}

fn binary_search_group(
    database: &[u8],
    group: PrefixGroup,
    address_len: usize,
    target: u128,
) -> Option<CountryCode> {
    let record_size = address_len + 2;
    let mut left = 0usize;
    let mut right = group.count as usize;
    while left < right {
        let middle = left + (right - left) / 2;
        let offset = group.offset as usize + middle * record_size;
        let record = &database[offset..offset + record_size];
        let value = match address_len {
            4 => u128::from(u32::from_be_bytes(record[..4].try_into().unwrap())),
            16 => u128::from_be_bytes(record[..16].try_into().unwrap()),
            _ => unreachable!(),
        };
        match value.cmp(&target) {
            std::cmp::Ordering::Less => left = middle + 1,
            std::cmp::Ordering::Greater => right = middle,
            std::cmp::Ordering::Equal => {
                return CountryCode::new([record[address_len], record[address_len + 1]]);
            }
        }
    }
    None
}

fn prefix_mask_u32(prefix: u32) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn prefix_mask_u128(prefix: u32) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

pub struct IpGeolocation {
    database: EmbeddedDatabase,
}

impl IpGeolocation {
    pub fn bundled() -> &'static Self {
        BUNDLED_GEOLOCATION.get_or_init(|| {
            Self::from_database(DATABASE).expect("bundled IP geolocation database must be valid")
        })
    }

    pub fn country_for_ip(&self, address: IpAddr) -> Option<CountryCode> {
        match address {
            IpAddr::V4(address) if ipv4_is_geolocatable(address) => {
                self.database.lookup_ipv4(address)
            }
            IpAddr::V6(address) => {
                if let Some(mapped) = address.to_ipv4_mapped() {
                    return self.country_for_ip(IpAddr::V4(mapped));
                }
                ipv6_is_geolocatable(address).then_some(())?;
                self.database.lookup_ipv6(address)
            }
            IpAddr::V4(_) => None,
        }
    }

    fn from_database(database: &'static [u8]) -> Result<Self, &'static str> {
        Ok(Self {
            database: EmbeddedDatabase::parse(database)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_entries(entries: &[(IpAddr, u8, &str)]) -> Self {
        let mut ipv4 = std::array::from_fn::<_, 33, _>(|_| Vec::new());
        let mut ipv6 = std::array::from_fn::<_, 129, _>(|_| Vec::new());
        for (address, prefix, country) in entries {
            let bytes: [u8; 2] = country.as_bytes().try_into().unwrap();
            match address {
                IpAddr::V4(address) => {
                    ipv4[*prefix as usize].push((address.octets().to_vec(), bytes));
                }
                IpAddr::V6(address) => {
                    ipv6[*prefix as usize].push((address.octets().to_vec(), bytes));
                }
            }
        }
        let mut database = DATABASE_MAGIC.to_vec();
        for count in ipv4.iter().chain(ipv6.iter()).map(Vec::len) {
            database.extend_from_slice(&(count as u32).to_le_bytes());
        }
        for group in ipv4.iter_mut().chain(ipv6.iter_mut()) {
            group.sort_unstable();
            for (address, country) in group {
                database.extend_from_slice(address);
                database.extend_from_slice(country);
            }
        }
        Self::from_database(Box::leak(database.into_boxed_slice())).unwrap()
    }
}

fn ipv4_is_geolocatable(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn ipv6_is_geolocatable(address: Ipv6Addr) -> bool {
    let octets = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || octets[0] == 0xff
        || octets[0] & 0xfe == 0xfc
        || (octets[0] == 0xfe && octets[1] & 0xc0 == 0x80)
        || (octets[0] == 0xfe && octets[1] & 0xc0 == 0xc0)
        || (octets[0] == 0x20 && octets[1] == 0x01 && octets[2] == 0x0d && octets[3] == 0xb8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> IpGeolocation {
        IpGeolocation::from_entries(&[
            ("0.0.0.0".parse().unwrap(), 8, "US"),
            ("8.0.0.0".parse().unwrap(), 8, "US"),
            ("8.8.0.0".parse().unwrap(), 16, "NL"),
            ("10.0.0.0".parse().unwrap(), 8, "US"),
            ("127.0.0.0".parse().unwrap(), 8, "US"),
            ("169.254.0.0".parse().unwrap(), 16, "US"),
            ("172.16.0.0".parse().unwrap(), 12, "US"),
            ("192.168.0.0".parse().unwrap(), 16, "US"),
            ("224.0.0.0".parse().unwrap(), 4, "US"),
            ("::".parse().unwrap(), 0, "US"),
            ("2001:4860::".parse().unwrap(), 32, "US"),
            ("fc00::".parse().unwrap(), 7, "US"),
        ])
    }

    #[test]
    fn finds_ipv4_country() {
        assert_eq!(
            fixture()
                .country_for_ip("8.1.2.3".parse().unwrap())
                .unwrap()
                .as_str(),
            "US"
        );
    }

    #[test]
    fn finds_ipv6_country() {
        assert_eq!(
            fixture()
                .country_for_ip("2001:4860:4860::8888".parse().unwrap())
                .unwrap()
                .as_str(),
            "US"
        );
    }

    #[test]
    fn uses_longest_prefix_match() {
        assert_eq!(
            fixture()
                .country_for_ip("8.8.8.8".parse().unwrap())
                .unwrap()
                .as_str(),
            "NL"
        );
    }

    #[test]
    fn unknown_address_has_no_country() {
        assert_eq!(fixture().country_for_ip("11.0.0.1".parse().unwrap()), None);
    }

    #[test]
    fn non_geographic_addresses_have_no_country() {
        let geolocation = fixture();
        for address in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.10",
            "127.0.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "ff02::1",
        ] {
            assert_eq!(
                geolocation.country_for_ip(address.parse().unwrap()),
                None,
                "{address}"
            );
        }
    }

    #[test]
    fn bundled_database_loads_both_address_families() {
        let geolocation = IpGeolocation::bundled();
        assert_eq!(
            geolocation
                .country_for_ip("8.8.8.8".parse().unwrap())
                .unwrap()
                .as_str(),
            "US"
        );
        assert_eq!(
            geolocation
                .country_for_ip("2001:4860:4860::8888".parse().unwrap())
                .unwrap()
                .as_str(),
            "US"
        );
    }
}
