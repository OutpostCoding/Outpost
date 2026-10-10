// ── surface: backend ──────────────────────────────────
// Godot 4 .tscn parser for godot_scene_tree — pure, fixture-tested, no Godot install.
// Exports: parse_scene, SceneFile, NodeDef, … · Related: tools/mod.rs

//! Godot 4 scene (.tscn) parsing for the `godot_scene_tree` tool.
//!
//! A .tscn file is a line-oriented text format (Godot 4 writes "format 3"):
//! `[section]` headers carry key="value" attributes, and property lines
//! `key = value` follow a section until the next `[section]`. Node headers
//! carry a `parent` attribute that is the FULL path from the root (e.g.
//! `parent="Inn/Interior"`), which is what lets us reconstruct the real tree
//! — the everyday failure mode the tool exists to kill is `get_node("Innkeeper")`
//! when the real path is `Inn/Interior/Innkeeper`.
//!
//! Everything here is pure text-in → structs-out; the tool arm in
//! `tools/mod.rs` handles path sandboxing and reads the file. No Godot
//! installation is required — this mirrors the on-disk format exactly.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Default output cap: a huge scene must not blow the model's context. The
/// model can raise it explicitly via `max_chars` (see [`SceneArgs`]).
const DEFAULT_MAX_CHARS: usize = 12_000;
const MAX_CHARS_CEILING: usize = 100_000;
/// Hard stop on rendered nodes so a pathological scene can't loop forever.
const NODE_CAP: usize = 1500;

// ── Parsed model ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SceneFile {
    pub format: u32,
    pub load_steps: Option<u32>,
    pub uid: Option<String>,
    pub ext_resources: Vec<ExtResource>,
    pub sub_resources: Vec<SubResource>,
    pub nodes: Vec<NodeDef>,
    pub connections: Vec<ConnectionDef>,
    /// Paths marked `[editable path=…]` — instanced scenes whose children may
    /// be overridden in this file.
    pub editable: Vec<String>,
}

#[derive(Debug)]
pub struct ExtResource {
    pub id: String,
    pub kind: String,
    pub path: String,
}

#[derive(Debug)]
pub struct SubResource {
    pub id: String,
    pub kind: String,
    pub props: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct NodeDef {
    pub name: String,
    /// Absent for instanced nodes — their type comes from the instanced scene.
    pub kind: Option<String>,
    /// Full path from the root (root nodes have no `parent` attribute).
    pub parent: Option<String>,
    /// ExtResource id when this node is an instanced sub-scene.
    pub instance: Option<String>,
    /// Raw property values, in file order (resolved at render time).
    pub props: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct ConnectionDef {
    pub signal: String,
    pub from: String,
    pub to: String,
    pub method: String,
}

// ── Tool arguments ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SceneArgs {
    /// .tscn path relative to the project root (`res://` already stripped).
    pub scene: String,
    /// Render nodes only down to this depth (root = 0); deeper subtrees are
    /// collapsed to a count. `None` = unlimited.
    pub max_depth: Option<usize>,
    /// Include per-node property values (default true).
    pub props: bool,
    /// Output cap in chars (default [`DEFAULT_MAX_CHARS`], ceiling
    /// [`MAX_CHARS_CEILING`]).
    pub max_chars: usize,
}

/// Parse the tool call's JSON arguments. Pure so it's unit-testable.
pub fn parse_godot_args(arguments: &str) -> Result<SceneArgs, String> {
    if arguments.trim().is_empty() {
        return Err(
            "godot_scene_tree was called with empty arguments — a malformed tool call. \
             Re-issue it with 'scene'."
                .into(),
        );
    }
    let v: Value = serde_json::from_str(arguments).map_err(|_| {
        "godot_scene_tree requires a JSON object argument with 'scene' — the .tscn path \
         relative to the project, or a res:// path."
            .to_string()
    })?;
    let raw_scene = ["scene", "path", "file"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(
            "godot_scene_tree requires a 'scene' argument: the .tscn path relative to the \
             project (or a res:// path).",
        )?;
    let trimmed = raw_scene.trim().trim_matches('"').trim_matches('\'').trim();
    let stripped = trimmed.strip_prefix("res://").unwrap_or(trimmed);
    let clean = stripped.trim_start_matches(['/', '\\']);
    if !clean.ends_with(".tscn") {
        return Err(format!(
            "'{raw_scene}' doesn't end in .tscn — godot_scene_tree reads Godot scene files."
        ));
    }
    let scene = clean.to_string();
    let max_depth = v.get("max_depth").and_then(Value::as_u64).map(|d| d as usize);
    let props = v.get("props").and_then(Value::as_bool).unwrap_or(true);
    let max_chars = v
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|d| d as usize)
        .unwrap_or(DEFAULT_MAX_CHARS)
        .clamp(1_000, MAX_CHARS_CEILING);
    Ok(SceneArgs {
        scene,
        max_depth,
        props,
        max_chars,
    })
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Which section a property line currently belongs to.
#[derive(PartialEq)]
enum Section {
    None,
    Node,
    SubResource,
}

/// Parse a .tscn file into its sections. Errors carry 1-based line numbers.
pub fn parse_scene(text: &str) -> Result<SceneFile, String> {
    let mut scene = SceneFile::default();
    let mut section = Section::None;
    let mut current: Option<usize> = None; // index into nodes / sub_resources
    // A property value that didn't balance on its first line — keeps growing
    // across lines until its brackets close (Godot wraps long arrays).
    let mut pending: Option<(usize, String, String)> = None;

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if let Some((start_line, key, mut value)) = pending.take() {
            value.push('\n');
            value.push_str(line);
            if !balanced(&value) {
                // Unterminated at the next section header = malformed file
                // (a valid continuation never contains a new section).
                if line.trim_start().starts_with('[') {
                    return Err(format!(
                        "line {start_line}: unterminated value for '{key}' — brackets never \
                         closed before the next section."
                    ));
                }
                pending = Some((start_line, key, value));
                i += 1;
                continue;
            }
            commit_prop(&mut scene, &section, &mut current, &key, &value)
                .map_err(|e| format!("line {start_line}: {e}"))?;
            i += 1;
            continue;
        }

        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        if t.starts_with('[') {
            if !t.ends_with(']') {
                return Err(format!("line {}: malformed section header: {t:?}", i + 1));
            }
            let inner = &t[1..t.len() - 1];
            let (tag, attrs) = parse_header(inner).map_err(|e| format!("line {}: {e}", i + 1))?;
            match tag.as_str() {
                "gd_scene" => {
                    scene.format = attr(&attrs, "format")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(3);
                    scene.load_steps = attr(&attrs, "load_steps")
                        .and_then(|v| v.parse().ok());
                    scene.uid = attr(&attrs, "uid").map(str::to_string);
                    section = Section::None;
                    current = None;
                }
                "ext_resource" => {
                    let (Some(id), Some(path)) = (attr(&attrs, "id"), attr(&attrs, "path")) else {
                        return Err(format!(
                            "line {}: [ext_resource] needs 'path' and 'id' attributes.",
                            i + 1
                        ));
                    };
                    scene.ext_resources.push(ExtResource {
                        id: id.to_string(),
                        kind: attr(&attrs, "type").unwrap_or("Resource").to_string(),
                        path: path.to_string(),
                    });
                    section = Section::None;
                    current = None;
                }
                "sub_resource" => {
                    let (Some(id), Some(kind)) = (attr(&attrs, "id"), attr(&attrs, "type")) else {
                        return Err(format!(
                            "line {}: [sub_resource] needs 'type' and 'id' attributes.",
                            i + 1
                        ));
                    };
                    scene.sub_resources.push(SubResource {
                        id: id.to_string(),
                        kind: kind.to_string(),
                        props: Vec::new(),
                    });
                    current = Some(scene.sub_resources.len() - 1);
                    section = Section::SubResource;
                }
                "node" => {
                    let name = attr(&attrs, "name").ok_or_else(|| {
                        format!("line {}: [node] header missing 'name'.", i + 1)
                    })?;
                    let instance = attr(&attrs, "instance").and_then(instance_id);
                    scene.nodes.push(NodeDef {
                        name: name.to_string(),
                        kind: attr(&attrs, "type").map(str::to_string),
                        parent: attr(&attrs, "parent").map(str::to_string),
                        instance,
                        props: Vec::new(),
                    });
                    current = Some(scene.nodes.len() - 1);
                    section = Section::Node;
                }
                "connection" => {
                    let (Some(signal), Some(from), Some(to), Some(method)) = (
                        attr(&attrs, "signal"),
                        attr(&attrs, "from"),
                        attr(&attrs, "to"),
                        attr(&attrs, "method"),
                    ) else {
                        return Err(format!(
                            "line {}: [connection] needs 'signal', 'from', 'to' and 'method'.",
                            i + 1
                        ));
                    };
                    scene.connections.push(ConnectionDef {
                        signal: signal.to_string(),
                        from: from.to_string(),
                        to: to.to_string(),
                        method: method.to_string(),
                    });
                    section = Section::None;
                    current = None;
                }
                "editable" => {
                    if let Some(p) = attr(&attrs, "path") {
                        scene.editable.push(p.to_string());
                    }
                    section = Section::None;
                    current = None;
                }
                // Unknown sections (plugins, custom resources) — skip their
                // header; their property lines get consumed and discarded by
                // the Section::None error path below. Tolerated rather than
                // fatal so a scene with a niche section still inspects.
                _ => {
                    section = Section::None;
                    current = None;
                }
            }
            i += 1;
            continue;
        }

        // Property line: `key = value` (split at the FIRST `=`, so values may
        // contain `=` inside strings/dicts).
        if let Some(eq) = t.find('=') {
            let key = t[..eq].trim().to_string();
            if key.is_empty() {
                i += 1;
                continue;
            }
            let value = t[eq + 1..].trim().to_string();
            if !balanced(&value) {
                pending = Some((i + 1, key, value));
            } else {
                commit_prop(&mut scene, &section, &mut current, &key, &value)
                    .map_err(|e| format!("line {}: {e}", i + 1))?;
            }
            i += 1;
            continue;
        }
        // Junk line (Godot writes oddities into scenes occasionally) — ignore.
        i += 1;
    }
    if let Some((start_line, key, _)) = pending {
        return Err(format!(
            "line {start_line}: unterminated value for '{key}' — brackets never closed."
        ));
    }
    Ok(scene)
}

/// Attach a property line to whatever section is open.
fn commit_prop(
    scene: &mut SceneFile,
    section: &Section,
    current: &mut Option<usize>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    match section {
        Section::Node => {
            let idx = current.ok_or("internal: node property with no node open")?;
            scene.nodes[idx].props.push((key.to_string(), value.to_string()));
        }
        Section::SubResource => {
            let idx = current.ok_or("internal: sub_resource property with no sub open")?;
            scene.sub_resources[idx].props.push((key.to_string(), value.to_string()));
        }
        Section::None => {
            return Err(format!("property '{key}' appears outside any section"));
        }
    }
    Ok(())
}

/// Pull an attribute by name from a parsed header.
fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Parse the inside of a `[ … ]` header into `(tag, attributes)`. Attributes
/// are `key="quoted value"` or bare tokens like `instance=ExtResource("2_abc")`
/// (no spaces, so they read as one token). The first bare token is the tag.
fn parse_header(s: &str) -> Result<(String, Vec<(String, String)>), String> {
    let mut attrs = Vec::new();
    let mut tag = String::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        // End of the key: first whitespace OR `=`, whichever comes first.
        let key_end = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let token = rest[..key_end].trim();
        if token.is_empty() {
            rest = rest[key_end..].trim_start();
            continue;
        }
        if rest[key_end..].starts_with('=') {
            // `key = value`
            let key = token.to_string();
            rest = rest[key_end + 1..].trim_start();
            let (value, after) = if rest.starts_with('"') {
                read_quoted(rest)?
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                (rest[..end].to_string(), &rest[end..])
            };
            attrs.push((key, value));
            rest = after;
        } else {
            // Bare token — the section tag (first one only; later bare tokens
            // are tolerated and skipped).
            if tag.is_empty() {
                tag = token.to_string();
            }
            rest = rest[key_end..].trim_start();
        }
    }
    Ok((tag, attrs))
}

/// Read a `"…"` string at the start of `s`, handling `\"` / `\\` escapes.
/// Returns (unescaped value, remainder after the closing quote).
fn read_quoted(s: &str) -> Result<(String, &str), String> {
    debug_assert!(s.starts_with('"'));
    let mut out = String::new();
    let inner = &s[1..];
    let mut chars = inner.char_indices();
    let mut end = None;
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, c2)) => out.push(c2),
                None => return Err("unterminated escape in quoted string".into()),
            },
            '"' => {
                end = Some(i);
                break;
            }
            _ => out.push(c),
        }
    }
    let Some(end) = end else {
        return Err("unterminated quoted string".into());
    };
    // `end` is relative to `inner`; skip the closing quote in the original.
    Ok((out, &s[end + 2..]))
}

/// Pull the id out of `ExtResource("2_abc")` / `SubResource("…")` (the two
/// forms the `instance` attribute can take).
fn instance_id(v: &str) -> Option<String> {
    for open in ["ExtResource(\"", "SubResource(\""] {
        if let Some(rest) = v.strip_prefix(open) {
            if let Some(id) = rest.strip_suffix("\")") {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Whether a value's brackets all balance (ignoring brackets inside quoted
/// strings, so `texture_filter("a(b")` doesn't confuse the counter).
fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

// ── Tree reconstruction ──────────────────────────────────────────────────────

struct TreeNode<'a> {
    def: &'a NodeDef,
    path: String,
    children: Vec<TreeNode<'a>>,
}

/// Build the node tree from `parent=` references. Returns (roots, orphans)
/// where orphans are nodes whose parent path doesn't exist in the file (or
/// that a parent cycle made unreachable).
fn build_tree<'a>(scene: &'a SceneFile) -> (Vec<TreeNode<'a>>, Vec<&'a NodeDef>) {
    // Godot writes parent references as "." (root), the bare root name, or
    // root-relative paths ("Interior", "Interior/Innkeeper"). Some tools write
    // root-qualified paths ("Inn/Interior"). Normalize all of them to full
    // root-qualified paths so children match their parents' paths.
    let root_name = scene
        .nodes
        .iter()
        .find(|n| n.parent.is_none())
        .map(|n| n.name.as_str());
    let resolve = |parent: &str| -> String {
        match root_name {
            Some(root) if parent == "." || parent == root => root.to_string(),
            Some(root) if parent.starts_with(&format!("{root}/")) => parent.to_string(),
            Some(root) => format!("{root}/{parent}"),
            None => parent.to_string(),
        }
    };
    let paths: Vec<String> = scene
        .nodes
        .iter()
        .map(|n| match &n.parent {
            Some(p) => format!("{}/{}", resolve(p), n.name),
            None => n.name.clone(),
        })
        .collect();
    let mut children_of: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        if let Some(p) = &n.parent {
            children_of.entry(resolve(p)).or_default().push(i);
        }
    }
    let mut visited: HashSet<usize> = HashSet::new();
    fn build<'a>(
        idx: usize,
        nodes: &'a [NodeDef],
        paths: &[String],
        children_of: &HashMap<String, Vec<usize>>,
        visited: &mut HashSet<usize>,
    ) -> TreeNode<'a> {
        let mut children = Vec::new();
        if let Some(cs) = children_of.get(paths[idx].as_str()) {
            let mut cs = cs.clone();
            cs.sort_by(|&a, &b| nodes[a].name.cmp(&nodes[b].name));
            for c in cs {
                if visited.insert(c) {
                    children.push(build(c, nodes, paths, children_of, visited));
                }
            }
        }
        TreeNode {
            def: &nodes[idx],
            path: paths[idx].clone(),
            children,
        }
    }
    let mut roots = Vec::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        if n.parent.is_none() && visited.insert(i) {
            roots.push(build(i, &scene.nodes, &paths, &children_of, &mut visited));
        }
    }
    let orphans: Vec<&NodeDef> = scene
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| !visited.contains(i))
        .map(|(_, n)| n)
        .collect();
    (roots, orphans)
}

fn subtree_size(node: &TreeNode) -> usize {
    1 + node.children.iter().map(subtree_size).sum::<usize>()
}

// ── Renderer ─────────────────────────────────────────────────────────────────

/// Parse `text` and render it as a resolved tree. Returns the text for the
/// model (possibly capped at `args.max_chars` with a truncation note).
pub fn render_scene(text: &str, args: &SceneArgs) -> Result<String, String> {
    let scene = parse_scene(text)?;
    let ext: HashMap<&str, &ExtResource> = scene
        .ext_resources
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let (roots, orphans) = build_tree(&scene);
    let mut r = Renderer {
        args,
        ext,
        editable: &scene.editable,
        out: String::new(),
        nodes_shown: 0,
    };

    r.push(&format!(
        "{} — Godot 4 scene (format {}, {} nodes, {} ext_resource, {} sub_resource{})",
        args.scene,
        scene.format,
        scene.nodes.len(),
        scene.ext_resources.len(),
        scene.sub_resources.len(),
        scene
            .uid
            .as_ref()
            .map(|u| format!(", uid {u}"))
            .unwrap_or_default(),
    ));
    r.push("\n");
    if roots.is_empty() {
        r.push("(no root node found)\n");
    }
    for (i, root) in roots.iter().enumerate() {
        let last = i == roots.len() - 1;
        r.render_subtree(root, "", "", last, 0);
    }
    if !orphans.is_empty() {
        r.push("\n(orphans — parent not found in this file)\n");
        for o in &orphans {
            r.push(&format!(
                "  {} ({})\n",
                o.name,
                o.kind.as_deref().unwrap_or("instanced")
            ));
        }
    }
    if r.nodes_shown > NODE_CAP {
        r.push(&format!(
            "\n[more than {NODE_CAP} nodes — the rest were not shown]\n"
        ));
    }

    // Appendix: resources + connections.
    if !scene.ext_resources.is_empty() || !scene.sub_resources.is_empty() {
        r.push("\nResources:\n");
        for e in &scene.ext_resources {
            r.push(&format!("  ext {} ({}) → {}\n", e.id, e.kind, e.path));
        }
        for s in &scene.sub_resources {
            let props = if args.props && !s.props.is_empty() {
                let ps: Vec<String> = s
                    .props
                    .iter()
                    .filter(|(k, _)| show_prop(k))
                    .map(|(k, v)| format!("{k}={}", resolve_value(v, &r.ext)))
                    .collect();
                if ps.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", ps.join(" "))
                }
            } else {
                String::new()
            };
            r.push(&format!("  sub {} ({}){}\n", s.id, s.kind, props));
        }
    }
    if !scene.connections.is_empty() {
        r.push("\nConnections:\n");
        let root_name = scene
            .nodes
            .iter()
            .find(|n| n.parent.is_none())
            .map(|n| n.name.as_str())
            .unwrap_or(".");
        for c in &scene.connections {
            let to = if c.to == "." { root_name } else { c.to.as_str() };
            r.push(&format!(
                "  {}: {} → {}.{}\n",
                c.signal, c.from, to, c.method
            ));
        }
    }
    if !scene.editable.is_empty() {
        r.push(&format!("\n[editable paths: {}]\n", scene.editable.join(", ")));
    }

    // Cap. Truncation keeps the newest material (the tree) and cuts the tail
    // (resources/connections) — the right thing to lose when a scene is huge.
    let mut out = r.out;
    let total_chars = out.chars().count();
    if total_chars > args.max_chars {
        let t: String = out.chars().take(args.max_chars).collect();
        if total_chars <= MAX_CHARS_CEILING {
            let suggest_cap = ((total_chars + 999) / 1000) * 1000;
            out = format!(
                "{t}\n\n[output truncated at {} chars ({total_chars} total chars) — pass max_chars: {suggest_cap} to see the full scene tree, or use max_depth to inspect specific subtree depths]",
                args.max_chars
            );
        } else {
            out = format!(
                "{t}\n\n[output truncated at {} chars ({total_chars} total chars) — scene exceeds {MAX_CHARS_CEILING} char ceiling. Pass max_depth to collapse subtrees or props: false for the skeleton.]",
                args.max_chars
            );
        }
    }
    Ok(out)
}

/// Properties that add signal: skip Godot's internal `_`-prefixed keys and
/// the `metadata/` tree (both are noise the model doesn't need to see).
fn show_prop(key: &str) -> bool {
    !key.starts_with('_') && !key.starts_with("metadata/")
}

/// Resolve `ExtResource("id")` references inside a value to their res:// path,
/// and `SubResource("id")` to the bare id (the appendix lists the sub's type
/// and properties) — the two facts the model actually needs to act on.
fn resolve_value(v: &str, ext: &HashMap<&str, &ExtResource>) -> String {
    let mut out = String::new();
    let mut rest = v;
    while let Some(start) = rest.find("ExtResource(\"") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "ExtResource(\"".len()..];
        match after.find("\")") {
            Some(end) => {
                let id = &after[..end];
                match ext.get(id) {
                    Some(e) => out.push_str(&e.path),
                    None => out.push_str(&format!("<missing ext_resource {id}>")),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("ExtResource(\"");
                rest = after;
            }
        }
    }
    // SubResource("id") → bare id (no lookup needed; type lives in the appendix).
    let mut rest = rest;
    let mut out2 = String::new();
    while let Some(start) = rest.find("SubResource(\"") {
        out2.push_str(&rest[..start]);
        let after = &rest[start + "SubResource(\"".len()..];
        match after.find("\")") {
            Some(end) => {
                out2.push_str(&after[..end]);
                rest = &after[end + 2..];
            }
            None => {
                out2.push_str("SubResource(\"");
                rest = after;
            }
        }
    }
    out2.push_str(rest);
    out.push_str(&out2);
    out
}

struct Renderer<'a> {
    args: &'a SceneArgs,
    ext: HashMap<&'a str, &'a ExtResource>,
    /// `[editable path=…]` entries — instanced scenes with local overrides.
    editable: &'a [String],
    out: String,
    nodes_shown: usize,
}

impl<'a> Renderer<'a> {
    fn push(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn node_kind(&self, def: &NodeDef) -> String {
        if let Some(k) = &def.kind {
            k.clone()
        } else if let Some(inst) = &def.instance {
            match self.ext.get(inst.as_str()) {
                Some(e) => format!("instance of {}", e.path),
                None => format!("instance of <missing ext_resource {inst}>"),
            }
        } else {
            "node".into()
        }
    }

    fn script_path(&self, def: &NodeDef) -> Option<String> {
        def.props
            .iter()
            .find(|(k, _)| k == "script")
            .map(|(_, v)| resolve_value(v, &self.ext))
    }

    fn render_subtree(
        &mut self,
        node: &TreeNode,
        prefix: &str,
        child_prefix: &str,
        last: bool,
        depth: usize,
    ) {
        if self.nodes_shown >= NODE_CAP {
            return;
        }
        self.nodes_shown += 1;

        let connector = if last { "└─ " } else { "├─ " };
        let mut line = format!("{prefix}{connector}{} ({})", node.def.name, self.node_kind(node.def));
        if let Some(s) = self.script_path(node.def) {
            line.push_str(&format!(" script={s}"));
        }
        if self.editable.iter().any(|p| p == &node.path) {
            line.push_str(" [editable]");
        }
        self.push(&line);
        self.push("\n");

        // Props: on an indented line under the node, so node names stay greppable.
        if self.args.props {
            let ps: Vec<String> = node
                .def
                .props
                .iter()
                .filter(|(k, _)| show_prop(k) && k != "script")
                .map(|(k, v)| format!("{k}={}", resolve_value(v, &self.ext)))
                .collect();
            if !ps.is_empty() {
                let prop_prefix = format!("{child_prefix}   ");
                self.push(&format!("{prop_prefix}props: {}\n", ps.join(" ")));
            }
        }

        // Depth limit: collapse deeper subtrees to a count.
        if let Some(max) = self.args.max_depth {
            if depth >= max && !node.children.is_empty() {
                let n = node.children.iter().map(subtree_size).sum::<usize>();
                self.push(&format!("{child_prefix}   … {n} more node(s) below\n"));
                return;
            }
        }
        for (i, child) in node.children.iter().enumerate() {
            let last_child = i == node.children.len() - 1;
            let cprefix = if last {
                format!("{child_prefix}   ")
            } else {
                format!("{child_prefix}│  ")
            };
            self.render_subtree(child, &cprefix, &cprefix, last_child, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[gd_scene load_steps=6 format=3 uid="uid://bq1abc12345"]

[ext_resource type="Script" path="res://scripts/inn.gd" id="1_inn"]
[ext_resource type="Texture2D" uid="uid://d2xyz" path="res://art/innkeeper.png" id="2_tex"]
[ext_resource type="Script" path="res://scripts/innkeeper.gd" id="3_script"]
[ext_resource type="PackedScene" uid="uid://c3table" path="res://scenes/table.tscn" id="4_table"]

[sub_resource type="RectangleShape2D" id="RectangleShape2D_1"]
size = Vector2(32, 64)

[node name="Inn" type="Node2D"]
script = ExtResource("1_inn")

[node name="Interior" type="Node2D" parent="."]

[node name="Innkeeper" type="StaticBody2D" parent="Interior"]
position = Vector2(120, 80)
texture = ExtResource("2_tex")
shape = SubResource("RectangleShape2D_1")
script = ExtResource("3_script")

[node name="Table" parent="Interior" instance=ExtResource("4_table")]

[connection signal="body_entered" from="Innkeeper" to="Inn" method="_on_innkeeper_body_entered"]
"#;

    fn args(scene: &str) -> SceneArgs {
        SceneArgs {
            scene: scene.to_string(),
            max_depth: None,
            props: true,
            max_chars: DEFAULT_MAX_CHARS,
        }
    }

    #[test]
    fn parses_minimal_root_only_scene() {
        let s = parse_scene("[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n").unwrap();
        assert_eq!(s.format, 3);
        assert_eq!(s.nodes.len(), 1);
        assert_eq!(s.nodes[0].name, "Root");
        assert_eq!(s.nodes[0].kind.as_deref(), Some("Node2D"));
        assert!(s.nodes[0].parent.is_none());
    }

    #[test]
    fn parses_full_fixture_and_builds_resolved_tree() {
        let s = parse_scene(FIXTURE).unwrap();
        assert_eq!(s.format, 3);
        assert_eq!(s.ext_resources.len(), 4);
        assert_eq!(s.sub_resources.len(), 1);
        assert_eq!(s.nodes.len(), 4);
        assert_eq!(s.connections.len(), 1);
        assert_eq!(s.uid.as_deref(), Some("uid://bq1abc12345"));

        let out = render_scene(FIXTURE, &args("scenes/inn.tscn")).unwrap();
        // Root + hierarchy with resolved script.
        assert!(out.contains("scenes/inn.tscn — Godot 4 scene (format 3, 4 nodes, 4 ext_resource, 1 sub_resource"), "{out}");
        assert!(out.contains("Inn (Node2D) script=res://scripts/inn.gd"), "{out}");
        assert!(out.contains("└─ Interior (Node2D)"), "{out}");
        assert!(out.contains("Innkeeper (StaticBody2D) script=res://scripts/innkeeper.gd"), "{out}");
        // Props resolved: ext_resource → res:// path, sub_resource stays by id.
        let props_line = out
            .lines()
            .find(|l| l.contains("props:"))
            .expect("props line");
        assert!(props_line.contains("position=Vector2(120, 80)"), "{props_line}");
        assert!(props_line.contains("texture=res://art/innkeeper.png"), "{props_line}");
        assert!(props_line.contains("shape=RectangleShape2D_1"), "{props_line}");
        // Instanced node shows the referenced scene.
        assert!(out.contains("Table (instance of res://scenes/table.tscn)"), "{out}");
        // Appendix + connection.
        assert!(out.contains("ext 2_tex (Texture2D) → res://art/innkeeper.png"), "{out}");
        assert!(out.contains("sub RectangleShape2D_1 (RectangleShape2D)"), "{out}");
        assert!(out.contains("body_entered: Innkeeper → Inn._on_innkeeper_body_entered"), "{out}");
    }

    #[test]
    fn props_false_omits_props_but_keeps_scripts() {
        let mut a = args("scenes/inn.tscn");
        a.props = false;
        let out = render_scene(FIXTURE, &a).unwrap();
        assert!(!out.contains("props:"), "{out}");
        assert!(out.contains("script=res://scripts/inn.gd"), "{out}");
        assert!(!out.contains("position=Vector2(120, 80)"), "{out}");
    }

    #[test]
    fn max_depth_collapses_deeper_subtrees() {
        let mut a = args("scenes/inn.tscn");
        a.max_depth = Some(1);
        let out = render_scene(FIXTURE, &a).unwrap();
        assert!(out.contains("… 2 more node(s) below"), "{out}");
        assert!(
            !out.contains("Innkeeper (StaticBody2D)"),
            "depth 2 should be collapsed: {out}"
        );
    }

    #[test]
    fn max_chars_truncates_with_note() {
        let mut a = args("scenes/inn.tscn");
        a.max_chars = 200;
        let out = render_scene(FIXTURE, &a).unwrap();
        assert!(out.chars().count() <= 400, "capped output too long: {}", out.chars().count());
        assert!(out.contains("[output truncated at 200 chars"), "{out}");
    }

    #[test]
    fn multiline_array_value_is_reassembled() {
        let s = r#"[gd_scene format=3]

[sub_resource type="Curve2D" id="Curve2D_1"]
points = PackedVector2Array(
0, 0,
10, 0,
10, 10
)

[node name="Root" type="Path2D"]
curve = SubResource("Curve2D_1")
"#;
        let parsed = parse_scene(s).unwrap();
        let props = &parsed.sub_resources[0].props;
        assert_eq!(props.len(), 1);
        assert_eq!(
            props[0].1,
            "PackedVector2Array(\n0, 0,\n10, 0,\n10, 10\n)"
        );
        let out = render_scene(s, &args("scenes/path.tscn")).unwrap();
        assert!(out.contains("curve=Curve2D_1"), "{out}");
    }

    #[test]
    fn orphan_nodes_are_flagged() {
        let s = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n\n[node name=\"Ghost\" type=\"Sprite2D\" parent=\"Root/Missing\"]\n";
        let out = render_scene(s, &args("scenes/orphan.tscn")).unwrap();
        assert!(out.contains("(orphans — parent not found in this file)"), "{out}");
        assert!(out.contains("Ghost (Sprite2D)"), "{out}");
    }

    #[test]
    fn unterminated_value_errors_with_line() {
        let s = "[gd_scene format=3]\n\n[sub_resource type=\"X\" id=\"X_1\"]\npoints = PackedVector2Array(0, 0,\n\n[node name=\"Root\" type=\"Node2D\"]\n";
        let e = parse_scene(s).unwrap_err();
        assert!(e.contains("line 4"), "{e}");
        assert!(e.contains("unterminated value for 'points'"), "{e}");
    }

    #[test]
    fn malformed_header_errors() {
        let e = parse_scene("[node name=\"x\"").unwrap_err();
        assert!(e.contains("line 1"), "{e}");
    }

    #[test]
    fn internal_and_metadata_props_are_hidden() {
        let s = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Node2D\"]\n_edit_lock_ = true\nmetadata/_custom = \"x\"\nvisible = true\n";
        let out = render_scene(s, &args("scenes/s.tscn")).unwrap();
        assert!(!out.contains("_edit_lock_"), "{out}");
        assert!(!out.contains("metadata/"), "{out}");
        assert!(out.contains("visible=true"), "{out}");
    }

    #[test]
    fn parse_godot_args_defaults_and_strips_res() {
        let a = parse_godot_args(r#"{"scene":"res://scenes/inn.tscn"}"#).unwrap();
        assert_eq!(a.scene, "scenes/inn.tscn");
        assert!(a.props);
        assert_eq!(a.max_chars, DEFAULT_MAX_CHARS);
        assert!(a.max_depth.is_none());

        let a = parse_godot_args(
            r#"{"path":"scenes/huge.tscn","props":false,"max_depth":2,"max_chars":50000}"#,
        )
        .unwrap();
        assert_eq!(a.scene, "scenes/huge.tscn");
        assert!(!a.props);
        assert_eq!(a.max_depth, Some(2));
        assert_eq!(a.max_chars, 50000);

        // Strips quotes and leading slashes
        let a = parse_godot_args(r#"{"scene":"\"/scenes/quoted.tscn\""}"#).unwrap();
        assert_eq!(a.scene, "scenes/quoted.tscn");
        let a = parse_godot_args(r#"{"scene":"'res://scenes/single.tscn'"}"#).unwrap();
        assert_eq!(a.scene, "scenes/single.tscn");

        // Ceilings and floors.
        let a = parse_godot_args(r#"{"scene":"a.tscn","max_chars":1}"#).unwrap();
        assert_eq!(a.max_chars, 1_000);
        let a = parse_godot_args(r#"{"scene":"a.tscn","max_chars":999999}"#).unwrap();
        assert_eq!(a.max_chars, MAX_CHARS_CEILING);
    }

    #[test]
    fn parse_godot_args_rejects_bad_calls() {
        assert!(parse_godot_args("").is_err());
        assert!(parse_godot_args(r#"{"scene":"res://scenes/inn.tscn"}"#).is_ok());
        assert!(parse_godot_args(r#"{"scene":"scenes/inn.png"}"#).is_err());
        assert!(parse_godot_args(r#"{"props":true}"#).is_err());
    }

    #[test]
    fn instance_id_extracts_both_forms() {
        assert_eq!(
            instance_id(r#"ExtResource("4_table")"#).as_deref(),
            Some("4_table")
        );
        assert_eq!(
            instance_id(r#"SubResource("X_1")"#).as_deref(),
            Some("X_1")
        );
        assert_eq!(instance_id("Node2D"), None);
    }
    /// Smoke-tests the parser against real Godot-authored `.tscn` files on disk:
    /// every `.tscn` under the `INN_DIR` project folder is parsed and rendered.
    /// Ignored by default, and skipped when `INN_DIR` is unset or not a folder.
    #[test]
    #[ignore = "set INN_DIR to a Godot project on disk to run"]
    fn parses_real_project_scenes() {
        let Some(dir) = std::env::var("INN_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
        else {
            eprintln!("skipping: set INN_DIR to a Godot project folder to run this test");
            return;
        };
        let mut scenes = Vec::new();
        collect_tscn(&dir, &mut scenes);
        assert!(!scenes.is_empty(), "no .tscn files under {}", dir.display());

        for path in &scenes {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let scene = parse_scene(&text)
                .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
            if path.file_name().and_then(|n| n.to_str()) == Some("inn.tscn") {
                assert_eq!(scene.nodes[0].name, "Inn", "{}", path.display());
                let out = render_scene(&text, &args("scenes/inn.tscn")).unwrap();
                assert!(out.contains("Inn (Node2D) script=res://scripts/inn.gd"), "{out}");
                assert!(
                    out.contains("Innkeeper (StaticBody2D) script=res://scripts/innkeeper.gd"),
                    "{out}"
                );
            }
        }
        eprintln!("parsed {} scene(s) OK from {}", scenes.len(), dir.display());
    }

    fn collect_tscn(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) != Some(".godot") {
                    collect_tscn(&p, out);
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some("tscn") {
                out.push(p);
            }
        }
    }
}
