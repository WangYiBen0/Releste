//! Localisation (l10n) for the map editor.
//!
//! # Why this exists in the editor crate
//!
//! AGENTS.md 15.1 forbids new crates and new dependencies, so l10n is a module,
//! not a crate. It has no egui dependency, so it is unit-testable headlessly and
//! reusable by the upcoming `releste edit` CLI subcommand (ROADMAP Appendix B).
//!
//! # Catalogs are runtime data
//!
//! AGENTS.md 5.4 states that every tracked file is ASCII-only and that
//! "localisation data is the only exception, and it does not live in the source
//! tree". The catalogs therefore live in `locales/<locale>/editor.toml` at the
//! repository root and are read from disk at startup - never embedded with
//! `include_str!`.
//!
//! The repository's ASCII gate (`nix/scripts/releste-lint-files.sh`, mode
//! `ascii`) already exempts any path matching `*l10n*`, `*i18n*`, `*locale*`,
//! `*.po` or `*.ftl`, so `locales/zh_CN/editor.toml` is excluded from the
//! ASCII-only rule automatically. See `CONTRIBUTING-i18n.md`.
//!
//! # Fallback chain
//!
//! For a key `k` under the requested locale `L`:
//!
//! 1. `L` itself (`zh_CN`)
//! 2. `en_US`, the project's source language
//! 3. the key itself, so a missing translation is *visible* in the UI
//!
//! There is deliberately no bare-language tier: `zh_CN` does not inherit from a
//! `zh` catalog. Key-set parity is enforced for every tracked catalog
//! (`tests/catalogs.rs`), so a partial parent catalog could never legally
//! exist and the tier would be dead code. See `docs/i18n.md` section 4.
//!
//! Step 3 is deliberate: a blank label hides the defect, a `menu.file.new`
//! label reports it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::Error as _;
use thiserror::Error;

/// The project's source language. Every catalog falls back to this one.
pub const SOURCE_LOCALE: &str = "en_US";

/// File name of a catalog inside a locale directory.
pub const CATALOG_FILE: &str = "editor.toml";

/// l10n error.
///
/// Only *fatal* configuration problems are errors. A missing or malformed
/// catalog is not fatal - it degrades to the fallback chain with a
/// `tracing::warn!` (AGENTS.md 15.9).
#[derive(Debug, Error)]
pub enum L10nError {
    /// The catalog file could not be read.
    #[error("failed to read catalog {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The catalog file is not valid TOML, or has the wrong shape.
    #[error("failed to parse catalog {path}: {source}")]
    Parse {
        /// Path that failed.
        path: PathBuf,
        /// Underlying error.
        source: toml::de::Error,
    },
    /// The locale tag could not be understood.
    #[error("invalid locale tag `{0}`")]
    InvalidLocale(String),
}

/// A BCP-47-ish locale tag, normalised for lookup.
///
/// The editor accepts the spellings users actually have in their environment:
/// `en_US`, `en-US`, `en_US.UTF-8`, `zh_CN`, `zh-CN`, `zh_CN.UTF-8` and
/// `zh-Hans-CN`. All of them normalise to `language_REGION`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Locale {
    /// Lowercase ISO-639 language, for example `en`.
    language: String,
    /// Uppercase ISO-3166 region, empty when the tag has none.
    region: String,
}

impl Locale {
    /// Parses a locale tag.
    ///
    /// Returns an error only when the tag has no usable language subtag.
    pub fn parse(tag: &str) -> Result<Self, L10nError> {
        let raw = tag.trim();
        if raw.is_empty() {
            return Err(L10nError::InvalidLocale(tag.to_string()));
        }

        // Drop the codeset: "en_US.UTF-8" -> "en_US".
        let no_codeset = raw.split('.').next().unwrap_or(raw);
        // Drop the modifier: "en_US@euro" -> "en_US".
        let no_modifier = no_codeset.split('@').next().unwrap_or(no_codeset);
        // POSIX uses '_', BCP-47 uses '-'; treat both as separators.
        let unified = no_modifier.replace('_', "-");

        let mut parts = unified.split('-').filter(|p| !p.is_empty());
        let language = parts
            .next()
            .ok_or_else(|| L10nError::InvalidLocale(tag.to_string()))?;
        if !language.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(L10nError::InvalidLocale(tag.to_string()));
        }

        // Pick the region out of the remaining subtags. Script subtags
        // ("Hans", "Latn") are four letters and are skipped; the region is a
        // two-letter or three-digit subtag.
        let mut region = String::new();
        for part in parts {
            let is_region = part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic())
                || part.len() == 3 && part.chars().all(|c| c.is_ascii_digit());
            if is_region {
                region = part.to_ascii_uppercase();
                break;
            }
        }

        Ok(Locale {
            language: language.to_ascii_lowercase(),
            region,
        })
    }

    /// The lowercase language subtag, for example `zh`.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The uppercase region subtag, empty when absent, for example `CN`.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// The source locale, `en_US`.
    pub fn source() -> Self {
        Locale {
            language: "en".to_string(),
            region: "US".to_string(),
        }
    }

    /// The catalogue directory name for this locale, for example `zh_CN`.
    ///
    /// This is the POSIX spelling so the on-disk layout matches what a
    /// translator sees in `LANG`.
    pub fn dir_name(&self) -> String {
        if self.region.is_empty() {
            self.language.clone()
        } else {
            format!("{}_{}", self.language, self.region)
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::source()
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.dir_name())
    }
}

impl std::str::FromStr for Locale {
    type Err = L10nError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Locale::parse(s)
    }
}

/// One catalog file, already flattened to `key -> text`.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    locale: Locale,
    entries: BTreeMap<String, String>,
}

impl Catalog {
    /// Creates an empty catalog for a locale.
    pub fn empty(locale: Locale) -> Self {
        Catalog {
            locale,
            entries: BTreeMap::new(),
        }
    }

    /// Builds a catalog from a flat key/value map.
    pub fn from_entries(locale: Locale, entries: BTreeMap<String, String>) -> Self {
        Catalog { locale, entries }
    }

    /// Loads `<root>/<locale dir>/editor.toml`.
    ///
    /// A missing or malformed file is logged and reported as `None`; callers
    /// keep the previous catalog and the fallback chain still resolves.
    pub fn load(root: &Path, locale: &Locale) -> Option<Self> {
        let path = root.join(locale.dir_name()).join(CATALOG_FILE);
        match Self::load_file(&path, locale.clone()) {
            Ok(catalog) => Some(catalog),
            Err(L10nError::Io { .. }) => {
                tracing::debug!(
                    path = %path.display(),
                    "locale catalog not present; falling back"
                );
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "locale catalog unusable; falling back");
                None
            }
        }
    }

    /// Loads one catalog file.
    fn load_file(path: &Path, locale: Locale) -> Result<Self, L10nError> {
        let text = std::fs::read_to_string(path).map_err(|source| L10nError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, locale, path)
    }

    /// Parses catalog text.
    ///
    /// The accepted shape is a flat table of strings:
    ///
    /// ```toml
    /// [editor]
    /// "menu.file" = "File"
    /// ```
    ///
    /// A top-level `[editor]` table is expected but a flat file is also
    /// accepted, so hand-written catalogs do not need the wrapper.
    pub fn parse(text: &str, locale: Locale, path: &Path) -> Result<Self, L10nError> {
        let value: toml::Value = toml::from_str(text).map_err(|source| L10nError::Parse {
            path: path.to_path_buf(),
            source,
        })?;

        let table = match value.get("editor") {
            Some(toml::Value::Table(t)) => t.clone(),
            Some(other) => {
                return Err(L10nError::Parse {
                    path: path.to_path_buf(),
                    source: toml::de::Error::custom(format!(
                        "`editor` must be a table, found {} (see docs/i18n.md)",
                        other.type_str()
                    )),
                })
            }
            None => match value {
                toml::Value::Table(t) => t,
                _ => toml::map::Map::new(),
            },
        };

        let mut entries = BTreeMap::new();
        for (key, val) in table {
            match val {
                toml::Value::String(s) => {
                    entries.insert(key, s);
                }
                other => {
                    tracing::warn!(
                        path = %path.display(),
                        key = %key,
                        kind = other.type_str(),
                        "catalog entry is not a string; ignored"
                    );
                }
            }
        }

        Ok(Catalog { locale, entries })
    }

    /// The locale this catalog was loaded for.
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    /// Direct lookup, without any fallback.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every key, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// The set of loaded catalogs plus the active locale.
///
/// Holds the requested locale's catalog (when it exists) and the source
/// catalog. Lookup walks them in that order and finally returns the key
/// itself. There is deliberately no parent-locale resolution: key-set parity is
/// enforced for every tracked catalog (docs/i18n.md 5.1), so a partial parent
/// catalog would have nothing to contribute.
pub struct Localizer {
    requested: Locale,
    source: Catalog,
    requested_catalog: Option<Catalog>,
}

impl Localizer {
    /// Builds a localizer for `requested`, loading catalogs from `root`.
    ///
    /// Never fails: an unreadable root simply means every lookup returns the
    /// key itself.
    pub fn load(root: &Path, requested: &Locale) -> Self {
        let requested_catalog = Catalog::load(root, requested);

        // An unknown locale is not fatal, but it must not pass silently: the
        // user asked for a language and is about to get English (AGENTS.md 15.9).
        if requested_catalog.is_none() && *requested != Locale::source() {
            tracing::warn!(
                locale = %requested,
                root = %root.display(),
                "no catalog for this language; falling back to the source locale"
            );
        }

        let source = Catalog::load(root, &Locale::source()).unwrap_or_else(|| {
            tracing::warn!(
                root = %root.display(),
                "source locale catalog missing; untranslated keys will show their key"
            );
            Catalog::empty(Locale::source())
        });

        Localizer {
            requested: requested.clone(),
            source,
            requested_catalog,
        }
    }

    /// Builds a localizer from catalogs already in memory (tests, embedders).
    pub fn from_catalogs(
        requested: Locale,
        requested_catalog: Option<Catalog>,
        source: Catalog,
    ) -> Self {
        Localizer {
            requested,
            source,
            requested_catalog,
        }
    }

    /// The active locale.
    pub fn locale(&self) -> &Locale {
        &self.requested
    }

    /// Resolves a key through the fallback chain.
    ///
    /// Order: requested locale -> `en_US` -> the key itself (docs/i18n.md 4).
    pub fn lookup<'a>(&'a self, key: &'a str) -> &'a str {
        if let Some(c) = &self.requested_catalog {
            if let Some(v) = c.get(key) {
                return v;
            }
        }
        if let Some(v) = self.source.get(key) {
            return v;
        }
        // Visible failure: the key resolves to itself (documented in
        // docs/i18n.md section 4), and the log names it.
        tracing::warn!(key, "no catalog entry in any locale; showing the key");
        key
    }

    /// Resolves a key and substitutes `{name}` placeholders.
    ///
    /// Placeholders with no matching argument are left intact rather than
    /// being blanked, so a translator can see what went wrong.
    pub fn format(&self, key: &str, args: &[(&str, String)]) -> String {
        substitute(self.lookup(key), args)
    }

    /// Looks a key up in the source catalog only.
    ///
    /// Used by the diagnostics that compare a translation against its source.
    pub fn source_of<'a>(&'a self, key: &'a str) -> &'a str {
        self.source.get(key).unwrap_or(key)
    }

    /// Keys present in `en_US` but missing from the active locale.
    ///
    /// Returns an empty list when the active locale *is* the source locale.
    pub fn missing_keys(&self) -> Vec<&str> {
        if self.requested == Locale::source() {
            return Vec::new();
        }
        // No catalog for the active locale at all: every key resolves through
        // the `en_US` tier, so parity is not measurable. `Localizer::load`
        // already warns about the missing catalog, so the signal is not lost.
        let Some(requested) = self.requested_catalog.as_ref() else {
            return Vec::new();
        };
        self.source
            .keys()
            .filter(|k| requested.get(k).is_none())
            .collect()
    }
}

/// Substitutes `{name}` placeholders in `text`.
///
/// Unknown placeholders are preserved verbatim. A literal brace can be escaped
/// by doubling it: `{{name}}` renders as `{name}`.
pub fn substitute(text: &str, args: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(open) = rest.find(['{', '}']) {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];

        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push(tail.chars().next().unwrap_or('{'));
            rest = &tail[2..];
            continue;
        }

        if !tail.starts_with('{') {
            // A stray '}': keep it, it is not a placeholder.
            out.push('}');
            rest = &tail[1..];
            continue;
        }

        let Some(close) = tail.find('}') else {
            // Unterminated '{': keep the remainder verbatim.
            out.push_str(tail);
            return out;
        };

        let name = &tail[1..close];
        match args.iter().find(|(n, _)| *n == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&tail[..=close]),
        }
        rest = &tail[close + 1..];
    }

    out.push_str(rest);
    out
}

/// Discovers locale directories under a catalog root.
///
/// A directory counts as a locale when it contains `editor.toml` and its name
/// parses as a locale tag.
pub fn discover(root: &Path) -> Vec<Locale> {
    let Ok(entries) = std::fs::read_dir(root) else {
        tracing::debug!(root = %root.display(), "catalog root not readable");
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(CATALOG_FILE).is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        match Locale::parse(name) {
            Ok(locale) => found.push(locale),
            Err(_) => {
                tracing::warn!(dir = %path.display(), "locale directory name is not a locale tag");
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

/// Finds the catalog root for a process.
///
/// Tries, in order:
///
/// 1. `RELESTE_LOCALE_DIR` (explicit override, used by tests and packaging)
/// 2. `<exe dir>/locales` (the shipped layout, ROADMAP Appendix C)
/// 3. `./locales` relative to the working directory
/// 4. `CARGO_MANIFEST_DIR/../../locales` (a `cargo run` from the checkout)
///
/// A candidate only counts when it actually contains at least one locale
/// catalog. Testing mere directory existence would let a stray empty
/// `./locales/` shadow the checkout's real `locales/`, silently dropping every
/// translation.
///
/// Returns `None` when no candidate holds a catalog; the caller then keeps the
/// key-as-text fallback rather than failing to start.
pub fn find_catalog_root() -> Option<PathBuf> {
    catalog_root_candidates()
        .into_iter()
        .find(|candidate| holds_a_catalog(candidate))
}

/// Whether a directory contains at least one locale catalog.
///
/// Split out from find_catalog_root so the predicate can be tested without
/// depending on which directories happen to exist next to the test binary.
fn holds_a_catalog(dir: &Path) -> bool {
    !discover(dir).is_empty()
}

/// The candidate catalog roots, in precedence order.
fn catalog_root_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(dir) = std::env::var("RELESTE_LOCALE_DIR") {
        if !dir.is_empty() {
            out.push(PathBuf::from(dir));
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            out.push(parent.join("locales"));
        }
    }

    out.push(PathBuf::from("locales"));

    // <workspace>/crates/editor/src/../../.. = <workspace>
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest.parent().and_then(Path::parent) {
        out.push(workspace.join("locales"));
    }

    out
}

/// Picks the locale to start in, from the CLI, the environment or the source.
///
/// Precedence: explicit flag -> `RELESTE_LANG` -> `LC_ALL` -> `LC_MESSAGES` ->
/// `LANG` -> `en_US`. Unparseable values are ignored with a warning; a common
/// POSIX value like `C` or `POSIX` simply falls through to `en_US`.
pub fn locale_from_env(explicit: Option<&str>) -> Locale {
    parse_locale_preference(explicit, |var| std::env::var(var).ok())
}

/// The environment-independent core of [`locale_from_env`].
///
/// `get` is injected so the precedence rules are testable without mutating
/// the process environment: `std::env::set_var` is unsafe in edition 2024 and
/// would make the tests order-dependent.
fn parse_locale_preference(explicit: Option<&str>, get: impl Fn(&str) -> Option<String>) -> Locale {
    if let Some(tag) = explicit {
        match Locale::parse(tag) {
            Ok(l) => return l,
            Err(_) => tracing::warn!(tag, "--lang is not a locale tag; using the environment"),
        }
    }

    for var in ["RELESTE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(value) = get(var) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value == "C" || value == "POSIX" {
            continue;
        }
        match Locale::parse(value) {
            Ok(l) => return l,
            Err(_) => tracing::debug!(var, value, "environment locale not understood"),
        }
    }

    Locale::source()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("releste-l10n-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---------- Locale parsing ----------

    #[test]
    fn parses_posix_and_bcp47_tags() {
        for tag in [
            "en_US",
            "en-US",
            "en_US.UTF-8",
            "zh_CN",
            "zh-CN",
            "zh_CN.UTF-8",
        ] {
            let l = Locale::parse(tag).unwrap();
            assert_eq!(
                l.language(),
                tag.split(['_', '-']).next().unwrap().to_ascii_lowercase()
            );
            assert_eq!(l.region().len(), 2, "tag {tag} should carry a region");
        }
    }

    #[test]
    fn parses_script_subtag_and_keeps_region() {
        let l = Locale::parse("zh-Hans-CN").unwrap();
        assert_eq!(l.language(), "zh");
        assert_eq!(l.region(), "CN");
        assert_eq!(l.dir_name(), "zh_CN");
    }

    #[test]
    fn normalises_case_and_separator() {
        assert_eq!(
            Locale::parse("ZH_cn").unwrap(),
            Locale::parse("zh-CN").unwrap()
        );
        assert_eq!(Locale::parse("zh_cn.utf-8").unwrap().to_string(), "zh_CN");
    }

    #[test]
    fn language_only_tag_has_no_region() {
        let l = Locale::parse("de").unwrap();
        assert_eq!(l.region(), "");
        assert_eq!(l.dir_name(), "de");
    }

    #[test]
    fn rejects_empty_and_non_alphabetic_language() {
        assert!(Locale::parse("").is_err());
        assert!(Locale::parse("   ").is_err());
        assert!(Locale::parse("123").is_err());
    }

    #[test]
    fn default_locale_is_the_source_locale() {
        assert_eq!(Locale::default(), Locale::source());
        assert_eq!(Locale::default().to_string(), SOURCE_LOCALE);
    }

    #[test]
    fn from_str_matches_parse() {
        let a: Locale = "zh_CN".parse().unwrap();
        assert_eq!(a, Locale::parse("zh_CN").unwrap());
    }

    // ---------- Catalog parsing ----------

    #[test]
    fn parses_editor_table_shape() {
        let text = "[editor]\n\"menu.file\" = \"File\"\n";
        let c = Catalog::parse(text, Locale::source(), Path::new("mem")).unwrap();
        assert_eq!(c.get("menu.file"), Some("File"));
    }

    #[test]
    fn parses_flat_shape_without_wrapper() {
        let c = Catalog::parse(
            "\"menu.file\" = \"File\"\n",
            Locale::source(),
            Path::new("mem"),
        )
        .unwrap();
        assert_eq!(c.get("menu.file"), Some("File"));
    }

    #[test]
    fn non_string_entries_are_skipped_not_fatal() {
        let text = "[editor]\n\"a\" = \"ok\"\n\"b\" = 3\n";
        let c = Catalog::parse(text, Locale::source(), Path::new("mem")).unwrap();
        assert_eq!(c.get("a"), Some("ok"));
        assert_eq!(c.get("b"), None);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn malformed_toml_is_an_error_and_never_panics() {
        let err = Catalog::parse("this is not = = toml", Locale::source(), Path::new("mem"));
        assert!(err.is_err());
    }

    // ---------- Fallback chain ----------

    fn catalog(locale: Locale, pairs: &[(&str, &str)]) -> Catalog {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), (*v).to_string());
        }
        Catalog::from_entries(locale, m)
    }

    #[test]
    fn requested_locale_wins() {
        let l = Localizer::from_catalogs(
            Locale::parse("zh_CN").unwrap(),
            Some(catalog(Locale::parse("zh_CN").unwrap(), &[("a", "A-zh")])),
            catalog(Locale::source(), &[("a", "A-en")]),
        );
        assert_eq!(l.lookup("a"), "A-zh");
    }

    #[test]
    fn falls_back_to_source_locale() {
        let l = Localizer::from_catalogs(
            Locale::parse("zh_CN").unwrap(),
            Some(catalog(Locale::parse("zh_CN").unwrap(), &[])),
            catalog(Locale::source(), &[("a", "A-en")]),
        );
        assert_eq!(l.lookup("a"), "A-en");
    }

    #[test]
    fn falls_back_to_the_key_itself() {
        let l = Localizer::from_catalogs(
            Locale::parse("zh_CN").unwrap(),
            Some(catalog(Locale::parse("zh_CN").unwrap(), &[])),
            catalog(Locale::source(), &[]),
        );
        assert_eq!(l.lookup("menu.file.new"), "menu.file.new");
    }

    #[test]
    fn missing_catalog_still_resolves_from_source() {
        let root = temp_root("missing-catalog");
        std::fs::create_dir_all(root.join("en_US")).unwrap();
        std::fs::write(
            root.join("en_US").join(CATALOG_FILE),
            "[editor]\n\"a\" = \"A-en\"\n",
        )
        .unwrap();

        let l = Localizer::load(&root, &Locale::parse("ja_JP").unwrap());
        assert_eq!(l.lookup("a"), "A-en");
        // No Japanese catalog at all: nothing is reported missing beyond the key.
        assert_eq!(l.lookup("nope"), "nope");
    }

    #[test]
    fn there_is_no_parent_locale_resolution() {
        // docs/i18n.md 4: zh_CN does not inherit from a bare `zh`. A catalog
        // for `pt` must not leak into `pt_BR`.
        let root = temp_root("no-parent-locale");
        for (dir, text) in [
            ("en_US", "[editor]\n\"a\" = \"A-en\"\n"),
            ("pt", "[editor]\n\"a\" = \"A-pt\"\n"),
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join(CATALOG_FILE), text).unwrap();
        }

        let l = Localizer::load(&root, &Locale::parse("pt_BR").unwrap());
        assert_eq!(
            l.lookup("a"),
            "A-en",
            "pt_BR must fall through to the source, never to a bare \"pt\" catalog"
        );
    }

    #[test]
    fn missing_keys_reports_gaps_against_source() {
        let l = Localizer::from_catalogs(
            Locale::parse("zh_CN").unwrap(),
            Some(catalog(Locale::parse("zh_CN").unwrap(), &[("a", "A-zh")])),
            catalog(Locale::source(), &[("a", "A-en"), ("b", "B-en")]),
        );
        assert_eq!(l.missing_keys(), vec!["b"]);
    }

    #[test]
    fn missing_keys_is_empty_for_the_source_locale() {
        let l = Localizer::from_catalogs(
            Locale::source(),
            None,
            catalog(Locale::source(), &[("a", "A-en")]),
        );
        assert!(l.missing_keys().is_empty());
    }

    // ---------- Placeholders ----------

    #[test]
    fn substitutes_known_placeholders() {
        let out = substitute("Undo {label}", &[("label", "Move entity".to_string())]);
        assert_eq!(out, "Undo Move entity");
    }

    #[test]
    fn leaves_unknown_placeholders_intact() {
        let out = substitute("Undo {label}", &[]);
        assert_eq!(out, "Undo {label}");
    }

    #[test]
    fn double_braces_escape() {
        assert_eq!(substitute("{{x}}", &[]), "{x}");
    }

    #[test]
    fn stray_and_unterminated_braces_are_kept() {
        assert_eq!(substitute("100%} done", &[]), "100%} done");
        assert_eq!(substitute("{unterminated", &[]), "{unterminated");
    }

    #[test]
    fn format_uses_the_fallback_chain() {
        let l = Localizer::from_catalogs(
            Locale::parse("zh_CN").unwrap(),
            None,
            catalog(
                Locale::source(),
                &[("status.room_added", "Room {name} added")],
            ),
        );
        let out = l.format("status.room_added", &[("name", "start".to_string())]);
        assert_eq!(out, "Room start added");
    }

    // ---------- Discovery ----------

    #[test]
    fn discovers_only_directories_with_a_catalog() {
        let root = temp_root("discover");
        for dir in ["en_US", "zh_CN", "notalocale"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("en_US").join(CATALOG_FILE), "").unwrap();
        std::fs::write(root.join("zh_CN").join(CATALOG_FILE), "").unwrap();

        let found: Vec<String> = discover(&root).iter().map(|l| l.to_string()).collect();
        assert_eq!(found, vec!["en_US", "zh_CN"]);
    }

    #[test]
    fn discovery_on_a_missing_root_is_empty_not_fatal() {
        assert!(discover(Path::new("/nonexistent/releste/locales")).is_empty());
    }

    #[test]
    fn catalog_root_candidates_are_absolute_or_relative_to_the_checkout() {
        // The last candidate is derived from CARGO_MANIFEST_DIR, so it points at
        // the workspace's locales/ even when the working directory is elsewhere.
        let candidates = catalog_root_candidates();
        assert!(!candidates.is_empty());
        let from_manifest = candidates
            .iter()
            .any(|c| c.ends_with("locales") && c.is_absolute());
        assert!(
            from_manifest,
            "expected an absolute cargo-manifest candidate, got {candidates:?}"
        );
    }

    #[test]
    fn an_empty_locales_directory_is_not_a_catalog_root() {
        // Regression: the candidate test used to be is_dir(), so a stray empty
        // locales/ shadowed the checkout real root and every label silently
        // degraded to its key.
        let empty = temp_root("empty-root");
        assert!(empty.is_dir(), "the fixture is a real directory");
        assert!(
            !holds_a_catalog(&empty),
            "an empty directory must not be accepted as a catalog root"
        );

        // A directory with a non-locale subdirectory is still not a root.
        std::fs::create_dir_all(empty.join("not-a-locale")).unwrap();
        assert!(!holds_a_catalog(&empty));

        // One real catalog makes it a valid root.
        std::fs::create_dir_all(empty.join("en_US")).unwrap();
        std::fs::write(empty.join("en_US").join(CATALOG_FILE), "").unwrap();
        assert!(holds_a_catalog(&empty));
    }

    #[test]
    fn find_catalog_root_finds_the_checkout_locales() {
        // The test runs from the checkout, so one of the candidates exists.
        let root = find_catalog_root().expect("the repository ships locales/");
        assert!(root.join(SOURCE_LOCALE).join(CATALOG_FILE).is_file());
    }

    // ---------- Environment ----------

    // These use parse_locale_preference with an injected getter rather than
    // std::env::set_var: mutating the process environment is unsafe in
    // edition 2024 and would make the tests order-dependent.

    #[test]
    fn explicit_flag_wins_over_environment() {
        let l = parse_locale_preference(Some("zh_CN.UTF-8"), |_| Some("de_DE".into()));
        assert_eq!(l.to_string(), "zh_CN");
    }

    #[test]
    fn invalid_explicit_flag_falls_through_without_panic() {
        // No environment at all: the source locale is the last resort.
        let l = parse_locale_preference(Some("!!!"), |_| None);
        assert_eq!(l.to_string(), SOURCE_LOCALE);
    }

    #[test]
    fn environment_is_read_when_no_flag_is_given() {
        let l = parse_locale_preference(None, |var| {
            (var == "LANG").then(|| "zh_CN.UTF-8".to_string())
        });
        assert_eq!(l.to_string(), "zh_CN");
    }

    #[test]
    fn c_and_posix_locales_are_ignored() {
        let l = parse_locale_preference(None, |var| match var {
            "LC_ALL" => Some("C".to_string()),
            "LANG" => Some("POSIX".to_string()),
            _ => None,
        });
        assert_eq!(l.to_string(), SOURCE_LOCALE);
    }

    #[test]
    fn releste_lang_is_honoured_and_outranks_lc_all() {
        let l = parse_locale_preference(None, |var| match var {
            "RELESTE_LANG" => Some("fr_FR.UTF-8".to_string()),
            "LC_ALL" => Some("de_DE.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(l.to_string(), "fr_FR");
    }

    #[test]
    fn lc_messages_is_consulted_when_the_variables_above_it_are_absent() {
        let l = parse_locale_preference(None, |var| match var {
            "LC_MESSAGES" => Some("pt_BR".to_string()),
            "LANG" => Some("zh_CN.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(l.to_string(), "pt_BR");
    }

    #[test]
    fn earlier_environment_variable_wins() {
        let l = parse_locale_preference(None, |var| match var {
            "LC_ALL" => Some("ja_JP.UTF-8".to_string()),
            "LANG" => Some("zh_CN.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(l.to_string(), "ja_JP");
    }
}
