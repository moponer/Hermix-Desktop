//! The accessibility-tree snapshot: the only thing the model ever "sees" of a
//! page.
//!
//! Pitfall F4 — **never return raw HTML**. A full `Page::content()` dump of
//! any real site is 500 KB+ and permanently poisons the context of the run that
//! asked for it. There is deliberately no `get_full_html` tool in this module
//! and no code path that can produce one. The AX tree carries roles, names and
//! states — everything needed to decide *what to click* — at a small fraction
//! of the size, and `Accessibility.getFullAXTree` lets us skip the nodes that
//! are noise.
//!
//! Refs (`e1`, `e2`, …) are ours, assigned in document order. Raw backend node
//! ids are never shown: they are stable within one tree only, so leaking them
//! would invite the model to invent refs that do not exist.

use std::collections::{HashMap, VecDeque};

use chromiumoxide::cdp::browser_protocol::accessibility::{
    AxNode as CdpAxNode, AxValue, GetFullAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;
use chromiumoxide::Page;

/// Longest accessible name kept per node. One node with a 30 KB `aria-label`
/// (a table of contents, a product description list) would otherwise consume
/// the entire node budget on its own.
const MAX_NAME_CHARS: usize = 300;

/// One actionable element, flattened for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct AxNode {
    pub ref_id: String,
    pub role: String,
    pub name: String,
    pub value: String,
    pub states: Vec<String>,
    pub depth: usize,
    /// AX nodes the browser hides from assistive tech. Kept as a flag rather
    /// than filtered at map time so the renderer is defensively correct on its
    /// own and so the filtering is unit-testable.
    pub ignored: bool,
    /// `None` only for nodes the renderer will not emit; [`snapshot`] keeps
    /// solely the nodes that carry one, which is what makes every ref in the
    /// output resolvable back to a DOM node.
    pub backend_node_id: Option<BackendNodeId>,
}

/// Render the tree as one line per node, capped on both axes.
///
/// Pure by design — this is the function the context-budget rules actually
/// live in, so it is where the unit tests belong.
pub fn render_ax_tree(nodes: &[AxNode], max_depth: usize, max_nodes: usize) -> String {
    let mut out = String::new();
    let mut emitted = 0usize;
    let mut hit_depth = false;
    let mut hit_nodes = false;

    for node in nodes {
        if node.ignored {
            continue;
        }
        if emitted >= max_nodes {
            hit_nodes = true;
            break;
        }
        // `continue` rather than `break`: this stays correct even if a caller
        // hands over nodes that are not in depth order.
        if node.depth > max_depth {
            hit_depth = true;
            continue;
        }

        // Indentation is capped: depth 40 must not push the role off the line.
        out.push_str(&"  ".repeat(node.depth.min(8)));
        out.push_str("ref=");
        out.push_str(&node.ref_id);
        out.push(' ');
        out.push_str(if node.role.is_empty() {
            "generic"
        } else {
            &node.role
        });
        out.push_str(" \"");
        // Clipped here rather than by the caller: this function is where the
        // context budget lives, so nothing can hand it an unclipped name. A raw
        // quote would break the one-line format the model parses, so it is
        // neutralised rather than escaped.
        out.push_str(&clip(&node.name.replace('"', "'")));
        out.push('"');
        if !node.value.is_empty() {
            out.push_str(" value=");
            out.push_str(&clip(&node.value));
        }
        if !node.states.is_empty() {
            out.push_str(&format!(" [{}]", node.states.join(",")));
        }
        out.push('\n');
        emitted += 1;
    }

    if hit_nodes || hit_depth {
        let mut reasons = Vec::new();
        if hit_nodes {
            reasons.push(format!("{max_nodes} nodes"));
        }
        if hit_depth {
            reasons.push(format!("depth {max_depth}"));
        }
        // An explicit marker beats silently dropping the tail: the model must
        // be able to tell "that is all there is" from "that is all you get".
        out.push_str(&format!(
            "[truncated: {} reached — re-run browser_snapshot with a higher max_nodes/max_depth \
             for the subtree you need]\n",
            reasons.join(" / ")
        ));
    }

    out
}

/// Fetch and render the current page's accessibility tree, plus the
/// `ref` → backend node mapping for every ref in the output.
///
/// The mapping is what makes `browser_click` / `browser_type` possible:
/// `chromiumoxide::Element` cannot be constructed from an arbitrary node id
/// (`Element::new` is crate-private), so the only public route from a ref back
/// to a DOM node is `DOM.resolveNode` on the backend id captured here.
pub async fn snapshot_with_refs(
    page: &Page,
    max_depth: usize,
    max_nodes: usize,
) -> Result<(String, HashMap<String, BackendNodeId>), String> {
    // Depth is capped in `render_ax_tree`, not here: one place owns the budget,
    // so the numbers the model passes cannot disagree with what it got back.
    let response = page
        .execute(GetFullAxTreeParams::builder().build())
        .await
        .map_err(|e| format!("读取无障碍树失败: {e}"))?;
    let nodes = map_tree(&response.result.nodes);
    let refs: HashMap<String, BackendNodeId> = nodes
        .iter()
        .filter(|n| !n.ignored)
        .filter_map(|n| n.backend_node_id.map(|id| (n.ref_id.clone(), id)))
        .collect();
    Ok((render_ax_tree(&nodes, max_depth, max_nodes), refs))
}

/// Flatten the CDP tree into renderable nodes, assigning our own refs.
fn map_tree(raw: &[CdpAxNode]) -> Vec<AxNode> {
    let depths = assign_depths(raw);
    let mut out = Vec::new();

    // Document order, not tree order: the model reads top-to-bottom, and the
    // refs stay stable with what it just saw.
    for node in raw {
        let Some(backend_node_id) = node.backend_dom_node_id else {
            // Pseudo-nodes (text alternatives, descriptions, relations) have no
            // DOM node and can never be clicked. Dropping them keeps every ref
            // in the output actionable.
            continue;
        };
        if node.ignored {
            continue;
        }
        out.push(AxNode {
            ref_id: format!("e{}", out.len() + 1),
            role: scalar_text(node.role.as_ref()),
            // Not clipped here: `render_ax_tree` owns every output cap, so the
            // budget cannot be half-applied depending on the caller.
            name: scalar_text(node.name.as_ref()),
            value: scalar_text(node.value.as_ref()),
            states: states_of(node),
            depth: depths.get(node.node_id.inner()).copied().unwrap_or(0),
            ignored: false,
            backend_node_id: Some(backend_node_id),
        });
    }
    out
}

/// Breadth-first depth assignment, rooted at nodes whose parent is not in this
/// tree.
///
/// `getFullAXTree` answers per frame, so a node's `parentId` can name an id
/// that was never returned; treating those as roots keeps a whole subframe from
/// disappearing into an unassigned, depth-0 pile.
fn assign_depths(raw: &[CdpAxNode]) -> HashMap<String, usize> {
    let by_id: HashMap<&str, &CdpAxNode> =
        raw.iter().map(|n| (n.node_id.inner().as_str(), n)).collect();

    let mut depths: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
    for node in raw {
        let is_root = node
            .parent_id
            .as_ref()
            .is_none_or(|parent| !by_id.contains_key(parent.inner().as_str()));
        if is_root {
            depths.insert(node.node_id.inner().clone(), 0);
            queue.push_back((node.node_id.inner(), 0));
        }
    }

    while let Some((id, depth)) = queue.pop_front() {
        let Some(node) = by_id.get(id) else { continue };
        for child in node.child_ids.iter().flatten() {
            // Insert-if-absent doubles as the visited set, which is also what
            // keeps a malformed cyclic tree from looping forever.
            if depths.insert(child.inner().clone(), depth + 1).is_none() {
                queue.push_back((child.inner(), depth + 1));
            }
        }
    }
    depths
}

/// Flatten a scalar AX value to its string form.
fn scalar_text(value: Option<&AxValue>) -> String {
    match value.and_then(|v| v.value.as_ref()) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_NAME_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_NAME_CHARS).collect();
    out.push('…');
    out
}

/// State flags worth showing.
///
/// The JSON value is inspected directly rather than the `AxValueType` enum so
/// that a boolean `true` is the one and only trigger: enumerating the enum
/// would make every protocol addition a compile break here for no gain.
fn states_of(node: &CdpAxNode) -> Vec<String> {
    node.properties
        .iter()
        .flatten()
        .filter_map(|prop| {
            let value = prop.value.value.as_ref()?;
            // `AxPropertyName` is a generated enum with no accessor, so its
            // serde name is the protocol name — which is exactly the string a
            // reader of the snapshot needs to see.
            let name = serde_json::to_value(&prop.name).ok()?.as_str()?.to_string();
            if name == "level" {
                return value.as_i64().map(|n| format!("level={n}"));
            }
            (*value == serde_json::Value::Bool(true)).then_some(name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ref_id: &str, role: &str, depth: usize) -> AxNode {
        AxNode {
            ref_id: ref_id.to_string(),
            role: role.to_string(),
            name: "label".to_string(),
            value: String::new(),
            states: Vec::new(),
            depth,
            ignored: false,
            backend_node_id: None,
        }
    }

    #[test]
    fn renders_one_line_per_node_with_depth_indent() {
        let tree = render_ax_tree(&[node("e1", "RootWebArea", 0), node("e2", "heading", 1)], 5, 10);
        assert_eq!(
            tree,
            "ref=e1 RootWebArea \"label\"\n  ref=e2 heading \"label\"\n"
        );
    }

    #[test]
    fn depth_cap_stops_output_and_marks_it() {
        let tree = vec![
            node("e1", "RootWebArea", 0),
            node("e2", "main", 1),
            node("e3", "heading", 2),
        ];
        let out = render_ax_tree(&tree, 1, 10);
        assert!(out.contains("ref=e1"));
        assert!(out.contains("ref=e2"));
        assert!(!out.contains("ref=e3"), "depth 2 must be cut at max_depth 1");
        assert!(out.contains("[truncated: depth 1 reached"));
    }

    #[test]
    fn node_cap_stops_output_and_marks_it() {
        let tree = vec![node("e1", "a", 0), node("e2", "b", 0), node("e3", "c", 0)];
        let out = render_ax_tree(&tree, 9, 2);
        assert!(out.contains("ref=e2"));
        assert!(!out.contains("ref=e3"));
        assert!(out.contains("[truncated: 2 nodes reached"));
    }

    #[test]
    fn both_caps_report_together() {
        // The depth cap `continue`s and the node cap `break`s, so hitting both
        // needs the depth overflow to come first — otherwise the break hides it.
        let tree = vec![
            node("e1", "a", 0),
            node("e2", "deep", 5),
            node("e3", "b", 0),
            node("e4", "c", 0),
        ];
        let out = render_ax_tree(&tree, 1, 2);
        assert!(out.contains("[truncated: 2 nodes / depth 1 reached"), "{out}");
    }

    #[test]
    fn ignored_nodes_are_dropped_and_do_not_consume_budget() {
        let mut hidden = node("e9", "generic", 0);
        hidden.ignored = true;
        let tree = vec![node("e1", "a", 0), hidden, node("e2", "b", 0)];
        let out = render_ax_tree(&tree, 9, 2);
        assert!(!out.contains("ref=e9"));
        assert!(out.contains("ref=e2"), "an ignored node must not eat a slot");
        assert!(!out.contains("[truncated"), "nothing was actually dropped");
    }

    #[test]
    fn empty_tree_renders_nothing_without_a_marker() {
        assert_eq!(render_ax_tree(&[], 5, 5), "");
    }

    #[test]
    fn states_and_value_are_rendered_and_quotes_are_neutralised() {
        let mut n = node("e1", "textbox", 0);
        n.name = r#"say "hi""#.to_string();
        n.value = "typed".to_string();
        n.states = vec!["focused".to_string(), "editable".to_string()];
        let out = render_ax_tree(&[n], 5, 5);
        assert_eq!(out, "ref=e1 textbox \"say 'hi'\" value=typed [focused,editable]\n");
    }

    #[test]
    fn long_names_are_clipped_on_a_char_boundary() {
        let mut n = node("e1", "link", 0);
        n.name = "中".repeat(MAX_NAME_CHARS + 50);
        let out = render_ax_tree(&[n], 5, 5);
        let name = out
            .split_once('"')
            .and_then(|(_, rest)| rest.rsplit_once('"'))
            .map(|(name, _)| name.to_string())
            .expect("quoted name");
        assert_eq!(name.chars().count(), MAX_NAME_CHARS + 1); // + the ellipsis
        assert!(name.ends_with('…'));
    }
}
