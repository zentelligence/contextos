use std::collections::BTreeSet;
use std::convert::Infallible;

use super::*;

struct Vault(BTreeSet<String>);

impl LinkTargets for Vault {
    type Error = Infallible;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        Ok(self.0.contains(path))
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        let suffix = suffix.to_lowercase();
        let ending = format!("/{suffix}");
        Ok(self
            .0
            .iter()
            .filter(|path| {
                let path = path.to_lowercase();
                path == suffix || path.ends_with(&ending)
            })
            .cloned()
            .collect())
    }
}

fn expand(paths: &[&str], source: &str, max_distinct_targets: usize) -> Result<Value, String> {
    let vault = Vault(paths.iter().map(|path| (*path).to_owned()).collect());
    let expanded = ExpandedCanvas::try_from(CanvasExpansionInput {
        source,
        canvas: "boards/plan.canvas",
        targets: &vault,
        max_distinct_targets,
        max_output_bytes: 1_000_000,
    })
    .map_err(|error| format!("{error:?}"))?;
    serde_json::from_str(expanded.as_ref()).map_err(|error| error.to_string())
}

const BOARD: &str = r##"{
  "nodes": [
    {"id": "t", "type": "text", "text": "See [[Plan]] and [x](nowhere.md)", "x": 0, "y": 0, "width": 10, "height": 10},
    {"id": "f", "type": "file", "file": "boards/Plan.md", "subpath": "#Goals", "x": 0, "y": 0, "width": 10, "height": 10},
    {"id": "g", "type": "file", "file": "gone.png", "x": 0, "y": 0, "width": 10, "height": 10},
    {"id": "l", "type": "link", "url": "https://example.org", "x": 0, "y": 0, "width": 10, "height": 10}
  ],
  "edges": [],
  "x-extension": {"kept": true}
}"##;

#[test]
fn a_text_node_has_its_links_expanded_from_the_canvas_folder() {
    let canvas = expand(&["boards/Plan.md"], BOARD, 100).unwrap_or_default();
    assert_eq!(
        canvas["nodes"][0]["text"],
        "See [[/boards/Plan.md|Plan]] and [x](/:!nowhere.md)"
    );
}

#[test]
fn a_file_node_is_rooted_when_it_exists_and_marked_when_it_does_not() {
    let canvas = expand(&["boards/Plan.md"], BOARD, 100).unwrap_or_default();
    assert_eq!(canvas["nodes"][1]["file"], "/boards/Plan.md");
    assert_eq!(canvas["nodes"][1]["subpath"], "#Goals");
    assert_eq!(canvas["nodes"][2]["file"], "/:!gone.png");
}

#[test]
fn everything_else_in_the_canvas_is_kept_in_its_order() {
    let canvas = expand(&["boards/Plan.md"], BOARD, 100).unwrap_or_default();
    assert_eq!(canvas["nodes"][3]["url"], "https://example.org");
    assert_eq!(canvas["x-extension"]["kept"], true);
    let keys: Vec<&String> = canvas
        .as_object()
        .map(|object| object.keys().collect())
        .unwrap_or_default();
    assert_eq!(keys, ["nodes", "edges", "x-extension"]);
}

#[test]
fn a_canvas_that_is_not_json_is_refused() {
    assert!(expand(&[], "{ not json", 100).is_err_and(|error| error.contains("InvalidCanvas")));
}

#[test]
fn a_canvas_whose_nodes_are_not_an_array_is_refused() {
    assert!(expand(&[], r#"{"nodes": {}}"#, 100).is_err_and(|error| error.contains("InvalidCanvas")));
}

#[test]
fn the_target_limit_counts_text_and_file_nodes_together() {
    let result = expand(&["boards/Plan.md"], BOARD, 2);
    assert!(result.is_err_and(|error| error.contains("TooManyTargets")));
}
