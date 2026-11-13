use std::path::{Path, PathBuf};
use trace_preproc::Language;
use walkdir::WalkDir;

pub fn discover_c_files(root: &Path) -> Vec<PathBuf> {
    discover_source_files(root).0
}

/// `.h` files under the analyzed tree (for struct layouts), not external include dirs.
pub fn discover_header_files(root: &Path) -> Vec<PathBuf> {
    discover_source_files(root).1
}

/// Translation-unit source extensions: C plus the C++ spellings.
pub const TU_EXTENSIONS: &[&str] = &["c", "cpp", "cc", "cxx", "c++", "C"];
/// Header extensions pulled in via `#include` (and macro-warmed).
pub const HEADER_EXTENSIONS: &[&str] = &["h", "hpp", "hh", "hxx", "h++", "H", "inl", "ipp"];

pub(crate) fn has_extension_in(path: &Path, set: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| set.contains(&e))
}

/// C++ translation-unit extensions (not headers). Which of
/// [`TU_EXTENSIONS`] are C++ is [`Language::from_path`]'s call, so the
/// preprocessor's lexer and the parser's grammar never disagree.
pub fn is_cpp_path(path: &Path) -> bool {
    has_extension_in(path, TU_EXTENSIONS) && Language::from_path(path) == Language::Cpp
}

/// C++ header extensions, per [`Language::from_path`]. `.h` is ambiguous
/// and is *not* included: a `.h` is parsed as C++ only when the include
/// graph reaches it from a C++ TU.
pub fn is_cpp_header_path(path: &Path) -> bool {
    has_extension_in(path, HEADER_EXTENSIONS) && Language::from_path(path) == Language::Cpp
}

/// `.idl` interface definitions (docs/ANALYSIS.md, "IDL-generated interfaces").
const IDL_EXTENSION: &str = "idl";

/// Everything one directory walk classifies, each list sorted.
#[derive(Debug, Default)]
pub(crate) struct DiscoveredFiles {
    pub sources: Vec<PathBuf>,
    pub headers: Vec<PathBuf>,
    pub idl: Vec<PathBuf>,
}

/// Single directory walk collecting C/C++ TU, header and `.idl` paths.
pub(crate) fn discover_files(root: &Path) -> DiscoveredFiles {
    let mut out = DiscoveredFiles::default();
    let mut classify = |path: &Path| match path.extension().and_then(|x| x.to_str()) {
        Some(e) if TU_EXTENSIONS.contains(&e) => out.sources.push(path.to_path_buf()),
        Some(e) if HEADER_EXTENSIONS.contains(&e) => out.headers.push(path.to_path_buf()),
        Some(IDL_EXTENSION) => out.idl.push(path.to_path_buf()),
        _ => {}
    };
    if root.is_file() {
        classify(root);
        return out;
    }
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            classify(entry.path());
        }
    }
    out.sources.sort();
    out.headers.sort();
    out.idl.sort();
    out
}

/// Single directory walk collecting C/C++ TU paths and header paths.
pub fn discover_source_files(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let found = discover_files(root);
    (found.sources, found.headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn public_discovery_helpers_split_translation_units_and_headers() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("main.c");
        let header = dir.path().join("util.hpp");
        let idl = dir.path().join("iface.idl");
        std::fs::write(&source, "int main(void) { return 0; }\n").unwrap();
        std::fs::write(&header, "struct S {};\n").unwrap();
        std::fs::write(&idl, "interface IFoo {};\n").unwrap();

        assert_eq!(discover_c_files(dir.path()), vec![source.clone()]);
        assert_eq!(discover_header_files(dir.path()), vec![header]);
        // A single-file root keeps the same classification.
        assert_eq!(discover_c_files(&source), vec![source.clone()]);
        assert!(discover_header_files(&source).is_empty());
        assert!(discover_c_files(&idl).is_empty());
    }

    #[test]
    fn cpp_header_extensions_are_cpp() {
        assert!(is_cpp_header_path(Path::new("util.hpp")));
        assert!(is_cpp_header_path(Path::new("util.hh")));
        assert!(!is_cpp_header_path(Path::new("plugin.h")));
        assert!(!is_cpp_path(Path::new("plugin.h")));
        assert!(is_cpp_path(Path::new("plugin.cpp")));
    }

    /// Every discoverable extension is classified the same way by the
    /// lexer language and by the grammar helpers.
    #[test]
    fn extension_classification_matches_language_from_path() {
        for ext in TU_EXTENSIONS {
            let p = PathBuf::from(format!("x.{ext}"));
            assert_eq!(
                is_cpp_path(&p),
                Language::from_path(&p) == Language::Cpp,
                ".{ext}"
            );
            assert!(!is_cpp_header_path(&p), ".{ext} is a TU, not a header");
        }
        for ext in HEADER_EXTENSIONS {
            let p = PathBuf::from(format!("x.{ext}"));
            assert_eq!(
                is_cpp_header_path(&p),
                Language::from_path(&p) == Language::Cpp,
                ".{ext}"
            );
            assert!(!is_cpp_path(&p), ".{ext} is a header, not a TU");
        }
        assert!(is_cpp_path(Path::new("x.C")));
        assert!(is_cpp_header_path(Path::new("x.H")));
        assert!(is_cpp_header_path(Path::new("x.h++")));
        assert!(!is_cpp_header_path(Path::new("x.h")));
        assert!(!is_cpp_path(Path::new("x.c")));
    }
}
