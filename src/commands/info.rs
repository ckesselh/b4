//! `b4 info <VAULT>`: show a vault's header without the password.

use crate::cli::{Globals, InfoArgs};
use crate::output::{self, Cell, Column, Record};
use crate::vault::{Header, Timestamp};
use crate::Error;

/// Show the unencrypted header of the vault `args.vault`.
///
/// Reads the first 512 bytes of the file with read access only (01 §3) and
/// asks the file system for the size. No password is asked for, nothing is
/// decrypted and the database is not touched. Fails on a file that cannot be
/// read, is shorter than a header, has other magic bytes or an unknown
/// format version.
///
/// The header is not validated beyond that (01 §3.2), so a cipher id that
/// b4 cannot decrypt is shown as `unknown (id N)` and not refused here.
pub fn run(args: &InfoArgs, globals: &Globals) -> Result<(), Error> {
    let header = Header::read(&args.vault)?;
    // The error of `Header::read` names the file; this one has to as well.
    let file_size = std::fs::metadata(&args.vault)
        .map_err(|error| {
            Error::from(error).context(format_args!(
                "cannot read the size of vault file {}",
                args.vault.display()
            ))
        })?
        .len();
    output::print_record(&record(&header, file_size), globals)
}

/// The header as a record. The keys are part of the command's interface.
fn record(header: &Header, file_size: u64) -> Record {
    let cipher = match header.cipher() {
        Some(cipher) => cipher.name().to_owned(),
        None => format!("unknown (id {})", header.cipher_id()),
    };
    // 01 §3.4: a stamp of 0 means "not set" and is shown as nothing.
    let time = |stamp: Timestamp| Cell::from(stamp.to_iso8601());

    let mut record = Record::new();
    record.push(
        Column::integer("format_version", "Format version"),
        i64::from(header.format_version()),
    );
    record.push(Column::text("cipher", "Cipher"), cipher);
    // What is run, not what is stored: zero or less means none (01 §5.2).
    record.push(
        Column::integer("kdf_rounds", "Key-stretch rounds"),
        i64::from(header.effective_kdf_rounds()),
    );
    record.push(
        Column::integer("cluster_size", "Cluster size"),
        i64::from(header.cluster_size()),
    );
    record.push(
        Column::text("vault_id", "Vault ID"),
        header.file_guid().to_string(),
    );
    record.push(
        Column::text("type_id", "Type ID"),
        header.type_guid().to_string(),
    );
    record.push(
        Column::boolean("banking4_vault", "Banking4 data vault"),
        header.is_banking4_vault(),
    );
    record.push(
        Column::boolean("device_bound", "Device-bound"),
        header.is_device_bound(),
    );
    record.push(
        Column::text("created_at", "Created (UTC)"),
        time(header.created_at()),
    );
    record.push(
        Column::text("modified_at", "Modified (UTC)"),
        time(header.modified_at()),
    );
    // A file is far smaller than i64::MAX bytes; the format ends at 2 GiB.
    record.push(
        Column::integer("file_size", "File size (bytes)"),
        i64::try_from(file_size).ok(),
    );
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;
    use crate::output::{render_record_on, Options};

    /// A header of 512 bytes with the given fields; everything else zero.
    fn header(cipher_id: i32, rounds: i32, flags: u32, created: i32, modified: i32) -> Header {
        let mut bytes = [0u8; 512];
        bytes[..4].copy_from_slice(&[0x53, 0x55, 0x42, 0x01]);
        bytes[0x04..0x08].copy_from_slice(&4096i32.to_le_bytes());
        bytes[0x08..0x0C].copy_from_slice(&created.to_le_bytes());
        bytes[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
        bytes[0x14..0x18].copy_from_slice(&cipher_id.to_le_bytes());
        bytes[0x18..0x1C].copy_from_slice(&rounds.to_le_bytes());
        bytes[0xD4..0xD8].copy_from_slice(&modified.to_le_bytes());
        Header::parse(&bytes).unwrap()
    }

    fn render(record: &Record, format: Format) -> String {
        let options = Options {
            format,
            header: true,
            width: None,
        };
        let mut out = Vec::new();
        render_record_on(&mut out, record, &options, false).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn odd_headers_are_shown_not_refused() {
        // 01 §3.4, §3.6, §3.7, §5.2: no time, an unknown cipher, a negative
        // round count, a device-bound vault of a foreign type.
        let header = header(7, -5, 1, 0, 0);
        assert_eq!(header.modified_at().raw(), 0);
        let record = record(&header, 8192);
        assert_eq!(
            render(&record, Format::Jsonl),
            "{\"format_version\":1,\"cipher\":\"unknown (id 7)\",\"kdf_rounds\":0,\
             \"cluster_size\":4096,\"vault_id\":\"00000000-0000-0000-0000-000000000000\",\
             \"type_id\":\"00000000-0000-0000-0000-000000000000\",\"banking4_vault\":false,\
             \"device_bound\":true,\"created_at\":null,\"modified_at\":null,\"file_size\":8192}\n"
        );
        // A time that is not set is blank in the table.
        let table = render(&record, Format::Table);
        assert!(table.contains("Created (UTC):"), "{table}");
        for line in table.lines().filter(|line| line.contains("(UTC)")) {
            assert!(line.trim_end().ends_with(':'), "{line:?}");
        }
        let csv = render(&record, Format::Csv);
        assert!(csv.ends_with(",false,true,,,8192\n"), "{csv}");
    }

    #[test]
    fn times_are_utc_iso_8601() {
        // 01 §3.4: 820540800 is 2026-01-01T00:00:00Z.
        let header = header(3, 100_000, 2, 820_540_800, 820_540_801);
        assert_eq!(header.modified_at().raw(), 820_540_801);
        let json = render(&record(&header, 8192), Format::Jsonl);
        assert!(json.contains("\"cipher\":\"AES-256\",\"kdf_rounds\":100000"));
        assert!(json.contains("\"device_bound\":false"));
        assert!(json.contains(
            "\"created_at\":\"2026-01-01T00:00:00Z\",\"modified_at\":\"2026-01-01T00:00:01Z\""
        ));
    }
}
