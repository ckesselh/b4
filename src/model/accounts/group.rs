//! The account group record (03 "Account group").

use serde::Serialize;

use super::kinds::GroupFlags;
use crate::model::base::{DecodeError, Record, RecordBase};
use crate::wire::Reader;

/// One group of `AcctGroups`, which accounts are sorted into.
///
/// Membership is stored on the account (`group_oid`), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountGroup {
    /// Object id and update stamp. Accounts refer to the id.
    #[serde(flatten)]
    pub base: RecordBase,
    /// Sort position among the groups. Groups sort by this, then by id.
    pub position: u16,
    /// Display name.
    pub name: Option<String>,
    /// Free text.
    pub description: Option<String>,
    /// Properties of the group.
    pub flags: GroupFlags,
}

impl Record for AccountGroup {
    const NAME: &'static str = "account group";

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        // One chunk, no conditionals, no version byte, no processing.
        Ok(Self {
            base: RecordBase::read(r),
            position: r.u16(),
            name: r.string("name")?,
            description: r.string("description")?,
            flags: GroupFlags::from_raw(r.i32()),
        })
    }

    fn oid(&self) -> i32 {
        self.base.oid
    }
}

#[cfg(test)]
mod tests {
    use super::super::sample::{chunk, file, Wire};
    use super::*;
    use crate::model::base::{load_collection, load_collection_strict};

    fn group_record(position: u16, name: Option<&str>, flags: i32) -> Vec<u8> {
        let mut w = Wire::default();
        w.i32(800_300_001);
        w.i32(800_400_001);
        w.u16(position);
        w.string(name);
        w.string(Some("a description"));
        w.i32(flags);
        chunk(&w.0, false)
    }

    #[test]
    fn decodes_the_four_fields() {
        let bytes = file(&[group_record(65535, Some("Household"), -8)]);
        let groups = load_collection_strict::<AccountGroup>(&bytes)
            .unwrap()
            .records;
        assert_eq!(
            groups,
            [AccountGroup {
                base: RecordBase {
                    oid: 800_300_001,
                    update_stamp: 800_400_001,
                },
                position: 65535,
                name: Some("Household".to_owned()),
                description: Some("a description".to_owned()),
                // Undefined bits are kept as they are.
                flags: GroupFlags::from_raw(-8),
            }]
        );
        assert!(!groups[0].flags.contains(GroupFlags::EXCLUDE_FROM_TOTALS));
        assert_eq!(groups[0].flags.names(), ["0xfffffff8"]);
    }

    #[test]
    fn a_short_record_decodes_to_defaults() {
        let record = group_record(7, Some("Household"), 0x2);
        // Cut after the position: header, two base fields, two bytes.
        let cut = chunk(&record[4..14], false);
        let groups = load_collection_strict::<AccountGroup>(&file(&[cut]))
            .unwrap()
            .records;
        assert_eq!(groups[0].position, 7);
        assert_eq!(groups[0].name, None);
        assert_eq!(groups[0].description, None);
        assert!(groups[0].flags.is_empty());

        let empty = load_collection::<AccountGroup>(&file(&[chunk(&[], false)]));
        assert_eq!(empty.records[0].base.oid, 0);
        assert_eq!(empty.records[0].position, 0);
    }
}
