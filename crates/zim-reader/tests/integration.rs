mod common;

use common::with_fixture;
use zim_reader::{Archive, NamespaceMode, article_namespace};

#[test]
fn test_open_v5_small() {
    with_fixture("data/withns/small.zim", |p| {
        let a = Archive::open(&p).expect("open v5 small");
        assert_eq!(a.header().major_version, 5);
        assert_eq!(a.namespace_mode(), NamespaceMode::Legacy);
        assert!(a.entry_count() > 0);
        assert!(a.cluster_count() > 0);
        assert!(!a.mime_types().is_empty());
    });
}

#[test]
fn test_open_v6_small() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).expect("open v6 small");
        assert_eq!(a.header().major_version, 6);
        assert_eq!(a.namespace_mode(), NamespaceMode::New);
        assert!(a.entry_count() > 0);
    });
}

#[test]
fn test_entries_iterate_v6() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        let entries: Vec<_> = a
            .entries()
            .collect::<Result<Vec<_>, _>>()
            .expect("entry iteration error");
        assert_eq!(entries.len() as u32, a.entry_count());
    });
}

#[test]
fn test_find_any_entry_round_trip_v5() {
    with_fixture("data/withns/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        let entry = a
            .articles()
            .filter_map(Result::ok)
            .next()
            .expect("at least one article entry");
        let found = a
            .find_by_path(Some(entry.namespace), &entry.path)
            .unwrap()
            .expect("found by path");
        assert_eq!(found.path(), entry.path);
        assert_eq!(found.namespace(), entry.namespace);
    });
}

#[test]
fn test_find_any_entry_round_trip_v6() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        let ns = article_namespace(a.namespace_mode());
        assert_eq!(ns, 'C');
        let entry = a
            .articles()
            .filter_map(Result::ok)
            .next()
            .expect("at least one article entry");
        let found = a.find_by_path(Some(ns), &entry.path).unwrap();
        assert!(found.is_some());
    });
}

#[test]
fn test_get_article_v6() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        let path = a
            .articles()
            .filter_map(Result::ok)
            .next()
            .map(|e| e.path)
            .expect("content entry");
        let article = a.get_article(&path).unwrap().expect("article exists");
        assert!(!article.data.is_empty());
        let _mime = article.mime_type(&a);
    });
}

#[test]
fn test_main_page_v6() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        if let Some(mp) = a.main_page().unwrap() {
            assert!(!mp.data.is_empty());
        }
    });
}

#[test]
fn test_metadata_title() {
    with_fixture("data/nons/small.zim", |p| {
        let a = Archive::open(&p).unwrap();
        if let Some(t) = a.metadata("Title").unwrap() {
            assert!(!t.trim().is_empty());
        }
    });
}
