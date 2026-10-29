//! Geohash encoding for location channels (iOS `Geohash`, `GeohashChannelLevel`).

const BASE32: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";

fn value(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    BASE32.iter().position(|&b| b as char == c).map(|i| i as u8)
}

/// 1 to 12 base32 characters, case-insensitive.
pub fn is_valid(geohash: &str) -> bool {
    let count = geohash.chars().count();
    (1..=12).contains(&count) && geohash.chars().all(|c| c.is_ascii() && value(c).is_some())
}

pub fn encode(latitude: f64, longitude: f64, precision: usize) -> String {
    let mut lat_range = (-90.0f64, 90.0f64);
    let mut lon_range = (-180.0f64, 180.0f64);
    let lat = latitude.clamp(-90.0, 90.0);
    let lon = longitude.clamp(-180.0, 180.0);

    let mut out = String::with_capacity(precision);
    let mut even = true;
    let mut bit = 0;
    let mut ch = 0u8;
    while out.len() < precision {
        let (range, v) = if even {
            (&mut lon_range, lon)
        } else {
            (&mut lat_range, lat)
        };
        let mid = (range.0 + range.1) / 2.0;
        if v >= mid {
            ch |= 1 << (4 - bit);
            range.0 = mid;
        } else {
            range.1 = mid;
        }
        even = !even;
        if bit < 4 {
            bit += 1;
        } else {
            out.push(BASE32[ch as usize] as char);
            bit = 0;
            ch = 0;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub lat_min: f64,
    pub lat_max: f64,
    pub lon_min: f64,
    pub lon_max: f64,
}

/// Bounding box. Characters outside the alphabet are skipped, as on iOS.
pub fn decode_bounds(geohash: &str) -> Bounds {
    let mut lat = (-90.0f64, 90.0f64);
    let mut lon = (-180.0f64, 180.0f64);
    let mut even = true;
    for c in geohash.chars() {
        let Some(cd) = value(c) else { continue };
        for mask in [16u8, 8, 4, 2, 1] {
            let range = if even { &mut lon } else { &mut lat };
            let mid = (range.0 + range.1) / 2.0;
            if cd & mask != 0 {
                range.0 = mid;
            } else {
                range.1 = mid;
            }
            even = !even;
        }
    }
    Bounds {
        lat_min: lat.0,
        lat_max: lat.1,
        lon_min: lon.0,
        lon_max: lon.1,
    }
}

pub fn decode_center(geohash: &str) -> (f64, f64) {
    let b = decode_bounds(geohash);
    ((b.lat_min + b.lat_max) / 2.0, (b.lon_min + b.lon_max) / 2.0)
}

/// Neighbours in N, NE, E, SE, S, SW, W, NW order, found by re-encoding offset
/// cell centres. Cells past a pole are left out; longitude wraps.
pub fn neighbors(geohash: &str) -> Vec<String> {
    if geohash.is_empty() {
        return Vec::new();
    }
    let precision = geohash.chars().count();
    let b = decode_bounds(geohash);
    let (lat, lon) = decode_center(geohash);
    let h = b.lat_max - b.lat_min;
    let w = b.lon_max - b.lon_min;
    let offsets = [
        (h, 0.0),
        (h, w),
        (0.0, w),
        (-h, w),
        (-h, 0.0),
        (-h, -w),
        (0.0, -w),
        (h, -w),
    ];
    offsets
        .iter()
        .filter_map(|&(dlat, dlon)| {
            let nlat = lat + dlat;
            if !(-90.0..=90.0).contains(&nlat) {
                return None;
            }
            let mut nlon = lon + dlon;
            while nlon > 180.0 {
                nlon -= 360.0;
            }
            while nlon < -180.0 {
                nlon += 360.0;
            }
            Some(encode(nlat, nlon, precision))
        })
        .collect()
}

/// Location channel levels and their geohash lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Region,
    Province,
    City,
    Neighborhood,
    Block,
    Building,
}

impl Level {
    pub const ALL: [Level; 6] = [
        Level::Region,
        Level::Province,
        Level::City,
        Level::Neighborhood,
        Level::Block,
        Level::Building,
    ];

    pub fn precision(self) -> usize {
        match self {
            Level::Region => 2,
            Level::Province => 4,
            Level::City => 5,
            Level::Neighborhood => 6,
            Level::Block => 7,
            Level::Building => 8,
        }
    }

    /// Level for a typed geohash of this length (iOS channel sheet).
    pub fn for_length(len: usize) -> Level {
        match len {
            0..=2 => Level::Region,
            3..=4 => Level::Province,
            5 => Level::City,
            6 => Level::Neighborhood,
            7 => Level::Block,
            8 => Level::Building,
            _ => Level::Block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_points() {
        assert_eq!(encode(57.64911, 10.40744, 11), "u4pruydqqvj");
        assert_eq!(encode(48.8566, 2.3522, 6), "u09tvw");
        let (lat, lon) = decode_center("ezs42");
        assert!((lat - 42.605).abs() < 0.01 && (lon + 5.603).abs() < 0.01);
    }

    #[test]
    fn validity() {
        assert!(is_valid("u4pruy"));
        assert!(is_valid("U4PRUY"));
        assert!(!is_valid(""));
        assert!(!is_valid("u4pruya"));
        assert!(!is_valid("u4pruyi"));
        assert!(!is_valid("#u4pruy"));
        assert!(!is_valid("0123456789bcd"));
    }

    #[test]
    fn neighbors_wrap_and_poles() {
        assert_eq!(neighbors("u4pruy").len(), 8);
        // Top row: the three northern neighbours are past the pole.
        assert_eq!(neighbors("zzzzzz").len(), 5);
        assert_eq!(neighbors("").len(), 0);
    }
}
