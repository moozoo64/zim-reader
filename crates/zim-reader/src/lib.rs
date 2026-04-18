//! A pure-Rust, read-only library for [ZIM archive files] — the offline
//! content format used by [Kiwix] for Wikipedia, Wiktionary, Stack Exchange,
//! and similar wiki-style corpora.
//!
//! An [`Archive`] is opened by memory-mapping a `.zim` file. Parsing is
//! lazy: header, MIME list, and namespace mode are resolved at open time;
//! dirents, clusters, and blobs are read on demand. Decompressed clusters
//! are cached in a fixed-size LRU.
//!
//! # Quick start
//!
//! ```no_run
//! use zim_reader::Archive;
//!
//! let archive = Archive::open("wikipedia.zim")?;
//! if let Some(article) = archive.get_article("A/Rust_(programming_language)")? {
//!     println!("{} bytes, mime: {}", article.data.len(), article.mime_type(&archive));
//! }
//! # Ok::<(), zim_reader::Error>(())
//! ```
//!
//! # Features
//!
//! - `compression-pure` (default): pure-Rust LZMA2 and Zstandard decoders
//!   via `lzma-rs` and `ruzstd`.
//!
//! [ZIM archive files]: https://wiki.openzim.org/wiki/ZIM_file_format
//! [Kiwix]: https://www.kiwix.org/

mod archive;
mod article;
mod cluster;
mod dirent;
mod error;
mod header;
mod mime;
mod namespace;
mod pointer_list;
mod util;

pub use archive::{Archive, ArchiveOptions, ArticleIter, EntryIter, VerifyChecksum};
pub use article::Article;
pub use dirent::{ContentEntry, Dirent, RedirectEntry};
pub use error::{Error, Result};
pub use header::Header;
pub use namespace::{article_namespace, metadata_namespace, Namespace, NamespaceMode};
