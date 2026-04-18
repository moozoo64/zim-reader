//! A pure-Rust, read-only library for [ZIM archive files].
//!
//! Phase 1 scope: open a ZIM file, parse the 80-byte header, read the MIME
//! type list, and detect the archive's namespace convention. Directory-entry
//! parsing, binary search, cluster decompression, and checksum verification
//! are implemented in subsequent phases.
//!
//! # Quick start
//!
//! ```no_run
//! use zim_reader::Archive;
//!
//! let archive = Archive::open("wikipedia.zim")?;
//! println!("version {}.{}", archive.header().major_version, archive.header().minor_version);
//! println!("{} MIME types, {} entries, {} clusters",
//!     archive.mime_types().len(),
//!     archive.entry_count(),
//!     archive.cluster_count(),
//! );
//! # Ok::<(), zim_reader::Error>(())
//! ```
//!
//! [ZIM archive files]: https://wiki.openzim.org/wiki/ZIM_file_format

mod archive;
mod dirent;
mod error;
mod header;
mod mime;
mod namespace;
mod pointer_list;
mod util;

pub use archive::{Archive, ArchiveOptions, ArticleIter, EntryIter, VerifyChecksum};
pub use dirent::{ContentEntry, Dirent, RedirectEntry};
pub use error::{Error, Result};
pub use header::Header;
pub use namespace::{article_namespace, metadata_namespace, Namespace, NamespaceMode};
