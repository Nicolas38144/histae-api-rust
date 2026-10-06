use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Route {
    method: String,
    path: String,
}

#[test]
fn documented_http_inventory_matches_rust_route_registrations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let documented = documented_routes(&root.join("docs/http-contract.md"));
    let registered = registered_routes(&root.join("src"));

    let missing = documented
        .difference(&registered)
        .cloned()
        .collect::<Vec<_>>();
    let undocumented = registered
        .difference(&documented)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && undocumented.is_empty(),
        "HTTP inventory differs: missing in Rust={missing:#?}; undocumented in Rust={undocumented:#?}"
    );
    assert_eq!(
        documented.len(),
        102,
        "review the expected route count when the contract changes"
    );
}

fn documented_routes(path: &Path) -> BTreeSet<Route> {
    let source = fs::read_to_string(path).expect("HTTP contract must be readable");
    source
        .lines()
        .filter_map(|line| {
            let mut cells = line.split('|').map(str::trim);
            let _empty = cells.next()?;
            let method = cells.next()?;
            let path = cells.next()?.strip_prefix('`')?.strip_suffix('`')?;
            is_method(method).then(|| Route {
                method: method.to_owned(),
                path: path.to_owned(),
            })
        })
        .collect()
}

fn registered_routes(root: &Path) -> BTreeSet<Route> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files);
    let mut routes = BTreeSet::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("Rust source must be readable");
        for (path, handlers) in route_calls(&source) {
            if !(path.starts_with("/api/") || path.starts_with("/health/")) {
                continue;
            }
            let normalized = normalize_axum_path(&path);
            for (method, function) in [
                ("GET", "get"),
                ("POST", "post"),
                ("PUT", "put"),
                ("PATCH", "patch"),
                ("DELETE", "delete"),
            ] {
                if contains_call(&handlers, function) {
                    routes.insert(Route {
                        method: method.to_owned(),
                        path: normalized.clone(),
                    });
                }
            }
        }
    }
    routes
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn route_calls(source: &str) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(".route(") {
        let start = cursor + offset + ".route(".len();
        let Some(end) = matching_parenthesis(source, start) else {
            break;
        };
        let arguments = &source[start..end];
        if let Some(comma) = top_level_comma(arguments) {
            let path = arguments[..comma].trim();
            if let Some(path) = path
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
            {
                calls.push((path.to_owned(), arguments[comma + 1..].to_owned()));
            }
        }
        cursor = end + 1;
    }
    calls
}

fn matching_parenthesis(source: &str, start: usize) -> Option<usize> {
    let mut depth = 1_u32;
    let mut string = false;
    let mut escaped = false;
    for (offset, character) in source[start..].char_indices() {
        if string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                string = false;
            }
            continue;
        }
        match character {
            '"' => string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn top_level_comma(arguments: &str) -> Option<usize> {
    let mut depth = 0_u32;
    let mut string = false;
    let mut escaped = false;
    for (offset, character) in arguments.char_indices() {
        if string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                string = false;
            }
            continue;
        }
        match character {
            '"' => string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some(offset),
            _ => {}
        }
    }
    None
}

fn contains_call(source: &str, function: &str) -> bool {
    source.match_indices(function).any(|(index, _)| {
        let before = source[..index].chars().next_back();
        let after = source[index + function.len()..].trim_start().chars().next();
        !before.is_some_and(|value| value.is_ascii_alphanumeric() || value == '_')
            && after == Some('(')
    })
}

fn normalize_axum_path(path: &str) -> String {
    let mut normalized = String::with_capacity(path.len());
    let mut parameter = false;
    for character in path.chars() {
        match character {
            '{' => {
                parameter = true;
                normalized.push(':');
            }
            '}' => parameter = false,
            _ => normalized.push(character),
        }
    }
    debug_assert!(!parameter);
    normalized
}

fn is_method(value: &str) -> bool {
    matches!(value, "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
}
