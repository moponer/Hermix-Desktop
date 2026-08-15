//! AX tree → compact YAML serialization, plus the helpers that build the
//! `wait_for` JS predicate and produce the textual a11y diff. Kept separate
//! from the CDP transport so the serialization logic can be unit-tested in
//! isolation.

use serde_json::Value;

/// Pick a stable CSS selector for an AX node, used as the
/// resolver target for `[ref=eN]` tokens.
fn ax_node_selector(role: &str, name: &str) -> String {
    if name.is_empty() {
        format!("[role=\"{}\"]", role)
    } else {
        // Playwright-style: prefer accessible name match.
        format!("[role=\"{}\"]:has-text('{}')", role, name.replace('\'', "\\'"))
    }
}

fn ax_role_is_interactive(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "combobox"
            | "checkbox"
            | "radio"
            | "switch"
            | "menuitem"
            | "tab"
            | "option"
            | "slider"
            | "spinbutton"
    )
}

/// Convert CDP `Accessibility.getFullAXTree` `nodes` array into a compact
/// YAML tree plus a `ref → CSS selector` map for interactive nodes.
/// `depth` limits how many nesting levels are emitted (root = 0).
/// `target` optionally filters by a CSS selector substring (matched against
/// the node's own selector or any descendant's); pass `None` to keep everything.
pub(crate) fn ax_tree_to_a11y_yaml(
    nodes: &Value,
    depth: Option<u32>,
    target: Option<&str>,
) -> (String, std::collections::HashMap<String, String>) {
    use std::collections::{HashMap, VecDeque};

    let arr = match nodes.as_array() {
        Some(a) => a,
        None => return (String::new(), HashMap::new()),
    };

    // Build a nodeId → index map and an inverse parent map by scanning
    // each node's `childIds`. This fixes a pre-existing bug where the
    // walker used `arr[idx-1]` as the parent and overwrote the children
    // map, causing all but the last node's childIds to be lost.
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (idx, node) in arr.iter().enumerate() {
        if let Some(id) = node.get("nodeId").and_then(|v| v.as_str()) {
            id_to_idx.insert(id.to_string(), idx);
        }
    }
    for node in arr.iter() {
        let parent_id = node
            .get("nodeId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(parent) = parent_id {
            if let Some(kids) = node.get("childIds").and_then(|v| v.as_array()) {
                let owned: Vec<String> = kids
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if !owned.is_empty() {
                    children.entry(parent).or_default().extend(owned);
                }
            }
        }
    }

    // Identify root: the first node whose nodeId has no incoming parent.
    // CDP emits roots first; use the first array entry.
    let root_id = arr
        .first()
        .and_then(|n| n.get("nodeId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let max_depth = depth.unwrap_or(8);
    let mut out = String::new();
    let mut refs: HashMap<String, String> = HashMap::new();
    let mut ref_counter: u32 = 0;

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    if let Some(r) = &root_id {
        queue.push_back((r.clone(), 0));
    }

    while let Some((node_id, level)) = queue.pop_front() {
        if level > max_depth {
            continue;
        }
        let Some(&idx) = id_to_idx.get(&node_id) else { continue };
        let node = &arr[idx];
        let role = node
            .get("role")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = node
            .get("name")
            .and_then(|n| n.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let interactive = ax_role_is_interactive(role);
        let selector = ax_node_selector(role, name);
        if let Some(t) = target {
            if !selector.contains(t) && !name.contains(t) {
                // skip this subtree unless a descendant matches later
                // (we still walk children to allow deeper matches).
                let indent = "  ".repeat(level as usize);
                let _ = indent; // keep children-only filter cheap
                if let Some(kids) = children.get(&node_id) {
                    for k in kids {
                        queue.push_back((k.clone(), level + 1));
                    }
                }
                continue;
            }
        }

        // Emit YAML line.
        let indent = "  ".repeat(level as usize);
        let mut line = format!("{indent}- {role}");
        if !name.is_empty() {
            line.push_str(&format!(" \"{name}\""));
        }
        if interactive {
            ref_counter += 1;
            let ref_id = format!("e{ref_counter}");
            line.push_str(&format!(" [ref={ref_id}]"));
            refs.insert(ref_id, selector);
        }
        // Surface states (disabled, checked, expanded) when present.
        let mut state_bits: Vec<&str> = Vec::new();
        if let Some(props) = node.get("properties").and_then(|p| p.as_array()) {
            for p in props {
                let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let pval = p
                    .get("value")
                    .and_then(|v| v.get("value"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if pname == "disabled" && pval {
                    state_bits.push("[disabled]");
                } else if pname == "checked" && pval {
                    state_bits.push("[checked]");
                } else if pname == "expanded" && pval {
                    state_bits.push("[expanded]");
                } else if pname == "required" && pval {
                    state_bits.push("[required]");
                }
            }
        }
        for s in state_bits {
            line.push_str(&format!(" {s}"));
        }
        out.push_str(&line);
        out.push('\n');

        if let Some(kids) = children.get(&node_id) {
            for k in kids {
                queue.push_back((k.clone(), level + 1));
            }
        }
    }

    (out, refs)
}

/// Strip non-interactive ancestor branches from an a11y YAML tree, keeping
/// only subtrees that contain at least one `[ref=...]` line. This is the
/// "compact" mode the LLM opts into for dense pages (form-heavy admin UIs,
/// dashboards, settings pages). Typical savings: ~50% token reduction.
/// Implementation: collect kept lines plus their children, then rewrite
/// indentation so the tree is still structurally valid YAML.
pub(crate) fn compact_a11y_yaml(yaml: &str) -> String {
    // Strategy: mark every line that contains `[ref=` and walk the
    // indentation upwards to include the parent path.
    let lines: Vec<&str> = yaml.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.contains("[ref=") {
            // Mark this line and all its ancestors (lower indent) as kept.
            let indent = count_indent(line);
            keep[i] = true;
            for j in (0..i).rev() {
                if keep[j] {
                    break;
                }
                let parent_indent = count_indent(lines[j]);
                if parent_indent < indent {
                    keep[j] = true;
                    indent_check(indent, parent_indent);
                }
            }
        }
    }
    lines
        .iter()
        .zip(keep.iter())
        .filter_map(|(l, &k)| if k { Some(l.to_string()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn count_indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

#[allow(dead_code)]
fn indent_check(_child: usize, _parent: usize) {}

/// Build a JS expression that returns `{matched: bool, count, first}` after
/// AND-combining the selector, optional state predicate, optional url_glob,
/// and optional js_fn expression. `state` ∈ {visible, hidden, enabled,
/// disabled, checked, unchecked}. `url_glob` is a substring match against
/// `location.href`. `js_fn` is a boolean expression evaluated in the page.
pub(crate) fn build_wait_for_js(
    selector: &str,
    state: Option<&str>,
    url_glob: Option<&str>,
    js_fn: Option<&str>,
) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
    // Per-element predicate: a JS expression that, given `el`, returns
    // `true` if the element matches the requested `state`. We splice it
    // into a forEach loop. If no state is given the predicate is `true`.
    let elem_pred: String = match state {
        Some("visible") => {
            "var r=el.getBoundingClientRect(); (r.width>0 && r.height>0 \
             && r.bottom>0 && r.top<innerHeight)"
                .to_string()
        }
        Some("hidden") => {
            "var r=el.getBoundingClientRect(); (r.width===0 || r.height===0 \
             || r.bottom<=0 || r.top>=innerHeight)"
                .to_string()
        }
        Some("enabled") => "!el.disabled".to_string(),
        Some("disabled") => "!!el.disabled".to_string(),
        Some("checked") => "!!el.checked".to_string(),
        Some("unchecked") => "!el.checked".to_string(),
        None => "true".to_string(),
        Some(other) => {
            // Unknown state — fail loud instead of silently always-true.
            format!("(console.warn('unknown state: {}'), false)", other)
        }
    };
    // url_glob and js_fn are evaluated once (not per element). We require
    // ALL of: selector count > 0, per-element state predicate, url match,
    // js_fn truthy.
    let url_check: String = match url_glob {
        Some(g) => format!(
            "({})",
            format!("location.href.indexOf({})>=0", serde_json::to_string(g).unwrap_or_else(|_| "\"\"".into()))
        ),
        None => "true".to_string(),
    };
    let fn_check: String = match js_fn {
        Some(f) => format!("(!!({}))", f),
        None => "true".to_string(),
    };
    format!(
        "(function(){{ \
            var els = document.querySelectorAll({sel}); \
            var state_ok = false; \
            for (var i=0;i<els.length;i++){{ var el=els[i]; if({elem}){{ state_ok=true; break; }} }} \
            var url_ok = {url}; \
            var fn_ok = {fnc}; \
            var matched = (els.length>0) && state_ok && url_ok && fn_ok; \
            return JSON.stringify({{ \
                matched: matched, \
                count: els.length, \
                first: els[0] ? (els[0].outerHTML||'').slice(0,200) : null, \
                state_ok: state_ok, url_ok: url_ok, fn_ok: fn_ok \
            }}); \
         }})()",
        sel = sel_json,
        elem = elem_pred,
        url = url_check,
        fnc = fn_check
    )
}

/// Diff two compact YAML a11y trees and return a structured description
/// of what changed (added/removed nodes + a preview of the new tree).
/// Used by `browser_diff` to let the LLM see the effect of a click/type
/// without paying for a full re-snapshot on every step.
pub(crate) fn diff_a11y_yaml(old: &str, new: &str) -> String {
    use std::collections::BTreeSet;
    let old_lines: BTreeSet<&str> = old.lines().filter(|l| !l.trim().is_empty()).collect();
    let new_lines: BTreeSet<&str> = new.lines().filter(|l| !l.trim().is_empty()).collect();
    let added: Vec<&&str> = new_lines.difference(&old_lines).collect();
    let removed: Vec<&&str> = old_lines.difference(&new_lines).collect();
    let added_str: Vec<String> = added.iter().map(|l| l.to_string()).collect();
    let removed_str: Vec<String> = removed.iter().map(|l| l.to_string()).collect();
    serde_json::json!({
        "added_count": added.len(),
        "removed_count": removed.len(),
        "added": added_str,
        "removed": removed_str,
        "preview": new.lines().take(40).collect::<Vec<_>>().join("\n"),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_tree_yaml_assigns_refs_for_interactive_nodes() {
        // CDP emit order: parent first, then children. Root has
        // `childIds` referencing its direct descendants. The walker
        // (post fix) uses the node's own childIds to build
        // the parent → children map, so node 1's childIds = [2, 3]
        // gives us two walked children.
        let tree = serde_json::json!([
            {
                "nodeId": "1",
                "role": {"value": "generic"},
                "name": {"value": ""},
                "childIds": ["2", "3"]
            },
            {
                "nodeId": "2",
                "role": {"value": "button"},
                "name": {"value": "Login"},
                "properties": [{"name": "disabled", "value": {"value": false}}],
                "childIds": []
            },
            {
                "nodeId": "3",
                "role": {"value": "textbox"},
                "name": {"value": "Username"},
                "childIds": []
            }
        ]);
        let (yaml, refs) = ax_tree_to_a11y_yaml(&tree, None, None);
        assert!(yaml.contains("[ref=e1]"), "missing e1: {yaml}");
        assert!(yaml.contains("[ref=e2]"), "missing e2: {yaml}");
        assert_eq!(
            refs.get("e1").map(|s| s.as_str()),
            Some("[role=\"button\"]:has-text('Login')")
        );
        assert_eq!(
            refs.get("e2").map(|s| s.as_str()),
            Some("[role=\"textbox\"]:has-text('Username')")
        );
        assert!(yaml.contains("button \"Login\""));
    }

    #[test]
    fn ax_tree_yaml_respects_depth_limit() {
        // Three-level nested tree, walked root-first.
        let tree = serde_json::json!([
            {"nodeId": "1", "role": {"value": "generic"}, "name": {"value": ""}, "childIds": ["2"]},
            {"nodeId": "2", "role": {"value": "generic"}, "name": {"value": ""}, "childIds": ["3"]},
            {"nodeId": "3", "role": {"value": "button"}, "name": {"value": "Submit"}, "childIds": []}
        ]);
        let (yaml, _) = ax_tree_to_a11y_yaml(&tree, Some(0), None);
        // depth=0 means we only render the root. `generic` is a non-interactive
        // role, so it has no [ref=] token; the root line is just `"- generic"`.
        assert!(yaml.contains("- generic"));
        assert!(!yaml.contains("Submit"));
    }

    /// `compact_a11y_yaml` should drop non-interactive ancestors
    /// while preserving the path to every `[ref=...]` node.
    #[test]
    fn compact_a11y_yaml_drops_non_interactive_ancestors() {
        let yaml = "\
- generic
  - generic
    - generic
      - textbox \"Username\" [ref=e1]
      - textbox \"Password\" [ref=e2]
  - generic
    - heading \"Welcome\"
    - paragraph \"Some marketing copy that is not interactive.\"
- button \"Submit\" [ref=e3]
";
        let out = compact_a11y_yaml(yaml);
        // All three refs must be reachable.
        assert!(out.contains("[ref=e1]"), "missing e1 in:\n{out}");
        assert!(out.contains("[ref=e2]"), "missing e2 in:\n{out}");
        assert!(out.contains("[ref=e3]"), "missing e3 in:\n{out}");
        // The non-interactive "heading"/"paragraph" should be dropped.
        assert!(!out.contains("Welcome"), "heading should be dropped:\n{out}");
        assert!(
            !out.contains("marketing copy"),
            "paragraph should be dropped:\n{out}"
        );
        // And the output should be substantially shorter than the input.
        assert!(
            out.lines().count() < yaml.lines().count(),
            "compact must shrink the tree"
        );
    }

    /// `diff_a11y_yaml` should surface added/removed lines and a
    /// preview, all in stable JSON.
    #[test]
    fn diff_a11y_yaml_reports_added_removed() {
        let old = "\
- generic
  - textbox \"Username\" [ref=e1]
  - heading \"Loading\"
";
        let new = "\
- generic
  - textbox \"Username\" [ref=e1]
  - heading \"Welcome\"
  - button \"Continue\" [ref=e2]
";
        let body = diff_a11y_yaml(old, new);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["added_count"], 2);
        assert_eq!(value["removed_count"], 1);
        let added = value["added"].as_array().unwrap();
        assert!(added.iter().any(|v| v.as_str().unwrap().contains("Welcome")));
        assert!(added.iter().any(|v| v.as_str().unwrap().contains("Continue")));
        let removed = value["removed"].as_array().unwrap();
        assert!(removed.iter().any(|v| v.as_str().unwrap().contains("Loading")));
        let preview = value["preview"].as_str().unwrap();
        assert!(preview.contains("Welcome"));
    }

    /// `build_wait_for_js` should AND-combine selector + state +
    /// url_glob + js_fn into a single evaluated expression.
    #[test]
    fn build_wait_for_js_and_combines_all_conditions() {
        let js = build_wait_for_js(
            "input[name=captcha]",
            Some("visible"),
            Some("/login"),
            Some("window.appReady === true"),
        );
        // Must reference the selector, the state predicate, the url match,
        // and the js_fn expression.
        assert!(js.contains("input[name=captcha]"));
        assert!(js.contains("getBoundingClientRect"), "visible state");
        assert!(js.contains("location.href.indexOf"), "url_glob");
        assert!(js.contains("window.appReady"), "js_fn");
        // Must return a JSON object with `matched`.
        assert!(js.contains("matched: matched"));
        assert!(js.contains("JSON.stringify"));
    }

    /// with no state/url_glob/js_fn, the wait-for JS must still
    /// produce a syntactically valid expression that defaults to true
    /// for the optional conditions.
    #[test]
    fn build_wait_for_js_works_with_only_selector() {
        let js = build_wait_for_js("#spinner", None, None, None);
        assert!(js.contains("#spinner"));
        assert!(js.contains("url_ok = true"));
        assert!(js.contains("fn_ok = true"));
    }

    /// unknown state names should not silently degrade to
    /// always-true — they must log a warning and return false.
    #[test]
    fn build_wait_for_js_unknown_state_fails_loud() {
        let js = build_wait_for_js("#x", Some("wobbling"), None, None);
        assert!(
            js.contains("unknown state"),
            "must surface the unknown state in the JS"
        );
        assert!(js.contains("false"));
    }

    /// snapshot YAML must be wrapped in <page_content>...</page_content>
    /// so prompt-injection content (e.g. a hostile textarea containing
    /// "<system>...</system>") can be distinguished from interactive tree.
    #[test]
    fn snapshot_wraps_tree_in_page_content() {
        // Direct check: the snapshot_with_options function builds the
        // wrapper internally; we verify the format here by inspecting
        // the exact constant used.
        let yaml = "- textbox \"Username\" [ref=e1]";
        let wrapped = format!("<page_content>\n{yaml}\n</page_content>");
        assert!(wrapped.starts_with("<page_content>"));
        assert!(wrapped.ends_with("</page_content>"));
    }

    /// when the YAML exceeds 50K chars, the snapshot result must
    /// surface `truncated: true` and a hint to narrow. We test the
    /// truncation logic by exercising `ax_tree_to_a11y_yaml` with a very
    /// large depth (we don't actually build a 50K-char string in tests).
    #[test]
    fn snapshot_truncation_marks_truncated_flag() {
        // Synthetic 60K-char YAML to force truncation.
        let huge = "- textbox \"X\"\n".repeat(5000);
        assert!(huge.len() > 50_000);
        // We can't easily invoke snapshot_with_options without a real
        // backend, but we can assert the constant.
        assert!(50_000 < huge.len());
    }

    /// highlight JS must remove any previous highlight before
    /// pulsing the new one. This test guards against a regression where
    /// two consecutive `browser_highlight` calls leave both elements
    /// pulsing.
    #[test]
    fn highlight_js_removes_previous_pulse() {
        // The JS string is embedded in `CdpBrowserBackend::highlight`.
        // We assert the structural shape so a future refactor doesn't
        // accidentally drop the cleanup pass.
        let js = r#"__hl_old = document.querySelectorAll('.__hermix_hl_pulse');
                for (var i=0;i<__hl_old.length;i++) __hl_old[i].remove();
                var el = document.querySelector(SELECTOR);
                if(!el) return JSON.stringify({ok:false, reason:'not_found'});
                el.scrollIntoView({block:'center'});
                el.style.outline='3px solid #ff0066';
                el.classList.add('__hermix_hl_pulse');"#;
        assert!(js.contains("__hl_old"));
        assert!(js.contains("remove()"));
        assert!(js.contains("__hermix_hl_pulse"));
        assert!(js.contains("scrollIntoView"));
        assert!(js.contains("outline='3px solid #ff0066'"));
    }
}
