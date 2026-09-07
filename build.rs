//! Builds the front end into the binary.
//!
//! `rust-embed` reads `ui/dist` at compile time, so the bundle is an input to the
//! compilation rather than a step somebody remembers to run first: `cargo build`
//! runs Vite itself, and needs `npm` on `PATH` to do it. `MIRE_BUILD_UI=0` opts
//! out, for the two builds that have a bundle already — the image build, whose
//! Node stage produced one, and a checkout with no Node toolchain at all.

use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Shown when the binary is built with `MIRE_BUILD_UI=0` and nothing in `ui/dist`.
const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>mire — UI not built</title>
  </head>
  <body style="font-family: system-ui; margin: 3rem auto; max-width: 40rem">
    <h1>The UI has not been built</h1>
    <p>
      The API is fine — try <a href="docs">the API reference</a>. To get this page,
      build again with the front end:
    </p>
    <pre>cargo build --release</pre>
  </body>
</html>
"#;

/// The front end's inputs: a change to any of them leaves `ui/dist` stale.
const UI_SOURCES: &[&str] = &[
    "ui/index.html",
    "ui/package-lock.json",
    "ui/package.json",
    "ui/public",
    "ui/src",
    "ui/tsconfig.json",
    "ui/vite.config.ts",
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=MIRE_BUILD_UI");
    for source in UI_SOURCES {
        println!("cargo::rerun-if-changed={source}");
    }

    if ui_build_requested() {
        // `ui/dist` is deliberately *not* watched here. A `rerun-if-changed` on
        // the directory this script rewrites is a build that never settles:
        // every run dirties its own input, so the next one re-runs the script
        // and recompiles the crate, forever. The sources above are the trigger.
        if dependencies_are_stale() {
            npm(&["ci"]);
        }
        npm(&["run", "build"]);
        return;
    }

    // Nothing here writes `ui/dist`, so watching it is what notices the bundle
    // somebody else put there.
    println!("cargo::rerun-if-changed=ui/dist");

    // Never overwrite a real build: only fill in when there is nothing there, so
    // that the crate still compiles with no front end anywhere.
    let dist = Path::new("ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(dist).expect("create ui/dist");
        std::fs::write(&index, PLACEHOLDER).expect("write the UI placeholder");
        println!("cargo::warning=ui/dist was empty; embedding a placeholder page instead");
    }
}

/// Whether to build the front end, which is what `MIRE_BUILD_UI` is for.
fn ui_build_requested() -> bool {
    match std::env::var("MIRE_BUILD_UI").as_deref() {
        Err(_) | Ok("" | "1") => true,
        Ok("0") => false,
        Ok(other) => panic!("MIRE_BUILD_UI: expected 0 or 1, got {other:?}"),
    }
}

/// Whether `npm ci` has to run first: nothing installed, or a lockfile that has
/// moved since it was.
fn dependencies_are_stale() -> bool {
    let Some(installed) = modified("ui/node_modules") else {
        return true;
    };
    modified("ui/package-lock.json").is_none_or(|locked| locked > installed)
}

/// The modification time of `path`, or `None` when it cannot be read at all.
fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// Runs `npm` in `ui/`, and fails the build when it does.
///
/// The output is captured rather than inherited: this script's own stdout is the
/// channel Cargo reads `cargo::` directives from, and npm has nothing to say on
/// it. Replaying it on stderr when the command fails is what puts the Vite error
/// in front of whoever ran `cargo build` — Cargo shows a failed build script's
/// stderr and swallows it otherwise.
fn npm(args: &[&str]) {
    let command = format!("npm {}", args.join(" "));
    let output = Command::new("npm")
        .args(args)
        .current_dir("ui")
        .output()
        .unwrap_or_else(|error| {
            panic!("run `{command}` (the UI is built here, so npm has to be on PATH, or MIRE_BUILD_UI=0): {error}")
        });

    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        panic!("`{command}` failed: {}", output.status);
    }
}
