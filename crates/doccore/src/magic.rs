//! Requirement 1: BSON files that carry a fixed prefix before the first
//! document.
//!
//! Plenty of tools write a header — a magic number, a version tag, a record
//! count — ahead of the BSON payload. The reader has to find where the real
//! document stream starts, and the writer has to put those exact bytes back.
//! Nothing here interprets the prefix; it is preserved verbatim.

use std::ops::Range;

use crate::bson_read;
use crate::error::{Error, Result};

/// Largest prefix considered during auto-detection.
///
/// Real headers are a handful of bytes. Capping the search keeps a hostile file
/// from turning detection into thousands of structural validation passes.
pub const MAX_MAGIC_SCAN: usize = 256;

/// Where the document stream sits inside a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framing {
    /// Bytes before the first document, restored verbatim on save.
    pub magic_len: usize,
    /// The BSON document stream itself.
    pub body: Range<usize>,
    pub documents: usize,
    /// Bytes after the last document, also restored verbatim.
    pub trailing: Range<usize>,
}

/// Locate the document stream, honoring a user-supplied prefix length when one
/// is given.
pub fn detect(bytes: &[u8], forced: Option<usize>) -> Result<Framing> {
    match forced {
        Some(magic_len) => {
            if magic_len > bytes.len() {
                return Err(Error::bson(
                    format!(
                        "the requested {} byte prefix is longer than the {} byte file",
                        magic_len,
                        bytes.len()
                    ),
                    0,
                ));
            }
            let scan = bson_read::scan(bytes, magic_len)?;
            Ok(framing(magic_len, scan.end, scan.documents, bytes.len()))
        }
        None => detect_auto(bytes),
    }
}

fn detect_auto(bytes: &[u8]) -> Result<Framing> {
    // Fast path: the first document spans the whole file, so there is no
    // prefix. The parser validates the structure anyway, so scanning here would
    // only walk a large file twice.
    if bson_read::declares_exact_document(bytes) {
        return Ok(framing(0, bytes.len(), 1, bytes.len()));
    }

    let limit = MAX_MAGIC_SCAN.min(bytes.len().saturating_sub(bson_read::MIN_DOC_LEN));
    let mut first_error = None;
    for offset in 0..=limit {
        match bson_read::scan(bytes, offset) {
            Ok(scan) => return Ok(framing(offset, scan.end, scan.documents, bytes.len())),
            // The offset-0 failure is the one worth reporting: it explains why
            // the file was not recognized as plain BSON.
            Err(err) if offset == 0 => first_error = Some(err),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or(Error::Unrecognized))
}

fn framing(magic_len: usize, end: usize, documents: usize, total: usize) -> Framing {
    Framing { magic_len, body: magic_len..end, documents, trailing: end..total }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bson, i32, nest, string};

    fn sample() -> Vec<u8> {
        bson(&[("id", 0x10, &i32(7)), ("name", 0x02, &string("sample"))])
    }

    fn prefixed(prefix: &[u8]) -> Vec<u8> {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&sample());
        bytes
    }

    #[test]
    fn plain_bson_has_no_prefix() {
        let bytes = sample();
        let framing = detect(&bytes, None).unwrap();
        assert_eq!(framing.magic_len, 0);
        assert_eq!(framing.body, 0..bytes.len());
        assert_eq!(framing.documents, 1);
        assert!(framing.trailing.is_empty());
    }

    #[test]
    fn a_four_byte_magic_is_found() {
        let bytes = prefixed(b"\x89BSON");
        // That prefix is five bytes; the point is that the declared length at
        // offset 0 is nonsense and detection has to search.
        let framing = detect(&bytes, None).unwrap();
        assert_eq!(framing.magic_len, 5);
        assert_eq!(&bytes[..framing.magic_len], b"\x89BSON");
        assert_eq!(framing.documents, 1);
    }

    #[test]
    fn magic_of_various_lengths_is_found() {
        for len in [1usize, 2, 3, 4, 8, 16, 33, 64] {
            let prefix: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(7) | 0x80).collect();
            let bytes = prefixed(&prefix);
            let framing = detect(&bytes, None).unwrap();
            assert_eq!(framing.magic_len, len, "prefix len {len}");
            assert_eq!(&bytes[..framing.magic_len], prefix.as_slice());
        }
    }

    #[test]
    fn a_document_stream_after_the_magic_is_counted() {
        let mut bytes = b"MAGIC".to_vec();
        bytes.extend_from_slice(&sample());
        bytes.extend_from_slice(&bson(&[("other", 0x08, &[1])]));
        let framing = detect(&bytes, None).unwrap();
        assert_eq!(framing.magic_len, 5);
        assert_eq!(framing.documents, 2);
        assert_eq!(framing.body.end, bytes.len());
    }

    #[test]
    fn trailing_bytes_are_kept_out_of_the_body() {
        let mut bytes = prefixed(b"\xAB\xCD");
        let body_end = bytes.len();
        bytes.extend_from_slice(b"checksum");
        let framing = detect(&bytes, None).unwrap();
        assert_eq!(framing.magic_len, 2);
        assert_eq!(framing.body, 2..body_end);
        assert_eq!(framing.trailing, body_end..bytes.len());
        assert_eq!(&bytes[framing.trailing.clone()], b"checksum");
    }

    #[test]
    fn a_forced_prefix_length_overrides_detection() {
        let bytes = prefixed(b"MAGIC");
        let framing = detect(&bytes, Some(5)).unwrap();
        assert_eq!(framing.magic_len, 5);
        assert_eq!(framing.documents, 1);
    }

    #[test]
    fn forcing_a_prefix_that_is_too_long_is_refused() {
        let bytes = sample();
        let err = detect(&bytes, Some(bytes.len() + 1)).unwrap_err();
        assert!(matches!(err, Error::Bson { .. }), "{err:?}");
        assert!(err.to_string().contains("longer than"));
    }

    #[test]
    fn forcing_a_prefix_where_no_document_follows_is_refused() {
        let bytes = prefixed(b"MAGIC");
        assert!(detect(&bytes, Some(2)).is_err());
    }

    #[test]
    fn detection_never_searches_past_the_cap() {
        // A prefix longer than MAX_MAGIC_SCAN is not auto-detected; the user
        // has to state the length. This bounds detection cost on hostile input.
        let long = vec![0xFFu8; MAX_MAGIC_SCAN + 40];
        let bytes = prefixed(&long);
        assert!(detect(&bytes, None).is_err());
        assert!(detect(&bytes, Some(long.len())).is_ok());
    }

    #[test]
    fn non_bson_input_is_rejected_with_a_useful_error() {
        let err = detect(b"this is not bson", None).unwrap_err();
        assert!(matches!(err, Error::Bson { .. }), "{err:?}");
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(detect(b"", None).is_err());
        assert!(detect(b"", Some(0)).is_err());
    }

    #[test]
    fn a_deeply_nested_document_is_still_detected_behind_a_prefix() {
        let bytes = {
            let mut v = b"HDR".to_vec();
            v.extend_from_slice(&nest(50));
            v
        };
        let framing = detect(&bytes, None).unwrap();
        assert_eq!(framing.magic_len, 3);
        assert_eq!(framing.documents, 1);
    }
}
