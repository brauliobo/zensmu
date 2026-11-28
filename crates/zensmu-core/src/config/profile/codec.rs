//! Custom serde adapter for `BTreeMap<u32, i32>` so TOML table keys serialize as strings (`"0"`).

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserializer, Serializer};
use std::collections::BTreeMap;
use std::fmt;

pub(super) fn serialize<S>(
    map: &BTreeMap<u32, i32>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut s = serializer.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        s.serialize_entry(&k.to_string(), v)?;
    }
    s.end()
}

pub(super) fn deserialize<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<u32, i32>, D::Error>
where
    D: Deserializer<'de>,
{
    struct U32MapVisitor;

    impl<'de> Visitor<'de> for U32MapVisitor {
        type Value = BTreeMap<u32, i32>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map of non-negative integer core indices to CO margins")
        }

        fn visit_map<M>(self, mut access: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut map = BTreeMap::new();
            while let Some((key, val)) = access.next_entry::<String, i32>()? {
                let core_idx: u32 = key.parse().map_err(|_| {
                    de::Error::custom(format!(
                        "invalid core index key '{key}': expected non-negative integer"
                    ))
                })?;
                map.insert(core_idx, val);
            }
            Ok(map)
        }
    }

    deserializer.deserialize_map(U32MapVisitor)
}
