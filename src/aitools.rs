//! Switching on `asqu event` in the AI tools a person uses, so each records the request
//! behind every change. Every tool gets the same generic command; only where its settings
//! live and what it calls its events differ. All taken from each tool's official docs
//! (checked 2026-10-18).
//!
//! Only Asqu's own entries are ever added or removed (any hook whose command runs
//! `asqu event`); everything else in those files is kept.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// How a tool's hook settings are laid out.
enum Format {
    /// `{"hooks": {"Event": [{"matcher": "...", "hooks": [{"type": "command", "command": "..."}]}]}}`
    /// (`wrapped: false` leaves out the outer `hooks` key.)
    Grouped { wrapped: bool },
    /// `{"version": 1, "hooks": {"event": [{"command": "..."}]}}` (version only if given).
    Flat { version: Option<i64> },
    /// A file of its own: `{"version": 1, "hooks": {"event": [{"type": "command", "bash": "..."}]}}`.
    CopilotFile,
    /// A file of its own: `{"version": "v1", "hooks": [{"name", "trigger", "action"}]}`.
    KiroFile,
}

pub struct AiTool {
    pub id: &'static str,
    pub name: &'static str,
    file: &'static str,
    format: Format,
    /// (the tool's event name, which tools it applies to if it asks, Asqu's kind)
    events: &'static [(&'static str, Option<&'static str>, &'static str)],
    /// A folder whose presence suggests the tool is used here.
    hint: &'static str,
}

const PROMPT_EDIT_DONE: &[(&str, Option<&str>, &str)] = &[
    ("UserPromptSubmit", None, "prompt"),
    ("PostToolUse", None, "edit"),
    ("Stop", None, "done"),
];

pub const TOOLS: &[AiTool] = &[
    AiTool {
        id: "claude-code",
        name: "Claude Code",
        file: ".claude/settings.local.json",
        format: Format::Grouped { wrapped: true },
        events: &[
            ("UserPromptSubmit", None, "prompt"),
            (
                "PostToolUse",
                Some("Edit|Write|MultiEdit|NotebookEdit"),
                "edit",
            ),
            ("Stop", None, "done"),
        ],
        hint: ".claude",
    },
    AiTool {
        id: "codex",
        name: "Codex",
        file: ".codex/hooks.json",
        format: Format::Grouped { wrapped: true },
        events: &[
            ("UserPromptSubmit", None, "prompt"),
            ("PostToolUse", Some("apply_patch|Edit|Write"), "edit"),
            ("Stop", None, "done"),
        ],
        hint: ".codex",
    },
    AiTool {
        id: "cursor",
        name: "Cursor",
        file: ".cursor/hooks.json",
        format: Format::Flat { version: Some(1) },
        events: &[
            ("beforeSubmitPrompt", None, "prompt"),
            ("afterFileEdit", None, "edit"),
            ("stop", None, "done"),
        ],
        hint: ".cursor",
    },
    AiTool {
        id: "gemini",
        name: "Gemini CLI",
        file: ".gemini/settings.json",
        format: Format::Grouped { wrapped: true },
        events: &[
            ("BeforeAgent", None, "prompt"),
            ("AfterTool", Some("write_file|replace"), "edit"),
            ("AfterAgent", None, "done"),
        ],
        hint: ".gemini",
    },
    AiTool {
        id: "copilot",
        name: "GitHub Copilot",
        file: ".github/hooks/asqu.json",
        format: Format::CopilotFile,
        events: &[
            ("userPromptSubmitted", None, "prompt"),
            ("postToolUse", None, "edit"),
            ("agentStop", None, "done"),
        ],
        hint: ".github/copilot",
    },
    AiTool {
        id: "windsurf",
        name: "Windsurf (Devin Desktop)",
        file: ".devin/hooks.json",
        format: Format::Flat { version: None },
        events: &[
            ("pre_user_prompt", None, "prompt"),
            ("post_write_code", None, "edit"),
            ("post_cascade_response", None, "done"),
        ],
        hint: ".windsurf",
    },
    AiTool {
        id: "kiro",
        name: "Kiro",
        file: ".kiro/hooks/asqu.json",
        format: Format::KiroFile,
        events: &[
            ("UserPromptSubmit", None, "prompt"),
            ("PostFileSave", None, "edit"),
            ("Stop", None, "done"),
        ],
        hint: ".kiro",
    },
    AiTool {
        id: "factory",
        name: "Factory Droid",
        file: ".factory/hooks.json",
        format: Format::Grouped { wrapped: false },
        events: PROMPT_EDIT_DONE,
        hint: ".factory",
    },
    AiTool {
        id: "qwen",
        name: "Qwen Code",
        file: ".qwen/settings.json",
        format: Format::Grouped { wrapped: true },
        events: PROMPT_EDIT_DONE,
        hint: ".qwen",
    },
];

/// AI tools with no way to run a command on events: covered by the AGENTS.md note and
/// the file watcher instead.
pub const WITHOUT_HOOKS: &str = "Cline, OpenCode, Amp, Aider and Zed";

pub fn find(id: &str) -> Option<&'static AiTool> {
    let id = id.trim().to_ascii_lowercase();
    TOOLS
        .iter()
        .find(|t| t.id == id || t.name.to_ascii_lowercase() == id)
}

/// Tools that look like they're used in this project (their settings folder exists).
pub fn detected(root: &Path) -> Vec<&'static AiTool> {
    TOOLS
        .iter()
        .filter(|t| root.join(t.hint).exists() || root.join(t.file).exists())
        .collect()
}

fn command(tool: &AiTool, kind: &str) -> String {
    format!("asqu event {kind} --ai \"{}\"", tool.name)
}

/// Is this hook command one of Asqu's (including what older versions wrote)?
fn is_asqu(cmd: &str) -> bool {
    cmd.contains("asqu event") || (cmd.contains("asqu") && cmd.trim_end().ends_with(" hook"))
}

impl AiTool {
    /// Where this tool's settings go in this project. Windsurf reads `.devin/` first and
    /// falls back to the older `.windsurf/`: use whichever the project already has.
    pub fn path(&self, root: &Path) -> PathBuf {
        if self.id == "windsurf"
            && !root.join(".devin/hooks.json").exists()
            && root.join(".windsurf/hooks.json").exists()
        {
            return root.join(".windsurf/hooks.json");
        }
        root.join(self.file)
    }

    fn whole_file_is_ours(&self) -> bool {
        matches!(self.format, Format::CopilotFile | Format::KiroFile)
    }

    /// The settings with Asqu's entries for this tool added (or, `add: false`, removed).
    fn change(&self, current: Value, add: bool) -> Result<Value> {
        let mut root = if current.is_object() {
            current
        } else {
            json!({})
        };
        match self.format {
            Format::CopilotFile => {
                let mut hooks = Map::new();
                for (event, _, kind) in self.events {
                    hooks.insert(
                        event.to_string(),
                        json!([{ "type": "command", "bash": command(self, kind), "timeoutSec": 30 }]),
                    );
                }
                return Ok(if add {
                    json!({ "version": 1, "hooks": hooks })
                } else {
                    Value::Null
                });
            }
            Format::KiroFile => {
                let hooks: Vec<Value> = self
                    .events
                    .iter()
                    .map(|(event, _, kind)| {
                        json!({ "name": format!("asqu-{kind}"), "trigger": event,
                                "action": { "type": "command", "command": command(self, kind) } })
                    })
                    .collect();
                return Ok(if add {
                    json!({ "version": "v1", "hooks": hooks })
                } else {
                    Value::Null
                });
            }
            Format::Grouped { wrapped } => {
                let obj = root
                    .as_object_mut()
                    .context("settings must be a JSON object")?;
                let container = if wrapped {
                    obj.entry("hooks")
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                        .context("\"hooks\" must be an object")?
                } else {
                    obj
                };
                for (event, matcher, kind) in self.events {
                    let list = container
                        .entry(event.to_string())
                        .or_insert_with(|| json!([]));
                    let groups = list
                        .as_array_mut()
                        .with_context(|| format!("{event} must be a list"))?;
                    // Take out Asqu's own commands; drop groups left empty.
                    for g in groups.iter_mut() {
                        if let Some(h) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                            h.retain(|x| !x["command"].as_str().is_some_and(is_asqu));
                        }
                    }
                    groups.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
                    if add {
                        let mut group = json!({ "hooks": [{ "type": "command", "command": command(self, kind) }] });
                        if let Some(m) = matcher {
                            group["matcher"] = json!(m);
                        }
                        groups.push(group);
                    }
                    if groups.is_empty() {
                        container.remove(*event);
                    }
                }
                if wrapped && obj_hooks_empty(&root) {
                    root.as_object_mut().unwrap().remove("hooks");
                }
            }
            Format::Flat { version } => {
                let obj = root
                    .as_object_mut()
                    .context("settings must be a JSON object")?;
                if let (Some(v), true) = (version, add) {
                    obj.entry("version").or_insert(json!(v));
                }
                let hooks = obj
                    .entry("hooks")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .context("\"hooks\" must be an object")?;
                for (event, _, kind) in self.events {
                    let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
                    let entries = list
                        .as_array_mut()
                        .with_context(|| format!("{event} must be a list"))?;
                    entries.retain(|x| !x["command"].as_str().is_some_and(is_asqu));
                    if add {
                        entries.push(json!({ "command": command(self, kind) }));
                    }
                    if entries.is_empty() {
                        hooks.remove(*event);
                    }
                }
                if obj_hooks_empty(&root) {
                    root.as_object_mut().unwrap().remove("hooks");
                }
            }
        }
        Ok(root)
    }
}

fn obj_hooks_empty(root: &Value) -> bool {
    root["hooks"].as_object().is_some_and(|h| h.is_empty())
}

/// Where a tool's settings are, if it's safe to write there: a plain file inside the
/// project (or not there yet), never through a link that leads elsewhere.
fn safe_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let mut at = root.to_path_buf();
    for part in path.strip_prefix(root).unwrap_or(path).components() {
        at.push(part);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{} is a link, so Asqu won't change it", at.display());
        }
    }
    Ok(path.to_path_buf())
}

/// What `apply` would do for a tool: the file, and whether anything changes.
pub struct Plan {
    pub tool: &'static AiTool,
    pub file: PathBuf,
    pub exists: bool,
    pub up_to_date: bool,
}

fn read(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => serde_json::from_str(&t).with_context(|| {
            format!(
                "{} isn't valid JSON, so Asqu won't change it",
                path.display()
            )
        }),
        _ => Ok(Value::Null),
    }
}

pub fn plan(root: &Path, tool: &'static AiTool) -> Result<Plan> {
    let file = safe_path(root, &tool.path(root))?;
    let current = read(&file)?;
    let wanted = tool.change(current.clone(), true)?;
    Ok(Plan {
        tool,
        exists: file.exists(),
        up_to_date: current == wanted,
        file,
    })
}

/// Switches on `asqu event` in this tool's settings for the project. Returns the file.
pub fn apply(root: &Path, tool: &'static AiTool) -> Result<PathBuf> {
    let file = safe_path(root, &tool.path(root))?;
    let wanted = tool.change(read(&file)?, true)?;
    fs::create_dir_all(file.parent().context("bad settings path")?)?;
    fs::write(&file, serde_json::to_string_pretty(&wanted)? + "\n")?;
    Ok(file)
}

/// Takes Asqu's entries out of every tool's settings in the project. Returns the files changed.
pub fn remove_all(root: &Path) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for tool in TOOLS {
        let Ok(file) = safe_path(root, &tool.path(root)) else {
            continue;
        };
        if !file.exists() {
            continue;
        }
        let current = read(&file)?;
        let after = tool.change(current.clone(), false)?;
        if after == current {
            continue;
        }
        // Nothing left but a format version: the file only existed for Asqu.
        let nothing_left = after
            .as_object()
            .is_some_and(|o| o.keys().all(|k| k == "version"));
        if tool.whole_file_is_ours() || nothing_left {
            fs::remove_file(&file)?;
        } else {
            fs::write(&file, serde_json::to_string_pretty(&after)? + "\n")?;
        }
        changed.push(file);
    }
    Ok(changed)
}
