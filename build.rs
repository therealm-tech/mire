//! Keeps the embedded UI and the binary in step.
//!
//! `rust-embed` reads `ui/dist` at compile time, so Cargo has to be told that a
//! rebuilt UI means a rebuilt binary — and that the directory has to exist at all
//! for the crate to compile.

use std::path::Path;

/// Shown when the binary is built without the UI having been built first.
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
      build the front end and rebuild the binary:
    </p>
    <pre>cd ui &amp;&amp; npm install &amp;&amp; npm run build
cargo build --release</pre>
  </body>
</html>
"#;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=ui/dist");

    // Never overwrite a real build: only fill in when there is nothing there, so
    // that `cargo build` works on a fresh clone without a Node toolchain.
    let dist = Path::new("ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(dist).expect("create ui/dist");
        std::fs::write(&index, PLACEHOLDER).expect("write the UI placeholder");
        println!("cargo::warning=ui/dist was empty; embedding a placeholder page instead");
    }
}
