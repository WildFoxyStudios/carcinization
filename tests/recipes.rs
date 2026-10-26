//! Every recipe in the tree has to parse.
//!
//! A recipe that does not parse breaks the listing of the whole category it
//! belongs to, which is a very confusing way to find out that a single file
//! has a typo. This test turns that into an obvious failure.

use std::path::Path;

#[test]
fn every_recipe_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("recipes");
    assert!(root.is_dir(), "the recipes directory is missing");

    let mut count = 0usize;
    let mut broken: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| {
            e.file_name() != "source" && e.file_name() != "target" && e.file_name() != "stage"
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.file_name().map(|f| f == "recipe.toml") != Some(true) {
            continue;
        }
        count += 1;

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                broken.push(format!("{}: cannot be read: {}", path.display(), err));
                continue;
            }
        };

        if let Err(err) = toml::from_str::<cookbook::recipe::Recipe>(&text) {
            broken.push(format!("{}: {}", path.display(), err));
        }
    }

    assert!(
        broken.is_empty(),
        "{} recipe(s) do not parse:\n{}",
        broken.len(),
        broken.join("\n")
    );
    assert!(count > 1000, "only {} recipes were found", count);
}
