//! AsquHub: the hosted home of a project, for people and AI agents working together.
//!
//! `asqu link hub` asks the Hub for a link and shows an approval page; the project's owner
//! approves it once in the browser and Asqu collects the key itself, so an AI can set this
//! up with one tap from the person. From then on the background copier sends every save
//! to the Hub, in order, signed with this person's Asqu key. Links and their keys live in
//! the person's own Asqu folder, never in the project.

use crate::hash::{self, Hash};
use crate::store::{LogEntry, Manifest, DIR};
use crate::{backup, sign, Store};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEFAULT_HUB: &str = "https://asquhub.com";
/// The Hub takes files up to this size for now.
const MAX_BLOB: usize = 4 * 1024 * 1024;
/// The longest message the Hub shows in its history.
const MAX_MESSAGE: usize = 2000;

pub const STALE: &str = "the Hub has newer work; bringing it down here arrives in the next update";
pub const UNLINKED: &str = "the Hub link was removed or expired; run asqu link hub again";

// ---- the Hub's address ----

/// `https://…` (or `http://` on this computer, for testing), as just the address.
pub fn parse_hub(spec: &str) -> Result<String> {
    let url: url::Url = spec.trim().parse().context("the Hub address isn't a web address")?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        bail!("the Hub address must start with https://");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!("give just the Hub's address, like {DEFAULT_HUB}");
    }
    Ok(url.origin().ascii_serialization())
}

/// asquhub.com itself, or (in development and test builds only) a Hub on this computer.
/// In a released Asqu, only asquhub.com can be linked without the person confirming:
/// a Hub on this computer could be anyone's program listening on a port.
pub fn is_default_or_local(hub: &str) -> bool {
    hub == DEFAULT_HUB
        || (cfg!(debug_assertions)
            && ["http://127.0.0.1", "http://localhost", "http://[::1]"].iter().any(|l| hub == *l || hub.starts_with(&format!("{l}:"))))
}

/// How the Hub is shown to people: `asquhub.com`.
pub fn hub_name(hub: &str) -> &str {
    hub.split_once("://").map(|(_, rest)| rest).unwrap_or(hub)
}

// ---- links (kept outside the project) ----

/// A project's link to the Hub. The key in it is never shown or logged.
#[derive(Clone)]
pub struct Link {
    pub hub: String,
    pub project_id: String,
    pub project_name: String,
    pub label: String,
    token: String,
    pub permissions: Vec<String>,
    pub expires_at: Option<i64>,
}

impl Link {
    fn can(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    /// What this computer is allowed to do in the Hub project, in plain words.
    pub fn level(&self) -> &'static str {
        if self.can("accept") {
            "can make changes"
        } else if self.can("write") {
            "can suggest changes"
        } else {
            "can view"
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "hub": self.hub,
            "project_id": self.project_id,
            "project_name": self.project_name,
            "label": self.label,
            "token": self.token,
            "permissions": self.permissions,
            "expires_at": self.expires_at,
        })
    }

    fn from_json(v: &Value) -> Option<Link> {
        Some(Link {
            hub: parse_hub(v["hub"].as_str()?).ok()?,
            project_id: safe_id(v["project_id"].as_str()?)?.to_string(),
            // Text from the Hub is shown to the person, so anything hidden in it (codes that
            // recolour, retitle or rewrite the terminal) is taken out as it arrives.
            project_name: crate::describe::printable(v["project_name"].as_str().unwrap_or("")),
            label: crate::describe::printable(v["label"].as_str().unwrap_or("")),
            token: v["token"].as_str()?.into(),
            permissions: v["permissions"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| p.as_str().map(crate::describe::printable)).collect())
                .unwrap_or_default(),
            expires_at: v["expires_at"].as_i64(),
        })
    }
}

/// A project id from the Hub goes into web addresses: only letters, digits, `_` and `-`.
fn safe_id(id: &str) -> Option<&str> {
    (!id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .then_some(id)
}

fn links_file() -> Result<PathBuf> {
    Ok(sign::home().context("no home folder to keep the Hub link in")?.join("hub-links.json"))
}

fn read_links() -> Value {
    links_file()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

/// This project's Hub link, if it has one.
pub fn link(root: &Path) -> Option<Link> {
    Link::from_json(&read_links()[backup::key(root)])
}

fn save_link(root: &Path, link: &Link) -> Result<()> {
    let mut all = read_links();
    all[backup::key(root)] = link.to_json();
    backup::write_private_json(&links_file()?, &all)
}

/// Forgets this project's Hub link (the project and its history on the Hub are kept).
/// Returns false if it had none.
pub fn unlink(root: &Path) -> Result<bool> {
    let mut all = read_links();
    let removed = all.as_object_mut().is_some_and(|o| o.remove(&backup::key(root)).is_some());
    if removed {
        backup::write_private_json(&links_file()?, &all)?;
    }
    let _ = fs::remove_file(state_path(root));
    Ok(removed)
}

// ---- talking to the Hub (with curl) ----

enum Body<'a> {
    /// A small JSON body, sent through curl's input.
    Json(&'a Value),
    /// A file's bytes, with their content type.
    File(&'a Path, &'a str),
}

/// A string inside curl's settings: `\` and `"` escaped.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Sends one request and returns the status and the JSON answer (Null if none).
/// The address, the key and small bodies go in through curl's input, never its command
/// line, so other programs on the computer can't see them.
fn call(method: &str, url: &str, token: Option<&str>, body: Body, retry: bool) -> Result<(u16, Value)> {
    use std::io::Write;
    if url.contains(['"', '\\', '\n', '\r']) {
        bail!("unexpected characters in a Hub address");
    }
    let mut config = format!("url = {}\nrequest = {}\n", quoted(url), quoted(method));
    if let Some(t) = token {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\') {
            bail!("the stored Hub key is damaged; run asqu link hub again");
        }
        config += &format!("header = \"Authorization: Bearer {t}\"\n");
    }
    match body {
        Body::Json(v) => {
            config += "header = \"Content-Type: application/json\"\n";
            config += &format!("data-binary = {}\n", quoted(&v.to_string()));
        }
        Body::File(path, kind) => {
            // A line break in the path would start a new curl setting of someone's choosing.
            if path.to_string_lossy().chars().any(char::is_control) {
                bail!("this project's folder has a name with hidden characters, so nothing was sent to the Hub");
            }
            config += &format!("header = {}\n", quoted(&format!("Content-Type: {kind}")));
            config += &format!("data-binary = {}\n", quoted(&format!("@{}", path.display())));
        }
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--connect-timeout", "20", "--max-time", "300", "-w", "\\n%{http_code}", "--config", "-"]);
    if retry {
        cmd.args(["--retry", "2"]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl is needed to reach the Hub")?;
    child.stdin.take().context("curl input")?.write_all(config.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("couldn't reach the Hub: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (answer, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    Ok((code.trim().parse().unwrap_or(0), serde_json::from_str(answer).unwrap_or(Value::Null)))
}

// ---- linking ----

/// A link request waiting for the owner's approval.
pub struct Pending {
    /// Proves this computer made the request; never shown.
    link_id: String,
    pub code: String,
    pub approve_url: String,
    expires_in: u64,
    interval: u64,
}

/// A name the Hub accepts: no control characters, trimmed, 1–100 bytes.
pub fn clean_name(name: &str, fallback: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let name = cut(name.trim(), 100).trim().to_string();
    if name.is_empty() {
        fallback.into()
    } else {
        name
    }
}

/// At most `max` bytes of `s`, cut at a character boundary.
fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Asks the Hub for a link. `client` is the AI tool (or "Asqu"), `device` this computer.
pub fn start(hub: &str, client: &str, device: &str, project_name: &str, project_id: Option<&str>) -> Result<Pending> {
    let signer = hash::hex(&sign::Identity::load_or_create()?.public());
    let mut body = json!({
        "client": clean_name(client, "Asqu"),
        "device": clean_name(device, "this computer"),
        "project_name": clean_name(project_name, "Project"),
        "signer": signer,
    });
    if let Some(id) = project_id {
        body["project_id"] = json!(safe_id(id).context("that isn't a Hub project id")?);
    }
    let (code, reply) = call("POST", &format!("{hub}/v1/link/start"), None, Body::Json(&body), false)?;
    match code {
        201 => {}
        429 => bail!("the Hub is busy with link requests; try again in a few minutes"),
        _ => bail!("the Hub said {code} when asked for a link{}", error_of(&reply)),
    }
    let text = |k: &str| reply[k].as_str().map(str::to_string).with_context(|| format!("the Hub's answer has no {k}"));
    let pending = Pending {
        link_id: text("link_id")?,
        code: text("code")?,
        approve_url: text("approve_url")?,
        expires_in: reply["expires_in"].as_u64().unwrap_or(600).clamp(30, 3600),
        interval: reply["interval"].as_u64().unwrap_or(3).clamp(1, 30),
    };
    // Only ever open a page on the Hub itself.
    if !pending.approve_url.starts_with(&format!("{hub}/")) || pending.approve_url.contains(char::is_whitespace) {
        bail!("the Hub gave an approval address outside the Hub");
    }
    if pending.code.is_empty() || pending.code.len() > 32 || !pending.code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!("the Hub gave an unexpected code");
    }
    Ok(pending)
}

/// Waits for the owner to approve (or turn down) the link, checking every few seconds.
pub fn wait(hub: &str, pending: &Pending) -> Result<Link> {
    let deadline = Instant::now() + Duration::from_secs(pending.expires_in + 30);
    let mut interval = pending.interval;
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        let body = json!({ "link_id": pending.link_id });
        let (code, reply) = match call("POST", &format!("{hub}/v1/link/collect"), None, Body::Json(&body), false) {
            Ok(r) => r,
            // A moment offline doesn't end the wait.
            Err(_) if Instant::now() < deadline => continue,
            Err(e) => return Err(e),
        };
        match (code, reply["status"].as_str()) {
            (200, Some("approved")) => {
                // The key is kept for the Hub Asqu asked, whatever address the answer names.
                let mut v = reply.clone();
                v["hub"] = json!(hub);
                return Link::from_json(&v).context("the Hub's approval was missing details");
            }
            (200, Some("waiting")) => {}
            (200, Some("denied")) => bail!("the owner turned the link down, so nothing is shared"),
            (404, _) => bail!("the link request expired before it was approved; run asqu link hub again"),
            (429, _) => interval = (interval * 2).min(30),
            _ => bail!("the Hub said {code} while waiting for approval{}", error_of(&reply)),
        }
        if Instant::now() >= deadline {
            bail!("the link request expired before it was approved; run asqu link hub again");
        }
    }
}

/// Keeps the approved link for this project and starts sharing from its first save.
pub fn remember(store: &Store, link: &Link) -> Result<()> {
    save_link(store.root(), link)?;
    reset_progress(store, &link.project_id)
}

fn reset_progress(store: &Store, project_id: &str) -> Result<()> {
    store.set_meta("hub_project", project_id)?;
    store.set_meta("hub_synced_seq", "0")?;
    store.set_meta("hub_head", "")
}

fn error_of(reply: &Value) -> String {
    // Shown to the person and kept for `asqu status`: nothing hidden, and not too long.
    reply["error"]
        .as_str()
        .map(|e| format!(" ({})", crate::describe::printable(e).chars().take(200).collect::<String>()))
        .unwrap_or_default()
}

// ---- what the Hub is sent ----

/// One file as the Hub names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubFile {
    pub path: String,
    /// `b3:<hex>`: the same BLAKE3 fingerprint Asqu uses.
    pub blob: String,
    pub exec: bool,
}

/// The files of a Timeline entry, in path order.
pub fn hub_files(m: &Manifest) -> Vec<HubFile> {
    // A Manifest is ordered by path bytes, the same order the Hub uses.
    m.iter().map(|(path, f)| HubFile { path: path.clone(), blob: format!("b3:{}", hash::hex(&f.hash)), exec: f.exec }).collect()
}

/// The Hub's name for a set of files (matches the Hub's own `snapshot_identity`).
pub fn snapshot_identity(files: &[HubFile]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"asquhub-snapshot-v1\0");
    for f in files {
        if f.exec {
            h.update(&[0xFF]);
        }
        h.update(&(f.path.len() as u64).to_be_bytes());
        h.update(f.path.as_bytes());
        h.update(f.blob.as_bytes());
    }
    format!("s1:{}", h.finalize().to_hex())
}

/// The exact bytes Asqu signs for a save sent to the Hub (matches the Hub's own
/// `signed_save_message`): the project, the version it builds on, the files and the entry.
pub fn signed_save_message(project_id: &str, expected_base: Option<&str>, snapshot: &str, entry: &str) -> Vec<u8> {
    format!("asquhub-save-v1\0{project_id}\0{}\0{snapshot}\0{entry}", expected_base.unwrap_or("")).into_bytes()
}

/// The message shown in the Hub's history: the request, or Asqu's own description.
fn message(e: &LogEntry) -> String {
    let text = match (e.request.trim(), e.summary.as_deref().map(str::trim)) {
        ("", Some(s)) if !s.is_empty() => s.to_string(),
        ("", _) => format!("Saved #{}", e.seq),
        (r, _) => r.to_string(),
    };
    let text: String = text.chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).collect();
    cut(&text, MAX_MESSAGE).to_string()
}

fn files_json(files: &[HubFile]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|f| {
                let mut v = json!({ "path": f.path, "blob": f.blob });
                if f.exec {
                    v["exec"] = json!(true);
                }
                v
            })
            .collect(),
    )
}

/// A Hub version id is stored and sent back as is: only plain characters.
fn safe_revision(r: &str) -> Option<&str> {
    (!r.is_empty() && r.len() <= 200 && r.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')).then_some(r)
}

// ---- sending saves ----

fn meta_seq(store: &Store) -> Result<i64> {
    Ok(store.meta("hub_synced_seq")?.and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Sends every Timeline entry the Hub doesn't have yet, oldest first. Returns the entry
/// number the Hub now has. Stops at the first problem; the rest stay queued for next time.
pub fn sync(store: &Store, link: &Link) -> Result<i64> {
    if store.meta("hub_project")?.as_deref() != Some(link.project_id.as_str()) {
        reset_progress(store, &link.project_id)?;
    }
    let mut synced = meta_seq(store)?;
    let head_seq = store.head_seq()?.unwrap_or(0);
    if synced >= head_seq {
        return Ok(synced);
    }
    if !link.can("accept") {
        if link.can("write") {
            bail!("this link can only suggest changes, and sending suggestions arrives in a later update");
        }
        bail!("this link can only view the Hub project, so saves aren't sent");
    }
    let me = sign::Identity::load_or_create()?;
    let signer = hash::hex(&me.public());
    let mut base = store.meta("hub_head")?.filter(|h| !h.is_empty());
    // Files of the last entry sent are already on the Hub.
    let mut sent: HashSet<Hash> =
        if synced > 0 { store.manifest_at(synced)?.values().map(|f| f.hash).collect() } else { HashSet::new() };
    let project = format!("{}/v1/projects/{}", link.hub, link.project_id);
    let asqu_dir = store.root().join(DIR);

    for seq in synced + 1..=head_seq {
        let entry = store.entry_at(seq)?;
        let manifest = store.manifest_at(seq)?;
        for (path, f) in &manifest {
            if sent.contains(&f.hash) {
                continue;
            }
            let data = store.get_blob(&f.hash)?;
            if data.len() > MAX_BLOB {
                bail!("file {path} is too big for the Hub yet (over 4 MB); bigger files arrive in a later update");
            }
            let url = format!("{project}/blobs/b3:{}", hash::hex(&f.hash));
            let (code, reply) = with_temp(&asqu_dir, "blob", &data, |p| {
                call("PUT", &url, Some(&link.token), Body::File(p, "application/octet-stream"), true)
            })?;
            match code {
                200 | 201 => {}
                401 => bail!(UNLINKED),
                _ => bail!("the Hub said {code} for file {path}{}", error_of(&reply)),
            }
            sent.insert(f.hash);
        }

        let files = hub_files(&manifest);
        let snapshot = snapshot_identity(&files);
        let entry_hex = hash::hex(&entry.hash);
        let signature = me.sign(&signed_save_message(&link.project_id, base.as_deref(), &snapshot, &entry_hex));
        let body = json!({
            "idempotency_key": entry_hex,
            "expected_base": base,
            "files": files_json(&files),
            "message": message(&entry),
            "asqu": { "entry": entry_hex, "signer": signer, "signature": hash::hex(&signature) },
        });
        let (code, reply) = with_temp(&asqu_dir, "save", body.to_string().as_bytes(), |p| {
            call("POST", &format!("{project}/candidates"), Some(&link.token), Body::File(p, "application/json"), true)
        })?;
        match (code, reply["error"].as_str()) {
            (200 | 201, _) if reply["status"] == "accepted" => {
                let revision = reply["revision"]
                    .as_str()
                    .and_then(safe_revision)
                    .context("the Hub accepted the save but didn't say which version it became")?;
                store.set_meta("hub_head", revision)?;
                store.set_meta("hub_synced_seq", &seq.to_string())?;
                base = Some(revision.to_string());
                synced = seq;
            }
            (409, Some("stale_base")) => bail!(STALE),
            (401, _) => bail!(UNLINKED),
            (403, _) => bail!("the Hub refused save #{seq}: this link isn't allowed to make changes"),
            (422, Some(e)) => bail!("the Hub turned down save #{seq} ({e})"),
            _ => bail!("the Hub said {code} for save #{seq}{}; it will be sent again later", error_of(&reply)),
        }
    }
    Ok(synced)
}

/// Runs `f` with `data` in a temporary file inside `.asqu`, then removes the file.
fn with_temp<T>(dir: &Path, what: &str, data: &[u8], f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let tmp = backup::tempfile_in(dir, what)?;
    fs::write(&tmp, data)?;
    let r = f(&tmp);
    let _ = fs::remove_file(&tmp);
    r
}

// ---- sharing state (shown by `asqu status` and `asqu link`) ----

fn state_path(root: &Path) -> PathBuf {
    root.join(DIR).join("hub-state.json")
}

fn read_state(root: &Path) -> Value {
    fs::read_to_string(state_path(root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}))
}

fn record(root: &Path, outcome: &Result<i64>) {
    let prev = read_state(root);
    let state = match outcome {
        Ok(seq) => json!({ "ok": backup::now_ms(), "seq": seq, "tried": backup::now_ms() }),
        Err(e) => json!({ "ok": prev["ok"], "seq": prev["seq"], "tried": backup::now_ms(), "error": format!("{e:#}") }),
    };
    let _ = crate::store::write_file_safely(&state_path(root), serde_json::to_string_pretty(&state).unwrap_or_default().as_bytes());
}

/// Sends new saves to the Hub now, if the project is linked, and records how it went.
pub fn sync_and_record(store: &Store) -> Option<Result<i64>> {
    let link = link(store.root())?;
    let r = sync(store, &link);
    record(store.root(), &r);
    Some(r)
}

/// Is the project linked to the Hub with saves the Hub doesn't have yet? Compares entry
/// numbers rather than times, so a save made while others are being sent isn't missed.
pub fn needs_sync(root: &Path) -> bool {
    use rusqlite::{Connection, OpenFlags};
    let Some(link) = link(root) else { return false };
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(c) = Connection::open_with_flags(root.join(DIR).join("store.db"), flags) else { return true };
    let meta = |k: &str| c.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get::<_, String>(0)).ok();
    let head: i64 = c.query_row("SELECT COALESCE(MAX(seq), 0) FROM timeline", [], |r| r.get(0)).unwrap_or(0);
    let synced = match meta("hub_project") {
        Some(p) if p == link.project_id => meta("hub_synced_seq").and_then(|s| s.parse().ok()).unwrap_or(0),
        _ => 0,
    };
    head > synced
}

/// When sending to the Hub was last tried (0 if never).
pub fn last_tried(root: &Path) -> i64 {
    read_state(root)["tried"].as_i64().unwrap_or(0)
}

/// Did the last try fail?
pub fn failed_last(root: &Path) -> bool {
    link(root).is_some() && read_state(root)["error"].is_string()
}

/// One line for `asqu status`: where the project is shared and how the last send went.
pub fn status_line(root: &Path) -> Option<String> {
    let link = link(root)?;
    let s = read_state(root);
    let place = format!("the Hub at {} (project \"{}\", {})", hub_name(&link.hub), link.project_name, link.level());
    Some(match (s["error"].as_str(), s["ok"].as_i64()) {
        (Some(e), _) => format!("Sharing this project with {place} stopped: {e}"),
        (None, Some(ok)) => format!(
            "This project is shared with {place}: last sent {}, up to #{}",
            backup::ago(ok),
            s["seq"].as_i64().unwrap_or(0)
        ),
        _ => format!("This project is linked to {place}; nothing has been sent yet"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<HubFile> {
        vec![
            HubFile { path: "README.md".into(), blob: format!("b3:{}", "11".repeat(32)), exec: false },
            HubFile { path: "run.sh".into(), blob: format!("b3:{}", "22".repeat(32)), exec: true },
            HubFile { path: "src/ü.txt".into(), blob: format!("b3:{}", "33".repeat(32)), exec: false },
        ]
    }

    // These values were computed by the Hub's own `asquhub::snapshot_identity` and
    // `asquhub::signed_save_message` for the same files, so both sides agree byte for byte.
    const SNAPSHOT: &str = "s1:fd64dbc5b82bff6dcf9ab81fcb66c90f9cb5ca3d5c2c81b6ac9e3909d4035a57";

    #[test]
    fn snapshot_matches_the_hub() {
        assert_eq!(snapshot_identity(&fixture()), SNAPSHOT);
    }

    #[test]
    fn signed_message_matches_the_hub() {
        let entry = "44".repeat(32);
        let with_base = signed_save_message("p_abc", Some("v1:xyz"), SNAPSHOT, &entry);
        assert_eq!(
            hash::hex(&with_base),
            format!(
                "617371756875622d736176652d763100705f6162630076313a78797a00{}00{}",
                hash::hex(SNAPSHOT.as_bytes()),
                hash::hex(entry.as_bytes())
            )
        );
        let first = signed_save_message("p_abc", None, SNAPSHOT, &entry);
        assert_eq!(
            hash::hex(&first),
            format!("617371756875622d736176652d763100705f6162630000{}00{}", hash::hex(SNAPSHOT.as_bytes()), hash::hex(entry.as_bytes()))
        );
    }

    #[test]
    fn files_list_marks_only_runnable_files() {
        let v = files_json(&fixture());
        assert_eq!(v[1]["exec"], json!(true));
        assert!(v[0].get("exec").is_none() && v[2].get("exec").is_none());
    }

    #[test]
    fn hub_addresses() {
        assert_eq!(parse_hub("https://asquhub.com/").unwrap(), "https://asquhub.com");
        assert_eq!(parse_hub("http://127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080");
        assert!(parse_hub("http://asquhub.com").is_err());
        assert!(parse_hub("https://asquhub.com/x").is_err());
        assert!(parse_hub("https://me:pw@asquhub.com").is_err());
        assert_eq!(hub_name("https://asquhub.com"), "asquhub.com");
        assert!(is_default_or_local("https://asquhub.com") && is_default_or_local("http://127.0.0.1:4747"));
        assert!(!is_default_or_local("https://asquhub.com.evil.example") && !is_default_or_local("http://127.0.0.1.evil.example"));
    }

    #[test]
    fn names_and_messages_fit_the_hub() {
        assert_eq!(clean_name("  My\tMac\n ", "x"), "My Mac");
        assert_eq!(clean_name("", "Asqu"), "Asqu");
        assert!(clean_name(&"é".repeat(80), "x").len() <= 100);
        let e = LogEntry {
            seq: 3,
            ts: 0,
            hash: hash::ZERO,
            request: "".into(),
            actor: "a".into(),
            agent: None,
            edits: 1,
            summary: Some("Changed\u{7}a.txt".into()),
        };
        assert_eq!(message(&e), "Changed a.txt");
        let long = LogEntry { request: "ü".repeat(1500), ..e };
        assert!(message(&long).len() <= MAX_MESSAGE);
    }
}
