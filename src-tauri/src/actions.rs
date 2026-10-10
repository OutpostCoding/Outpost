// ── surface: backend ──────────────────────────────────
// Prompt actions & scaffolded thinking — discovers, seeds, and lists custom markdown
// prompt actions from both global (%APPDATA%/actions) and workspace (.outpost/actions) folders.
// Exports: ActionInfo, list_actions, reveal_actions_dir · Related: commands.rs, Composer.tsx

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionInfo {
    pub name: String,
    pub description: String,
    pub template: String,
    pub path: String,
    pub scope: String, // "global" | "workspace"
}

const DEFAULT_RESEARCH: &str = r#"---
description: Multi-phase search, adversarial verification & cited synthesis
---
Conduct a rigorous, deep research investigation into the following topic or question:

$INPUT

Follow this scaffolded procedure:
1. **Scope & Hypotheses**: Clarify the core mechanics, key questions, and potential edge cases or misconceptions.
2. **Investigation & Evidence Gathering**: Use search and fetch tools to examine primary documentation, source code, or authoritative references.
3. **Adversarial Verification**: Actively search for contradictions, race conditions, edge-case failures, or caveats that challenge initial assumptions.
4. **Synthesis & Findings**: Explain how it works from first principles. Include concrete examples, architectural context, and exact citations for primary sources.
5. **Open Questions & Trade-offs**: Identify what remains undocumented, unresolved, or subject to engineering trade-offs.
"#;

const DEFAULT_AUDIT: &str = r#"---
description: Adversarial code & security review (bugs, races, edge cases)
---
Perform an adversarial code and security audit on the relevant files or staged changes:

$INPUT

Adopt the mindset of an adversarial security auditor and senior systems engineer. Do not merely summarize or praise the code. Actively scrutinize:
1. **Concurrency & Race Conditions**: Check locks, shared mutable state, async boundaries, cancel safety, and thread races.
2. **Resource Leaks & Lifecycles**: Verify memory allocation, open file handles, database connections, cleanup on drop/exit, and error unwind safety.
3. **Input Validation & Security Boundaries**: Look for injection vectors, path traversal, unchecked bounds, integer overflows, and unvalidated external data.
4. **Error Propagation & Silent Failures**: Find swallowed errors, missing logging, unhandled `None`/`Err` paths, and inconsistent error states.
5. **Concrete Recommendations**: For every issue found, explain the exact failure scenario and provide the minimal robust fix.
"#;

const DEFAULT_PLAN: &str = r#"---
description: Implementation plan with architecture, trade-offs & verification
---
Develop a comprehensive technical implementation plan for the following feature or change:

$INPUT

Structure your plan with rigorous engineering scrutiny:
1. **Problem Statement & Goals**: What are we solving, and what non-goals or constraints apply?
2. **Architecture & Component Boundaries**: Which modules, files, and data structures are affected? What contracts change?
3. **Alternatives & Trade-offs**: What alternate approaches exist, and why is the chosen design superior?
4. **Step-by-Step Implementation**: Break the work down into atomic, ordered steps (dependencies first).
5. **Verification & Failure Modes**: How will this change be verified (automated tests, manual validation), and how does it fail safely?
"#;

const DEFAULT_EXPLAIN: &str = r#"---
description: First-principles deep dive into mechanisms, mental models & pitfalls
---
Provide a first-principles, deep technical explanation of:

$INPUT

Structure your explanation to build a deep, intuitive mental model:
1. **Core Intuition & Mental Model**: What problem does this exist to solve, and what is the fundamental abstraction?
2. **Under the Hood Mechanics**: How does it actually work step-by-step at the systems/runtime level? Trace the data flow.
3. **Crucial Design Decisions**: Why is it designed this way rather than simpler alternatives? What trade-offs were accepted?
4. **Subtle Traps & Gotchas**: What are the non-obvious failure modes, performance cliffs, or misconceptions developers stumble into?
5. **Practical Code Walkthrough**: Provide a clear, real-world example demonstrating best practices.
"#;

const DEFAULT_DEBUG: &str = r#"---
description: Root-cause diagnostic matrix & hypothesis testing for bugs
---
Perform a root-cause diagnostic investigation into this issue:

$INPUT

Follow this diagnostic procedure before touching or suggesting fixes:
1. **Symptom vs. Cause**: Distinguish the observable failure from the underlying broken invariant.
2. **Hypothesis Matrix**: Formulate 3 distinct hypotheses for why this occurs, ranked by likelihood.
3. **Differential Verification**: For each hypothesis, what specific evidence (logs, variable state, code paths) would confirm or refute it?
4. **Targeted Inspection**: State exactly which files, functions, or commands need to be checked right now to isolate the cause.
5. **Minimal Root Fix & Prevention**: Address the core defect (not just the symptom) and explain how to prevent regressions.
"#;

const DEFAULT_TEST: &str = r#"---
description: Adversarial test suite & edge-case matrix generator
---
Design an exhaustive, adversarial test suite for:

$INPUT

Scaffold the test cases across these specific failure surfaces:
1. **Boundary & Extreme Inputs**: 0, -1, MAX_INT, empty collections, singletons, massive payloads, Unicode/multibyte strings, NaN.
2. **State & Lifecycle Violations**: Double-initialization, post-close operations, out-of-order calls, re-entrancy.
3. **Environmental & Fault Injection**: Network timeouts, disk write failures, partial reads, permission denied, aborts mid-operation.
4. **Invariant Assertions**: What algebraic properties or state invariants must hold true across *every* execution?
5. **Executable Test Code**: Provide idiomatic unit/integration tests ready to run in the project's native test runner.
"#;

const DEFAULT_COMMIT: &str = r#"---
description: Conventional commit message based on git diff and rationale
---
Inspect the staged git changes or current work:

$INPUT

Generate a conventional commit message adhering to these standards:
1. **Header**: `<type>(<scope>): <concise imperative summary>` (under 50 characters, no period).
2. **The "Why" Body**: Explain the context and motivation behind the change. What was broken or missing, and why was this approach chosen?
3. **The "How" Summary**: Bullet points of key technical changes (files/modules affected, edge cases handled).
4. **Breaking Changes / Notes**: Explicitly call out any breaking changes or required migrations.
"#;

const DEFAULT_PERF: &str = r#"---
description: Algorithmic & systems-level performance audit
---
Perform a performance and resource-utilization audit on:

$INPUT

Structure your analysis around systems-level realities:
1. **Complexity Analysis**: Time and space complexity (average and worst-case).
2. **Allocation & Memory Churn**: Look for unnecessary heap allocations, defensive copies, clone calls, or oversized buffer growth.
3. **I/O & Syscall Overhead**: Identify repeated disk/network reads, missing batching, unbuffered streams, or lock contention.
4. **Cache & Data Layout**: Evaluate memory locality and data structures for cache efficiency.
5. **High-ROI Recommendations**: Provide before/after benchmarks or code transformations ranked by impact.
"#;

const DEFAULT_ADR: &str = r#"---
description: Architecture Decision Record (ADR) with trade-offs & alternatives
---
Document an Architecture Decision Record (ADR) for:

$INPUT

Format the record using standard ADR structure:
1. **Title & Status**: Clear title and current state (Proposed / Accepted / Superseded).
2. **Context & Problem Statement**: What technical or business problem forces this decision? What constraints exist?
3. **Decision & Architecture**: What is the chosen technical approach, and how does it fit the system?
4. **Consequences & Trade-offs**:
   - **Positive**: What is gained (simplicity, throughput, maintainability)?
   - **Negative**: What is sacrificed or made harder?
5. **Alternatives Considered**: 2-3 alternative designs and the exact reasons they were rejected.
"#;

const DEFAULT_GRILL: &str = r#"---
description: Adversarial interview that stress-tests ideas and uncovers hidden flaws
---
Act as a skeptical, pragmatic Principal Engineer. I want to pitch you an idea, design, or plan:

$INPUT

Your goal is to challenge my assumptions and find the weak links before writing code:
1. **Fatal Assumptions**: What unverified assumptions am I making that could sink this?
2. **Edge Cases & Failure Modes**: Where does this design break down as usage scales 10x?
3. **The YAGNI / Simplicity Check**: What parts of this are over-engineered or premature? Can this be solved with half the code?
4. **Targeted Questions**: Ask me 3 sharp, difficult questions about the parts of the plan that are least defined.
"#;

/// Seed starter action templates into the global actions directory if it's empty.
pub fn seed_default_actions(dir: &Path) {
    if !dir.exists() {
        let _ = fs::create_dir_all(dir);
    }

    let starters = [
        ("research.md", DEFAULT_RESEARCH),
        ("audit.md", DEFAULT_AUDIT),
        ("plan.md", DEFAULT_PLAN),
        ("explain.md", DEFAULT_EXPLAIN),
        ("debug.md", DEFAULT_DEBUG),
        ("test.md", DEFAULT_TEST),
        ("commit.md", DEFAULT_COMMIT),
        ("perf.md", DEFAULT_PERF),
        ("adr.md", DEFAULT_ADR),
        ("grill.md", DEFAULT_GRILL),
    ];

    for (file, content) in starters {
        let path = dir.join(file);
        if !path.exists() {
            let _ = fs::write(&path, content);
        }
    }
}

/// Parse description and clean template from a markdown file with optional frontmatter.
pub fn parse_action_markdown(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    if trimmed.starts_with("---") {
        if let Some(end_idx) = trimmed[3..].find("---") {
            let frontmatter = &trimmed[3..3 + end_idx];
            let rest = trimmed[3 + end_idx + 3..].trim();
            for line in frontmatter.lines() {
                let l = line.trim();
                if let Some(desc) = l.strip_prefix("description:") {
                    let d = desc.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !d.is_empty() {
                        return (d, rest.to_string());
                    }
                }
            }
            return ("Custom prompt action".to_string(), rest.to_string());
        }
    }

    // Fallback: check first heading or non-empty line
    for line in trimmed.lines() {
        let l = line.trim();
        if let Some(heading) = l.strip_prefix("# ") {
            return (heading.trim().to_string(), trimmed.to_string());
        }
    }

    ("Custom prompt action".to_string(), trimmed.to_string())
}

/// Resolve the global actions directory: `%APPDATA%/actions`.
pub fn global_actions_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {e}"))?;
    Ok(data_dir.join("actions"))
}

/// Scan a directory for `*.md` action files.
fn scan_actions_in_dir(dir: &Path, scope: &str) -> Vec<ActionInfo> {
    let mut actions = Vec::new();
    if !dir.is_dir() {
        return actions;
    }

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return actions,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let name = stem.to_lowercase();
                if let Ok(raw) = fs::read_to_string(&path) {
                    let (description, template) = parse_action_markdown(&raw);
                    actions.push(ActionInfo {
                        name,
                        description,
                        template,
                        path: path.display().to_string(),
                        scope: scope.to_string(),
                    });
                }
            }
        }
    }

    actions
}

/// List all available prompt actions (global + workspace).
/// Workspace actions (.outpost/actions/*.md) override global actions of the same name.
#[tauri::command]
pub fn list_actions(
    app: AppHandle,
    session_root: Option<String>,
) -> Result<Vec<ActionInfo>, String> {
    let global_dir = global_actions_dir(&app)?;
    seed_default_actions(&global_dir);

    let mut actions_map = std::collections::HashMap::new();

    // 1. Scan global actions
    for act in scan_actions_in_dir(&global_dir, "global") {
        actions_map.insert(act.name.clone(), act);
    }

    // 2. Scan workspace actions if inside a project
    if let Some(root) = session_root {
        if !root.trim().is_empty() {
            let ws_dir = Path::new(&root).join(".outpost").join("actions");
            for act in scan_actions_in_dir(&ws_dir, "workspace") {
                // Workspace overrides global
                actions_map.insert(act.name.clone(), act);
            }
        }
    }

    let mut result: Vec<ActionInfo> = actions_map.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// Reveal the global actions directory in the OS file manager.
#[tauri::command]
pub fn reveal_actions_dir(app: AppHandle) -> Result<(), String> {
    let global_dir = global_actions_dir(&app)?;
    seed_default_actions(&global_dir);

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(&global_dir);
        c
    };

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&global_dir);
        c
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&global_dir);
        c
    };

    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_action_with_frontmatter() {
        let raw = r#"---
description: Test action description
---
Hello $INPUT world"#;
        let (desc, template) = parse_action_markdown(raw);
        assert_eq!(desc, "Test action description");
        assert_eq!(template, "Hello $INPUT world");
    }

    #[test]
    fn test_parse_action_fallback_heading() {
        let raw = "# My Great Action\n\nDo something useful with $INPUT";
        let (desc, template) = parse_action_markdown(raw);
        assert_eq!(desc, "My Great Action");
        assert_eq!(template, raw);
    }

    #[test]
    fn test_parse_action_default_fallback() {
        let raw = "Just some text without heading or frontmatter";
        let (desc, template) = parse_action_markdown(raw);
        assert_eq!(desc, "Custom prompt action");
        assert_eq!(template, raw);
    }

    #[test]
    fn test_all_default_starters_valid() {
        let starters = [
            ("research.md", DEFAULT_RESEARCH),
            ("audit.md", DEFAULT_AUDIT),
            ("plan.md", DEFAULT_PLAN),
            ("explain.md", DEFAULT_EXPLAIN),
            ("debug.md", DEFAULT_DEBUG),
            ("test.md", DEFAULT_TEST),
            ("commit.md", DEFAULT_COMMIT),
            ("perf.md", DEFAULT_PERF),
            ("adr.md", DEFAULT_ADR),
            ("grill.md", DEFAULT_GRILL),
        ];

        for (name, content) in starters {
            let (desc, template) = parse_action_markdown(content);
            assert!(
                !desc.is_empty() && desc != "Custom prompt action",
                "Starter {} missing valid frontmatter description",
                name
            );
            assert!(
                template.contains("$INPUT"),
                "Starter {} template must include $INPUT placeholder",
                name
            );
        }
    }
}
