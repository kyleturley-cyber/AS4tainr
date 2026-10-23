//! QSYSINC, the system include library: source members with the
//! structures and prototypes of the system APIs for programs to /COPY
//! (`/COPY QSYSINC/QRPGLESRC,QUSEC`). IBM ships it as an option of the
//! operating system; as4tainr's members are its own, written from the API
//! documentation (the formats' fields and the names programs use for
//! them), and made at catalog initialization.

use anyhow::Result;
use tokio_postgres::Client;

/// (source file, member, source type, text, source).
const MEMBERS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "QRPGLESRC",
        "QUSEC",
        "RPGLE",
        "Error code parameter",
        // The error code parameter, format ERRC0100 (API documentation,
        // "Error code parameter"): bytes provided (binary 4), bytes available
        // (binary 4), exception ID (char 7), reserved (char 1); the exception
        // data follows in the caller's own storage.
        "      * QUSEC: the error code parameter of the system APIs (format
      * ERRC0100: bytes provided, bytes available, exception ID, reserved).
     DQUSEC            DS
     D QUSBPRV                 1      4B 0
     D QUSBAVL                 5      8B 0
     D QUSEI                   9     15
     D QUSERVED               16     16
",
    ),
    (
        "QRPGLESRC",
        "QWCRNETA",
        "RPGLE",
        "Retrieve Network Attributes",
        // One attribute's entry in the receiver (API documentation,
        // "Retrieve Network Attributes (QWCRNETA)"): attribute name
        // (char 10), type of data (char 1), information status (char 1),
        // length of data (binary 4), then the data. The names are the
        // ones programs use (OSSILE's NSTATR: QWCRAT, QWCNA, QWCLD).
        "      * QWCRNETA: an attribute entry of the Retrieve Network Attributes
      * API's receiver (name, type of data, information status, length).
     DQWCRAT           DS
     D QWCNA                   1     10
     D QWCLD                  13     16B 0
",
    ),
];

/// Make QSYSINC and its members (again: they follow this version).
pub async fn init(client: &Client) -> Result<()> {
    crate::catalog::create_library(client, "QSYSINC", "System openness includes").await?;
    for (file, member, srctype, text, source) in MEMBERS {
        if !crate::source::is_source_file(client, "QSYSINC", file).await? {
            crate::source::create_file(client, "QSYSINC", file, "").await?;
        }
        crate::source::put_member(
            client,
            &crate::source::Member {
                library: "QSYSINC".into(),
                file: (*file).into(),
                name: (*member).into(),
                srctype: (*srctype).into(),
                text: (*text).into(),
                source: (*source).into(),
                ccsid: 37,
                ..Default::default()
            },
            true,
        )
        .await?;
    }
    Ok(())
}
