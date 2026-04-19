//! Error-path tests against `invalid.*.zim` fixtures from the
//! `openzim/zim-testing-suite` submodule.
//!
//! Each test opens (or attempts to open) a deliberately malformed archive
//! and asserts that the library fails gracefully — returning `Err` rather
//! than panicking, and ideally returning a meaningful `Error` variant.
//!
//! Fixtures are skipped cleanly if the submodule is not initialised
//! (see `common::with_fixture`).

mod common;

use std::path::Path;

use common::with_fixture;
use zim_reader::{Archive, ArchiveOptions, Dirent, Error, VerifyChecksum};

/// Open an archive with checksum verification disabled. The invalid
/// fixtures from zim-testing-suite are modified in place without updating
/// the stored MD5, so default-opened archives would fail with
/// `ChecksumMismatch` before ever exercising the deeper code paths we
/// want to test.
fn open_skip_checksum(p: &Path) -> zim_reader::Result<Archive> {
    let mut opts = ArchiveOptions::default();
    opts.verify_checksum = VerifyChecksum::Skip;
    Archive::open_with_options(p, opts)
}

/// File is smaller than the 80-byte header. `Archive::open` must return an
/// error at the header-parse step, not panic.
#[test]
fn errors_on_smaller_than_header() {
    with_fixture(
        "data/withns/invalid.smaller_than_header.zim",
        |p| match open_skip_checksum(&p) {
            Err(Error::TruncatedHeader(_) | Error::Io(_) | Error::InvalidMagic(_)) => {}
            Err(e) => panic!("unexpected error variant: {e:?}"),
            Ok(_) => panic!("expected open to fail"),
        },
    );
}

/// The `path_ptr_pos` in the header points outside the file. Reading any
/// dirent via the path pointer list must fail with `OffsetOutOfBounds`
/// (from one of the pointer-list reads) rather than panic.
#[test]
fn errors_on_outofbounds_first_direntptr() {
    with_fixture("data/withns/invalid.outofbounds_first_direntptr.zim", |p| {
        // Open may succeed (header is self-consistent); error surfaces
        // when we dereference a dirent pointer.
        let archive = match open_skip_checksum(&p) {
            Ok(a) => a,
            Err(e) => {
                // Acceptable: fail at open if bounds are checked eagerly.
                assert!(
                    matches!(e, Error::OffsetOutOfBounds { .. }),
                    "open failed with unexpected variant: {e:?}",
                );
                return;
            }
        };
        let any_err = archive.entries().any(|r| r.is_err());
        let any_find_err = archive.find_by_path(None, "nonexistent").is_err();
        assert!(
            any_err || any_find_err,
            "expected an error during entry iteration or path lookup on a \
             fixture with an out-of-bounds dirent pointer",
        );
    });
}

/// A cluster's first-blob offset is not a multiple of the offset size —
/// the cluster blob-offset table is malformed. Fetching a blob from that
/// cluster must fail with `OffsetOutOfBounds { field: "cluster_first_offset" }`.
#[test]
fn errors_on_misaligned_blob_offset() {
    with_fixture(
        "data/withns/invalid.misaligned_offset_of_first_blob_in_cluster_10.zim",
        |p| {
            let archive = open_skip_checksum(&p).expect("header parses cleanly");
            let saw_misalignment = archive.entries().flatten().any(|d| {
                if let Dirent::Content(entry) = d {
                    matches!(
                        archive.get_blob(&entry),
                        Err(Error::OffsetOutOfBounds {
                            field: "cluster_first_offset",
                            ..
                        }),
                    )
                } else {
                    false
                }
            });
            assert!(
                saw_misalignment,
                "expected Error::OffsetOutOfBounds {{ field: \"cluster_first_offset\" }} \
                 from some blob read on a misaligned-offset fixture",
            );
        },
    );
}

/// A blob-end offset lies outside the decompressed cluster. Fetching a
/// blob must fail with `OffsetOutOfBounds` rather than panic or return
/// garbage.
#[test]
fn errors_on_offset_in_cluster() {
    with_fixture("data/withns/invalid.offset_in_cluster.zim", |p| {
        let archive = open_skip_checksum(&p).expect("header parses cleanly");
        let saw_err = archive.entries().flatten().any(|d| {
            if let Dirent::Content(entry) = d {
                matches!(
                    archive.get_blob(&entry),
                    Err(Error::OffsetOutOfBounds { .. }),
                )
            } else {
                false
            }
        });
        assert!(
            saw_err,
            "expected at least one blob fetch to fail with an \
             OffsetOutOfBounds error on a bad-cluster-offset fixture",
        );
    });
}

/// A dirent references a MIME-type index that does not exist in the MIME
/// list. Parsing the dirent itself doesn't care about this — the index is
/// just stored — so the failure (if any) surfaces when code consults the
/// MIME table. We only assert that iteration does not panic.
#[test]
fn does_not_panic_on_bad_mimetype_in_dirent() {
    with_fixture("data/withns/invalid.bad_mimetype_in_dirent.zim", |p| {
        let archive = open_skip_checksum(&p).expect("header parses cleanly");
        // Consume all entries. Must not panic.
        let _: Vec<_> = archive.entries().collect();
        // Try a couple of paths — must not panic either.
        let _ = archive.find_by_path(None, "A/a");
        let _ = archive.find_by_path(None, "does_not_exist");
    });
}

/// The dirent table is not sorted by path. Our binary search *may* return
/// the wrong entry or `None`, but it must not panic or loop forever.
#[test]
fn does_not_panic_on_nonsorted_dirent_table() {
    with_fixture("data/withns/invalid.nonsorted_dirent_table.zim", |p| {
        let archive = open_skip_checksum(&p).expect("header parses cleanly");
        for probe in ["a", "z", "main", "does_not_exist", ""] {
            let _ = archive.find_by_path(None, probe);
            let _ = archive.find_by_title(None, probe);
        }
        // And iteration must terminate.
        let count = archive.entries().count();
        assert_eq!(count as u32, archive.entry_count());
    });
}
