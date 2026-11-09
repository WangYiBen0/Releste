//! Integration test: the shipped catalogs load, agree on their key sets, and
//! resolve through the documented fallback chain.
//!
//! This is the test that guards the DATA. The unit tests in src/l10n.rs guard
//! the loader's logic with synthetic catalogs; this one reads the real files a
//! translator edits, so a missing translation or a typo'd key fails CI.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use releste_editor::l10n::{discover, Locale, Localizer, CATALOG_FILE, SOURCE_LOCALE};

/// The repository's locales/ directory.
fn catalog_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/editor is two levels below the workspace root")
        .join("locales")
}

#[test]
fn every_tracked_locale_has_a_catalog() {
    let root = catalog_root();
    assert!(
        root.join(SOURCE_LOCALE).join(CATALOG_FILE).is_file(),
        "the source catalog must exist at {}",
        root.display()
    );

    let found: Vec<String> = discover(&root).iter().map(|l| l.to_string()).collect();
    assert!(
        found.contains(&SOURCE_LOCALE.to_string()),
        "discovery must find the source locale, found {found:?}"
    );
    assert!(
        found.contains(&"zh_CN".to_string()),
        "discovery must find zh_CN, found {found:?}"
    );
}

#[test]
fn source_catalog_has_no_missing_keys() {
    let root = catalog_root();
    let l = Localizer::load(&root, &Locale::source());
    assert!(l.missing_keys().is_empty());
    // A key that resolves to itself means the source catalog is incomplete.
    for key in [
        "menu.file.new",
        "menu.file.save",
        "status.ready",
        "panel.rooms",
        "panel.inspector",
        "widget.type_mismatch.int",
    ] {
        assert_ne!(l.lookup(key), key, "the source catalog is missing this key");
    }
}

#[test]
fn every_locale_has_the_same_key_set_as_the_source() {
    let root = catalog_root();
    for locale in discover(&root) {
        if locale == Locale::source() {
            continue;
        }
        let l = Localizer::load(&root, &locale);
        let missing = l.missing_keys();
        assert!(
            missing.is_empty(),
            "{locale} is missing {} key(s) present in {SOURCE_LOCALE}: {missing:?}",
            missing.len()
        );
    }
}

#[test]
fn no_locale_defines_a_key_the_source_lacks() {
    let root = catalog_root();

    let read_keys = |locale: &str| -> BTreeSet<String> {
        let path = root.join(locale).join(CATALOG_FILE);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let catalog = releste_editor::Catalog::parse(&text, Locale::parse(locale).unwrap(), &path)
            .unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));
        catalog.keys().map(str::to_string).collect()
    };

    let source = read_keys(SOURCE_LOCALE);
    for locale in discover(&root) {
        let name = locale.to_string();
        if name == SOURCE_LOCALE {
            continue;
        }
        let keys = read_keys(&name);
        let extra: Vec<&String> = keys.difference(&source).collect();
        assert!(
            extra.is_empty(),
            "{name} defines {} key(s) absent from {SOURCE_LOCALE}: {extra:?}",
            extra.len()
        );
    }
}

#[test]
fn zh_cn_actually_translates_and_keeps_placeholders() {
    let root = catalog_root();
    let l = Localizer::load(&root, &Locale::parse("zh_CN").unwrap());

    // Translated, not passed through.
    assert_ne!(l.lookup("menu.file.save"), "menu.file.save");
    assert_ne!(l.lookup("menu.file.save"), l.source_of("menu.file.save"));

    // Placeholders survive, and substitution works in a non-ASCII string.
    let out = l.format("status.room_added", &[("name", "start".to_string())]);
    assert!(out.contains("start"), "substitution failed: {out}");
    assert!(!out.contains("{name}"), "placeholder left over: {out}");

    // A non-ASCII catalog really is non-ASCII - this is the file the ASCII gate
    // is supposed to exempt (docs/i18n.md section 7).
    let text = std::fs::read_to_string(root.join("zh_CN").join(CATALOG_FILE)).unwrap();
    assert!(
        text.bytes().any(|b| b > 0x7F),
        "zh_CN must contain non-ASCII bytes"
    );
}

#[test]
fn unknown_locale_falls_back_to_the_source_catalog() {
    let root = catalog_root();
    let l = Localizer::load(&root, &Locale::parse("ja_JP").unwrap());
    assert_eq!(l.lookup("menu.file.save"), l.source_of("menu.file.save"));
}

#[test]
fn posix_style_tags_resolve_to_the_same_catalog() {
    let root = catalog_root();
    let posix = Localizer::load(&root, &Locale::parse("zh_CN.UTF-8").unwrap());
    let plain = Localizer::load(&root, &Locale::parse("zh-CN").unwrap());
    assert_eq!(
        posix.lookup("menu.file.save"),
        plain.lookup("menu.file.save")
    );
}

/// The namespaces a localisable key may use.
///
/// Scanning source for these prefixes is deliberately namespace-based rather
/// than call-syntax-based: it does not care whether a key is reached through
/// `t("..")`, `tf("..")`, `format("..")` or a value stored in a local, and
/// it keeps working when a call is split across lines.
const KEY_NAMESPACES: &[&str] = &[
    "app.",
    "menu.",
    "panel.",
    "inspector.",
    "status.",
    "widget.",
    "canvas.",
    "dialog.",
    "settings.",
    "script.",
];

/// Every localisation key referenced by the editor's Rust source.
fn keys_used_by_source() -> BTreeSet<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeSet::new();

    for entry in std::fs::read_dir(&src).expect("src/ must be readable") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // Walk every double-quoted literal; a key is a literal that starts
        // with one of the known namespaces and contains no spaces.
        for literal in text.split('"') {
            let candidate = literal.trim();
            if !KEY_NAMESPACES.iter().any(|ns| candidate.starts_with(ns)) {
                continue;
            }
            if candidate.contains(' ') || candidate.contains('\\') {
                continue;
            }
            found.insert(candidate.to_string());
        }
    }

    found
}

#[test]
fn every_key_used_by_the_source_exists_in_the_catalogs() {
    // A key used in code but absent from en_US renders as the raw key ID in
    // the UI. This test turns that runtime embarrassment into a CI failure.
    let root = catalog_root();
    let catalog = releste_editor::Catalog::parse(
        &std::fs::read_to_string(root.join(SOURCE_LOCALE).join(CATALOG_FILE)).unwrap(),
        Locale::source(),
        &root.join(SOURCE_LOCALE).join(CATALOG_FILE),
    )
    .expect("en_US catalog must parse");
    let known: BTreeSet<&str> = catalog.keys().collect();

    let mut missing = Vec::new();
    for key in keys_used_by_source() {
        if !known.contains(key.as_str()) {
            missing.push(key);
        }
    }
    assert!(
        missing.is_empty(),
        "{} key(s) used in source but absent from {SOURCE_LOCALE}: {missing:?}",
        missing.len()
    );
}

#[test]
fn no_catalog_key_is_unused() {
    // The other direction: a key nobody looks up is dead weight that makes
    // translators do pointless work. Deleting it is the fix.
    let root = catalog_root();
    let catalog = releste_editor::Catalog::parse(
        &std::fs::read_to_string(root.join(SOURCE_LOCALE).join(CATALOG_FILE)).unwrap(),
        Locale::source(),
        &root.join(SOURCE_LOCALE).join(CATALOG_FILE),
    )
    .expect("en_US catalog must parse");

    let used = keys_used_by_source();
    let dead: Vec<&str> = catalog.keys().filter(|k| !used.contains(*k)).collect();
    assert!(
        dead.is_empty(),
        "{} key(s) in {SOURCE_LOCALE} are never used by the source: {dead:?}",
        dead.len()
    );
}

#[test]
fn switching_language_live_reloads_the_catalog() {
    // The language switcher calls EditorUi::set_locale. This exercises the
    // same path without needing a window or a GPU, so the "no restart"
    // property is actually covered.
    let root = catalog_root();
    let mut ui = releste_editor::ui::EditorUi::with_locale_at(Locale::source(), root);

    let english = ui.t("menu.file.save");
    assert_eq!(english, "Save");
    assert_eq!(ui.locale().to_string(), "en_US");

    ui.set_locale(Locale::parse("zh_CN").unwrap());
    let chinese = ui.t("menu.file.save");
    assert_eq!(ui.locale().to_string(), "zh_CN");
    assert_ne!(
        chinese, english,
        "switching language must re-read the catalog, not keep the old text"
    );
    assert!(
        !chinese.is_ascii(),
        "zh_CN label should be non-ASCII, got {chinese:?}"
    );

    // Switching back restores English, proving the reload is not one-way.
    ui.set_locale(Locale::source());
    assert_eq!(ui.t("menu.file.save"), "Save");
}
