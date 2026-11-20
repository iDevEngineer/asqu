//! `asqu event`: one way in for every AI tool, with nothing specific to any of them.
//!
//! Most AI coding tools can run a command when something happens ("hooks"). Each points
//! the same command at Asqu:
//!
//! ```text
//! asqu event prompt --ai "Codex"    # the person sent a request
//! asqu event edit   --ai "Codex"    # the AI changed files
//! asqu event done   --ai "Codex"    # the AI finished its turn
//! ```
//!
//! Whatever the tool sends on its input (usually JSON) is read generously: the request
//! text, a session id, the edited file and the folder are picked out from the field names
//! tools commonly use, so no tool needs its own code. Without the kind argument, it's
//! worked out from the tool's own event name where one is given.
//!
//! An event never gets in the AI's way: problems are reported quietly and it always
//! succeeds, and nothing is printed on standard output (some tools show that to the AI).

use crate::store::{Store, Why};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;

const MAX_REQUEST_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Prompt,
    Edit,
    Done,
}

impl Kind {
    pub fn parse(word: &str) -> Option<Kind> {
        // Our own words, plus the event names tools commonly use for the same moments.
        let w: String = word.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        match w.as_str() {
            "prompt" | "userpromptsubmit" | "beforesubmitprompt" | "promptsubmit" | "userprompt" | "beforeagent" => {
                Some(Kind::Prompt)
            }
            "edit" | "posttooluse" | "afterfileedit" | "aftertool" | "afteredit" | "fileedit" | "filechange"
            | "posttool" | "aftertooluse" => Some(Kind::Edit),
            "done" | "stop" | "afteragent" | "sessionend" | "turnend" | "agentstop" | "taskcomplete" | "end" => {
                Some(Kind::Done)
            }
            _ => None,
        }
    }
}

/// The first text value found under any of `keys`, at the top level only. Used for what
/// decides where and how Asqu acts (folder, event, session): never taken from inside the
/// AI's own tool input, which the AI controls.
fn top_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str).map(str::to_string))
}

/// The first text value found under any of `keys`, at the top level or one level down
/// (for the request text and the edited file, which some tools nest).
fn find_str(v: &Value, keys: &[&str]) -> Option<String> {
    let direct = |obj: &Value| keys.iter().find_map(|k| obj.get(*k).and_then(Value::as_str).map(str::to_string));
    if let Some(s) = direct(v) {
        return Some(s);
    }
    v.as_object()?.values().filter(|x| x.is_object()).find_map(direct)
}

fn find_path(v: &Value) -> Option<PathBuf> {
    if let Some(p) = find_str(v, &["file_path", "filePath", "path", "file", "target_file"]) {
        return Some(PathBuf::from(p));
    }
    // A list of edited files: the first one is enough to tell which folder it's in.
    ["files", "edited_files", "paths", "file_paths"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_array))
        .and_then(|a| a.first())
        .and_then(|f| f.as_str().map(str::to_string).or_else(|| f.get("path").and_then(Value::as_str).map(str::to_string)))
        .map(PathBuf::from)
}

/// Takes out notes some AI tools add to the person's message behind the scenes (blocks
/// like `<system-reminder>…</system-reminder>`), keeping only what the person wrote.
fn person_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn shorten(s: &str) -> String {
    let s = person_words(s);
    let s = s.trim();
    if s.chars().count() <= MAX_REQUEST_CHARS {
        s.to_string()
    } else {
        s.chars().take(MAX_REQUEST_CHARS).collect::<String>() + "…"
    }
}

/// Handles one event. `kind` comes from the command line if the tool's settings name it;
/// otherwise from the tool's own event name in `input`.
pub fn handle(kind: Option<Kind>, ai: Option<&str>, input: &str, actor: &str) -> Result<()> {
    let v: Value = serde_json::from_str(input.trim()).unwrap_or(Value::Null);
    let kind = kind.or_else(|| {
        top_str(&v, &["hook_event_name", "hookEventName", "event", "event_name", "eventName", "agent_action_name", "hook", "type"])
            .and_then(|e| Kind::parse(&e))
    });
    let Some(kind) = kind else { return Ok(()) }; // not an event Asqu needs
    let session = top_str(
        &v,
        &["session_id", "sessionId", "conversation_id", "conversationId", "trajectory_id", "thread_id", "threadId", "chat_id"],
    )
    .unwrap_or_else(|| "default".into());
    let here = std::env::current_dir()?;
    let cwd = v
        .get("workspace_roots")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| top_str(&v, &["cwd", "working_directory", "workingDirectory", "project_dir", "projectDir"]))
        .map(PathBuf::from)
        .unwrap_or_else(|| here.clone());
    // A history is only ever started in the folder the tool ran Asqu from; elsewhere,
    // Asqu only records into a project that already has one.
    let same_place = cwd.canonicalize().ok() == here.canonicalize().ok();
    if !same_place && Store::find_root(&cwd).is_none() {
        return Ok(());
    }
    let why = |store: &Store| -> Result<Why> {
        Ok(Why {
            request: store.session_prompt(&session)?.unwrap_or_default(),
            actor: actor.into(),
            agent: ai.map(str::to_string),
            session: Some(session.clone()),
            ..Default::default()
        })
    };

    match kind {
        Kind::Prompt => {
            // Some tools pass the request in an environment variable instead.
            let prompt = find_str(&v, &["prompt", "user_prompt", "userPrompt", "message", "text", "input", "query"])
                .or_else(|| std::env::var("USER_PROMPT").ok())
                .unwrap_or_default();
            if prompt.trim().is_empty() {
                return Ok(());
            }
            let store = Store::open_or_create(&cwd)?;
            store.set_session_prompt(&session, &shorten(&prompt))?;
            if let Some(name) = store.try_for_path(&cwd) {
                // Asked inside a Try's folder (a worktree, say): it labels that Try's saves,
                // not the main project's.
                store.set_try_intent(&name, &shorten(&prompt))?;
            } else {
                // Also label saves by anything else (the watcher, say) during this task.
                store.set_intent(&shorten(&prompt))?;
            }
        }
        Kind::Edit => {
            let mut store = Store::open_or_create(&cwd)?;
            let why = why(&store)?;
            // An edit inside a Try's folder belongs to that Try, not the main project.
            if let Some(name) = find_path(&v).and_then(|p| store.try_for_path(&p)).or_else(|| store.try_for_path(&cwd)) {
                store.save_try(&name, &why)?;
            } else {
                store.save(&why)?;
            }
        }
        Kind::Done => {
            let mut store = Store::open_or_create(&cwd)?;
            let why = why(&store)?;
            if let Some(name) = store.try_for_path(&cwd) {
                store.save_try(&name, &why)?; // work done in a Try's folder belongs to that Try
            } else {
                store.save(&why)?;
                store.clear_intent(); // the task is finished
            }
        }
    }
    Ok(())
}
