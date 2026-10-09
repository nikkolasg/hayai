//! The sources of the crate stay in the Rust subset of Charon and Aeneas: no panic, no
//! iterator adapter chain, no hash map, no shared ownership, no global state, no
//! truncating cast. The test reads each file of `src/` and reports each line with a token
//! outside the subset. Lines of comments do not count, and the scan of a file stops at its
//! `#[cfg(test)] mod tests` module. A `#[cfg(test)]` on another item does not stop it.

use std::fs;
use std::path::Path;

/// The tokens that the subset does not have.
const FORBIDDEN: &[&str] = &[
    "unwrap(",
    "expect(",
    "unreachable!",
    "panic!",
    "assert!",
    "assert_eq!",
    "todo!",
    "unimplemented!",
    ".iter()",
    ".iter_mut()",
    ".into_iter()",
    ".map(",
    ".filter(",
    ".filter_map(",
    ".fold(",
    ".collect(",
    ".find(",
    ".position(",
    ".any(",
    ".all(",
    ".sum(",
    ".last(",
    ".rev(",
    ".zip(",
    ".enumerate(",
    ".sort(",
    ".sort_unstable(",
    ".sort_by",
    ".chunks(",
    ".windows(",
    ".step_by(",
    ".take(",
    ".skip(",
    ".max_by",
    ".min_by",
    "impl Iterator",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "Arc<",
    "Rc<",
    "dyn ",
    "LazyLock",
    "OnceLock",
    "OnceCell",
    "leak(",
    "static mut",
    "unsafe {",
    "unsafe fn",
    " as u",
    " as i",
    " as f",
];

fn sources(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("the source directory exists") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn the_sources_stay_in_the_subset() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "the crate has sources");
    let mut findings = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).expect("a readable source file");
        let lines: Vec<&str> = text.lines().collect();
        for (number, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                let next = lines.get(number + 1).map_or("", |next| next.trim_start());
                if next.starts_with("mod tests") || next.starts_with("pub(crate) mod tests") {
                    break;
                }
            }
            if trimmed.starts_with("//") {
                continue;
            }
            for token in FORBIDDEN {
                if line.contains(token) {
                    findings.push(format!(
                        "{}:{}: `{token}`: {}",
                        path.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "tokens outside the subset:\n{}",
        findings.join("\n")
    );
}

/// A module of the core has no name that a local variable has. Lean reads
/// `spec.CoreSpec.checked` as a field of a local `spec`, not as the namespace of the module
/// `spec`, so such a translation does not build. The test takes the binders of each
/// function (`name:` in a signature, `let name`, `let mut name`) and reports one that is a
/// module name.
#[test]
fn no_module_has_the_name_of_a_local() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    let modules: Vec<String> = files
        .iter()
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .filter(|stem| stem != "lib")
        .collect();
    assert!(!modules.is_empty(), "the crate has modules");
    let mut findings = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).expect("a readable source file");
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for module in &modules {
                let binder = trimmed.starts_with(&format!("let {module} "))
                    || trimmed.starts_with(&format!("let {module}:"))
                    || trimmed.starts_with(&format!("let mut {module} "))
                    || trimmed.starts_with(&format!("let mut {module}:"))
                    || line.contains(&format!(" {module}: "))
                    || line.contains(&format!("({module}: "));
                if binder {
                    findings.push(format!(
                        "{}:{}: `{module}` is a module: {}",
                        path.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "locals named as a module:\n{}",
        findings.join("\n")
    );
}
