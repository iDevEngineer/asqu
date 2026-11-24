//! Backups: copies a project's history to places the person chooses, automatically.
//!
//! A project is linked once (`asqu link <place>`); after that every change is copied in
//! the background, with no `push` to remember. A place is either a folder (iCloud Drive,
//! Dropbox, an external disk…) or S3-style cloud storage (Cloudflare R2, Amazon S3,
//! Backblaze…). Links live in the person's own Asqu folder, never in the project, so a
//! downloaded project can't make Asqu copy anything anywhere.
//!
//! A backup is `<place>/<name>-<id>/`: `store.db` (a consistent copy made with SQLite's
//! backup feature, never the live file), `large/` (big files) and `info.json`.

use crate::store::DIR;
use crate::{sign, Store};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The background copier waits at least this long between backups of one project.
const MIN_GAP: Duration = Duration::from_secs(20);
/// A lock older than this belongs to a copier that died; it's taken over.
const STALE_LOCK: Duration = Duration::from_secs(15 * 60);

// ---- places ----

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Folder(PathBuf),
    Cloud { bucket: String, prefix: String, endpoint: String, region: String },
}

impl Place {
    /// `s3://bucket/some/prefix` (with `endpoint`), or a folder path.
    pub fn parse(spec: &str, endpoint: Option<&str>, region: Option<&str>) -> Result<Place> {
        if let Some(rest) = spec.strip_prefix("s3://") {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            if bucket.is_empty() {
                bail!("give the bucket name, like s3://my-bucket/asqu");
            }
            let endpoint = endpoint
                .map(str::to_string)
                .or_else(|| std::env::var("ASQU_S3_ENDPOINT").ok())
                .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
                .context(
                    "which storage service? Add --endpoint, e.g. https://<account-id>.r2.cloudflarestorage.com \
                     for Cloudflare R2, or https://s3.<region>.amazonaws.com for Amazon S3",
                )?;
            let parsed: url::Url = endpoint.parse().context("the endpoint isn't a web address")?;
            let local = parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1");
            if parsed.scheme() != "https" && !local {
                bail!("the endpoint must start with https://");
            }
            let region = region
                .map(str::to_string)
                .or_else(|| std::env::var("AWS_REGION").ok())
                .unwrap_or_else(|| "auto".into());
            return Ok(Place::Cloud {
                bucket: bucket.into(),
                prefix: prefix.trim_matches('/').into(),
                endpoint: endpoint.trim_end_matches('/').into(),
                region,
            });
        }
        let path = PathBuf::from(spec);
        fs::create_dir_all(&path).with_context(|| format!("can't use the folder {}", path.display()))?;
        Ok(Place::Folder(path.canonicalize()?))
    }

    /// How the place is shown to the person.
    pub fn label(&self) -> String {
        match self {
            Place::Folder(p) => p.display().to_string(),
            Place::Cloud { bucket, prefix, .. } if prefix.is_empty() => format!("s3://{bucket}"),
            Place::Cloud { bucket, prefix, .. } => format!("s3://{bucket}/{prefix}"),
        }
    }

    /// A friendly name for people: "iCloud Drive › Asqu" instead of the full path.
    pub fn display(&self) -> String {
        let Place::Folder(path) = self else { return self.label() };
        let Some(home) = crate::platform::home_dir() else { return self.label() };
        let known: [(PathBuf, &str); 3] = [
            (home.join("Library/Mobile Documents/com~apple~CloudDocs"), "iCloud Drive"),
            (home.join("iCloudDrive"), "iCloud Drive"),
            (home.join("Dropbox"), "Dropbox"),
        ];
        for (base, name) in &known {
            if let Ok(rest) = path.strip_prefix(base) {
                return std::iter::once(name.to_string())
                    .chain(rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()))
                    .collect::<Vec<_>>()
                    .join(" › ");
            }
        }
        // Google Drive, OneDrive, Dropbox and others on newer Macs.
        if let Ok(rest) = path.strip_prefix(home.join("Library/CloudStorage")) {
            let mut parts = rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned());
            let service = parts.next().unwrap_or_default();
            let service = if service.starts_with("GoogleDrive-") {
                "Google Drive".to_string()
            } else if service.starts_with("OneDrive") {
                "OneDrive".to_string()
            } else if service.starts_with("Dropbox") {
                "Dropbox".to_string()
            } else {
                service
            };
            return std::iter::once(service).chain(parts.filter(|p| p != "My Drive")).collect::<Vec<_>>().join(" › ");
        }
        self.label()
    }

    fn to_json(&self) -> Value {
        match self {
            Place::Folder(p) => json!({ "folder": p }),
            Place::Cloud { bucket, prefix, endpoint, region } => {
                json!({ "bucket": bucket, "prefix": prefix, "endpoint": endpoint, "region": region })
            }
        }
    }

    fn from_json(v: &Value) -> Option<Place> {
        if let Some(p) = v["folder"].as_str() {
            return Some(Place::Folder(p.into()));
        }
        Some(Place::Cloud {
            bucket: v["bucket"].as_str()?.into(),
            prefix: v["prefix"].as_str().unwrap_or("").into(),
            endpoint: v["endpoint"].as_str()?.into(),
            region: v["region"].as_str().unwrap_or("auto").into(),
        })
    }
}

// ---- suggested places (for the Asqu view) ----

/// Backup places found on this computer: cloud-synced folders and external disks.
/// Each is (what to call it, the folder Asqu would back up into).
pub fn suggestions() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Some(home) = crate::platform::home_dir() else { return out };
    let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    if icloud.is_dir() {
        out.push(("iCloud Drive".to_string(), icloud.join("Asqu")));
    }
    // iCloud for Windows.
    if cfg!(windows) && home.join("iCloudDrive").is_dir() {
        out.push(("iCloud Drive".to_string(), home.join("iCloudDrive").join("Asqu")));
    }
    // OneDrive on Windows says where it is.
    if let Some(od) = std::env::var_os("OneDrive").map(PathBuf::from).filter(|p| p.is_dir()) {
        out.push(("OneDrive".to_string(), od.join("Asqu")));
    }
    if home.join("Dropbox").is_dir() {
        out.push(("Dropbox".to_string(), home.join("Dropbox/Asqu")));
    }
    // Dropbox, Google Drive, OneDrive and others on newer Macs live here.
    if let Ok(rd) = fs::read_dir(home.join("Library/CloudStorage")) {
        let mut found: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        for p in found {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (label, dir) = if let Some(acct) = name.strip_prefix("GoogleDrive-") {
                (format!("Google Drive ({acct})"), p.join("My Drive"))
            } else if name.starts_with("OneDrive") {
                if out.iter().any(|(l, _)| l == "OneDrive") {
                    continue;
                }
                ("OneDrive".to_string(), p.clone())
            } else if name.starts_with("Dropbox") {
                if out.iter().any(|(l, _)| l == "Dropbox") {
                    continue;
                }
                ("Dropbox".to_string(), p.clone())
            } else {
                (name.clone(), p.clone())
            };
            if dir.is_dir() {
                out.push((label, dir.join("Asqu")));
            }
        }
    }
    // External disks (not the Mac's own disk, which isn't a backup).
    if let Ok(rd) = fs::read_dir("/Volumes") {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let is_link = fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_link && p.is_dir() && name != "Macintosh HD" && !name.starts_with('.') {
                out.push((format!("Disk: {name}"), p.join("Asqu")));
            }
        }
    }
    out
}

/// Asks the person to pick a folder with the Mac's own folder picker.
/// Returns None if they cancel.
pub fn pick_folder() -> Result<Option<PathBuf>> {
    let out = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args(["-e", "POSIX path of (choose folder with prompt \"Choose where to back up this project\")"])
            .output()
    } else if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
                 $d.Description = 'Choose where to back up this project'; \
                 if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath } else { exit 1 }",
            ])
            .output()
    } else {
        bail!("type the folder's path instead (there's no folder picker here yet)");
    }
    .context("couldn't open the folder picker")?;
    if !out.status.success() {
        return Ok(None); // cancelled
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

// ---- links (kept outside the project) ----

fn links_file() -> Result<PathBuf> {
    Ok(sign::home().context("no home folder to keep backup settings in")?.join("backups.json"))
}

fn read_links() -> Value {
    links_file()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

/// A settings file about to be changed: `{}` when there is none yet. One that is there
/// is never quietly treated as empty (writing it back would wipe every other project's
/// entry in it): if it can't be read just now, nothing is changed; if it is damaged, Asqu
/// sets it aside under a new name, starts a fresh one and says so. The person never has
/// to repair a file by hand. `redo` says what then needs setting up again ("" for
/// nothing worth mentioning).
pub(crate) fn read_json_for_change(path: &Path, redo: &str) -> Result<Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e).with_context(|| format!("couldn't read {}, so it wasn't changed", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(v),
        _ => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let mut aside = path.with_file_name(format!("{name}.damaged-{stamp}"));
            let mut n = 1;
            while fs::symlink_metadata(&aside).is_ok() {
                n += 1;
                aside = path.with_file_name(format!("{name}.damaged-{stamp}-{n}"));
            }
            fs::rename(path, &aside)
                .with_context(|| format!("{} is damaged and couldn't be set aside, so it wasn't changed", path.display()))?;
            eprintln!(
                "asqu: one of Asqu's own settings files was damaged, so Asqu started a fresh one. \
                 The damaged one is kept as {}.{}",
                aside.display(),
                if redo.is_empty() { String::new() } else { format!(" {redo}") }
            );
            Ok(json!({}))
        }
    }
}

fn links_for_change() -> Result<Value> {
    read_json_for_change(
        &links_file()?,
        "Backup places chosen before need choosing again (`asqu link`, or in the Asqu view: `asqu open`). \
         The backups already made are still where they were.",
    )
}

fn write_links(all: &Value) -> Result<()> {
    let path = links_file()?;
    let dir = path.parent().context("no folder for the settings")?;
    fs::create_dir_all(dir)?;
    // A name of its own, so two runs writing at once never share a half-written file.
    let tmp = tempfile_in(dir, "links")?;
    fs::write(&tmp, serde_json::to_string_pretty(all)?)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Links are stored by the project's canonical path, so `/var/…` and `/private/var/…`
/// (or a path through a link) mean the same project.
pub(crate) fn key(root: &Path) -> String {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf()).display().to_string()
}

/// Where the person's default places live in the settings: they apply to every project.
const EVERYWHERE: &str = "*";

fn list_at(all: &Value, k: &str) -> Vec<Place> {
    all[k].as_array().map(|a| a.iter().filter_map(Place::from_json).collect()).unwrap_or_default()
}

fn skip_key(root: &Path) -> String {
    format!("skip:{}", key(root))
}

/// Places every project is backed up to (chosen once by the person).
pub fn everywhere() -> Vec<Place> {
    list_at(&read_links(), EVERYWHERE)
}

/// The places this project is backed up to: its own, plus the person's places for all
/// projects (unless this project was taken out of one).
pub fn places(root: &Path) -> Vec<Place> {
    let all = read_links();
    let skipped = list_at(&all, &skip_key(root));
    let mut out = list_at(&all, &key(root));
    for p in list_at(&all, EVERYWHERE) {
        if !out.contains(&p) && !skipped.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Backs up every project to `place` from now on, without setting each one up.
/// Returns false if it already was.
pub fn link_everywhere(place: &Place) -> Result<bool> {
    if let Place::Cloud { endpoint, .. } = place {
        remember_credentials(endpoint)?;
    }
    let mut all = links_for_change()?;
    let mut list = all[EVERYWHERE].as_array().cloned().unwrap_or_default();
    if list.iter().filter_map(Place::from_json).any(|p| &p == place) {
        return Ok(false);
    }
    list.push(place.to_json());
    all[EVERYWHERE] = Value::Array(list);
    write_links(&all)?;
    Ok(true)
}

/// Puts this project back into a place used by all projects, if it had been taken out.
pub fn include(root: &Path, place: &Place) -> Result<()> {
    let mut all = links_for_change()?;
    let skips = all[skip_key(root)].as_array().cloned().unwrap_or_default();
    all[skip_key(root)] = Value::Array(skips.into_iter().filter(|v| Place::from_json(v).as_ref() != Some(place)).collect());
    write_links(&all)
}

/// Stops backing every project up to `place` (backups already there are kept).
pub fn unlink_everywhere(place: &Place) -> Result<bool> {
    let mut all = links_for_change()?;
    let list = all[EVERYWHERE].as_array().cloned().unwrap_or_default();
    let kept: Vec<Value> = list.iter().filter(|v| Place::from_json(v).as_ref() != Some(place)).cloned().collect();
    let removed = kept.len() != list.len();
    all[EVERYWHERE] = Value::Array(kept);
    write_links(&all)?;
    Ok(removed)
}

/// Links the project to a place (no-op if already linked). Returns false if it already was.
pub fn link(root: &Path, place: &Place) -> Result<bool> {
    if let Place::Cloud { endpoint, .. } = place {
        remember_credentials(endpoint)?;
    }
    let mut all = links_for_change()?;
    // Linking a place this project was taken out of brings it back.
    let skips = all[skip_key(root)].as_array().cloned().unwrap_or_default();
    all[skip_key(root)] = Value::Array(skips.into_iter().filter(|v| Place::from_json(v).as_ref() != Some(place)).collect());
    let list = all[key(root)].as_array().cloned().unwrap_or_default();
    if list.iter().filter_map(Place::from_json).any(|p| &p == place) {
        write_links(&all)?;
        return Ok(false);
    }
    let mut list = list;
    list.push(place.to_json());
    all[key(root)] = Value::Array(list);
    write_links(&all)?;
    Ok(true)
}

/// Stops backing up to a place (the backup already there is left alone).
/// For a place used by all projects, this takes just this project out of it.
pub fn unlink(root: &Path, place: &Place) -> Result<bool> {
    let mut all = links_for_change()?;
    let list = all[key(root)].as_array().cloned().unwrap_or_default();
    let kept: Vec<Value> = list.iter().filter(|v| Place::from_json(v).as_ref() != Some(place)).cloned().collect();
    let mut removed = kept.len() != list.len();
    all[key(root)] = Value::Array(kept);
    if list_at(&all, EVERYWHERE).contains(place) {
        let mut skips = all[skip_key(root)].as_array().cloned().unwrap_or_default();
        if !skips.iter().filter_map(Place::from_json).any(|p| &p == place) {
            skips.push(place.to_json());
        }
        all[skip_key(root)] = Value::Array(skips);
        removed = true;
    }
    write_links(&all)?;
    Ok(removed)
}

// ---- cloud credentials ----

fn credentials_file() -> Result<PathBuf> {
    Ok(sign::home().context("no home folder")?.join("s3-credentials.json"))
}

fn env_credentials() -> Option<(String, String)> {
    let get = |a: &str, b: &str| std::env::var(a).or_else(|_| std::env::var(b)).ok().filter(|v| !v.is_empty());
    Some((
        get("ASQU_S3_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID")?,
        get("ASQU_S3_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY")?,
    ))
}

/// Saves the access keys from the environment for this endpoint, readable only by this
/// person, so background backups work from anywhere (AI hooks, the view…).
fn remember_credentials(endpoint: &str) -> Result<()> {
    let Some((id, secret)) = env_credentials() else {
        if stored_credentials(endpoint).is_some() {
            return Ok(());
        }
        bail!(
            "set your storage access keys first, e.g.\n  export ASQU_S3_ACCESS_KEY_ID=…\n  export ASQU_S3_SECRET_ACCESS_KEY=…\n\
             (Asqu then remembers them in ~/.asqu, readable only by you)"
        );
    };
    let path = credentials_file()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let mut all = read_json_for_change(&path, "Storage access keys remembered before need giving again.")?;
    all[endpoint] = json!({ "id": id, "secret": secret });
    write_private_json(&path, &all)
}

/// Writes a settings file holding secrets, readable only by this person, in one step.
pub(crate) fn write_private_json(path: &Path, all: &Value) -> Result<()> {
    let dir = path.parent().context("no folder for the settings")?;
    fs::create_dir_all(dir)?;
    // A fresh file with a name of its own: it gets the private permissions below, and two
    // runs writing at once never share it.
    let tmp = tempfile_in(dir, "settings")?;
    let written = (|| -> Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        use std::io::Write;
        opts.open(&tmp)?.write_all(serde_json::to_string_pretty(all)?.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn stored_credentials(endpoint: &str) -> Option<(String, String)> {
    let all: Value = serde_json::from_str(&fs::read_to_string(credentials_file().ok()?).ok()?).ok()?;
    let c = &all[endpoint];
    Some((c["id"].as_str()?.into(), c["secret"].as_str()?.into()))
}

// ---- talking to cloud storage (signed links, sent with curl) ----

struct Cloud {
    bucket: rusty_s3::Bucket,
    creds: rusty_s3::Credentials,
    prefix: String,
}

const LINK_LIFE: Duration = Duration::from_secs(5 * 60);

impl Cloud {
    fn new(place: &Place) -> Result<Cloud> {
        let Place::Cloud { bucket, prefix, endpoint, region } = place else { unreachable!() };
        let (id, secret) = env_credentials()
            .or_else(|| stored_credentials(endpoint))
            .context("no access keys for this storage: set ASQU_S3_ACCESS_KEY_ID and ASQU_S3_SECRET_ACCESS_KEY")?;
        let url: url::Url = endpoint.parse().context("bad endpoint address")?;
        let style = if url.host_str().is_some_and(|h| h.ends_with("amazonaws.com")) {
            rusty_s3::UrlStyle::VirtualHost
        } else {
            rusty_s3::UrlStyle::Path
        };
        let bucket = rusty_s3::Bucket::new(url, style, bucket.clone(), region.clone())
            .map_err(|e| anyhow::anyhow!("bad storage settings: {e:?}"))?;
        Ok(Cloud { bucket, creds: rusty_s3::Credentials::new(id, secret), prefix: prefix.clone() })
    }

    fn key(&self, rel: &str) -> String {
        if self.prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{rel}", self.prefix)
        }
    }

    fn put(&self, rel: &str, file: &Path) -> Result<()> {
        use rusty_s3::S3Action;
        let url = self.bucket.put_object(Some(&self.creds), &self.key(rel)).sign(LINK_LIFE);
        expect_ok(curl(&["-X", "PUT", "-T", &file.display().to_string()], url.as_str(), None)?, rel)
    }

    fn get(&self, rel: &str, dest: &Path) -> Result<()> {
        use rusty_s3::S3Action;
        let url = self.bucket.get_object(Some(&self.creds), &self.key(rel)).sign(LINK_LIFE);
        expect_ok(curl(&[], url.as_str(), Some(dest))?, rel)
    }

    fn exists(&self, rel: &str) -> Result<bool> {
        use rusty_s3::S3Action;
        let url = self.bucket.head_object(Some(&self.creds), &self.key(rel)).sign(LINK_LIFE);
        match curl(&["-I"], url.as_str(), None)? {
            200 => Ok(true),
            404 => Ok(false),
            code => bail!("the storage said {code} for {rel}"),
        }
    }

    /// Every key under `rel/` (relative to the prefix).
    fn list(&self, rel: &str) -> Result<Vec<String>> {
        use rusty_s3::S3Action;
        let full = self.key(rel);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            action.with_prefix(full.as_str());
            if let Some(t) = &token {
                action.with_continuation_token(t.as_str());
            }
            let url = action.sign(LINK_LIFE);
            let tmp = tempfile_in(&std::env::temp_dir(), "list")?;
            let code = curl(&[], url.as_str(), Some(&tmp))?;
            let body = fs::read_to_string(&tmp).unwrap_or_default();
            let _ = fs::remove_file(&tmp);
            expect_ok(code, "the list of backups")?;
            let resp = rusty_s3::actions::ListObjectsV2::parse_response(&body)
                .map_err(|e| anyhow::anyhow!("couldn't read the storage's answer: {e:?}"))?;
            let strip = if self.prefix.is_empty() { 0 } else { self.prefix.len() + 1 };
            out.extend(resp.contents.into_iter().filter_map(|c| c.key.get(strip..).map(str::to_string)));
            match resp.next_continuation_token {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }
}

/// Runs curl and returns the HTTP status. The signed link goes in through curl's input,
/// not its command line, so other programs on the computer can't see it.
fn curl(extra: &[&str], url: &str, out: Option<&Path>) -> Result<u16> {
    use std::io::Write;
    if url.contains(['"', '\\', '\n']) {
        bail!("unexpected characters in a storage link");
    }
    let out = out.map(|p| p.display().to_string()).unwrap_or_else(|| crate::platform::null_device().into());
    let mut child = Command::new("curl")
        .args(["-sS", "--retry", "2", "-o", &out, "-w", "%{http_code}", "--config", "-"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl is needed for cloud backups")?;
    child.stdin.take().context("curl input")?.write_all(format!("url = \"{url}\"\n").as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("couldn't reach the storage: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0))
}

fn expect_ok(code: u16, what: &str) -> Result<()> {
    match code {
        200..=299 => Ok(()),
        403 => bail!("the storage refused access to {what} (403): check the access keys and bucket permissions"),
        404 => bail!("{what} isn't there (404)"),
        _ => bail!("the storage said {code} for {what}"),
    }
}

// ---- making a backup ----

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `<folder name>-<first 8 characters of entry #1's hash>`, chosen at the first backup and
/// kept in the history itself, so a restored copy (in any folder) continues the same backup.
pub fn backup_name(store: &Store) -> Result<String> {
    if let Some(name) = store.meta("backup_name")? {
        // It becomes a folder name inside the backup place: only what Asqu itself writes.
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("this project's backup name is damaged; refusing to back up");
        }
        return Ok(name);
    }
    let first = store.first_hash()?.context("nothing to back up yet: save something first")?;
    let folder = store.root().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut name: String =
        folder.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    if name.trim_matches('-').is_empty() {
        name = "project".into();
    }
    let name = format!("{name}-{}", &crate::hash::hex(&first)[..8]);
    store.set_meta("backup_name", &name)?;
    Ok(name)
}

pub(crate) fn tempfile_in(dir: &Path, what: &str) -> Result<PathBuf> {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(dir.join(format!(".asqu-{what}-{}.tmp", crate::hash::hex(&b))))
}

fn large_files(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(DIR).join("large"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()))
                .collect()
        })
        .unwrap_or_default()
}

/// Backs the project up to one place now. Returns the entry number now backed up.
pub fn back_up(store: &Store, place: &Place) -> Result<i64> {
    let name = backup_name(store)?;
    let asqu_dir = store.root().join(DIR);
    let snapshot = tempfile_in(&asqu_dir, "snapshot")?;
    store.snapshot_db(&snapshot)?;
    // What the copy itself holds, not the live history a moment before or after: a save
    // in between would make `info.json` describe a different history than `store.db`.
    let (seq, head) = match latest_in(&snapshot) {
        Ok(found) => found,
        Err(e) => {
            let _ = fs::remove_file(&snapshot);
            return Err(e);
        }
    };
    let info = json!({
        "name": name,
        "seq": seq,
        "head": head,
        "time": chrono::Utc::now().to_rfc3339(),
        "asqu": env!("CARGO_PKG_VERSION"),
    });
    let result = (|| -> Result<()> {
        match place {
            Place::Folder(dir) => {
                // Everything happens through open handles to the place and the backup's
                // folder. The system then refuses any path that would lead outside them,
                // even through a link swapped in while the backup is running.
                let place_dir = cap_std::fs::Dir::open_ambient_dir(dir, cap_std::ambient_authority())
                    .with_context(|| format!("can't open {}", dir.display()))?;
                place_dir.create_dir_all(&name).context("can't create the backup folder")?;
                let dest = place_dir.open_dir(&name).context("refusing to back up through a linked folder")?;
                if let Ok(text) = dest.read_to_string("info.json") {
                    guard_against_overwrite(store, &text, &place.label())?;
                }
                dest.create_dir_all("large")?;
                let large = dest.open_dir("large").context("refusing to back up through a linked folder")?;
                // Big files first (each is named after its contents and never changes), then
                // the history that points to them, then `info.json`: stopped part-way, the
                // backup already there is still whole.
                for f in large_files(store.root()) {
                    if !large.exists(&f) {
                        copy_into(&asqu_dir.join("large").join(&f), &large, &f)?;
                    }
                }
                copy_into(&snapshot, &dest, "store.db")?;
                let tmp = temp_name("info")?;
                dest.write(&tmp, serde_json::to_string_pretty(&info)?)?;
                dest.rename(&tmp, &dest, "info.json")?;
            }
            Place::Cloud { .. } => {
                let cloud = Cloud::new(place)?;
                let tmp = tempfile_in(&asqu_dir, "theirs")?;
                let existing = match cloud.exists(&format!("{name}/info.json"))? {
                    true => cloud.get(&format!("{name}/info.json"), &tmp).and_then(|_| Ok(fs::read_to_string(&tmp)?)),
                    false => Ok(String::new()),
                };
                let _ = fs::remove_file(&tmp);
                guard_against_overwrite(store, &existing?, &place.label())?;
                // Big files first, as for a folder.
                for f in large_files(store.root()) {
                    let rel = format!("{name}/large/{f}");
                    if !cloud.exists(&rel)? {
                        cloud.put(&rel, &asqu_dir.join("large").join(&f))?;
                    }
                }
                cloud.put(&format!("{name}/store.db"), &snapshot)?;
                let tmp = tempfile_in(&asqu_dir, "info")?;
                fs::write(&tmp, serde_json::to_string_pretty(&info)?)?;
                let r = cloud.put(&format!("{name}/info.json"), &tmp);
                let _ = fs::remove_file(&tmp);
                r?;
            }
        }
        Ok(())
    })();
    let _ = fs::remove_file(&snapshot);
    result.map(|_| seq)
}

/// The latest Timeline entry in a copy of the history: its number and fingerprint (0 and
/// empty when it has none).
fn latest_in(db: &Path) -> Result<(i64, String)> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("reading the copy of the history")?;
    let latest: Option<(i64, Vec<u8>)> = rusqlite::OptionalExtension::optional(conn.query_row(
        "SELECT seq, hash FROM timeline ORDER BY seq DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ))?;
    Ok(latest.map(|(seq, h)| (seq, crate::hash::hex(&h))).unwrap_or_default())
}

/// Copies a file into `dir` (an open folder) as `name`, through a temporary file, so the
/// backup never holds a half-written file.
fn copy_into(from: &Path, dir: &cap_std::fs::Dir, name: &str) -> Result<()> {
    let tmp = temp_name("copy")?;
    let mut src = fs::File::open(from)?;
    let mut out = dir.create(&tmp).with_context(|| format!("can't write into the backup ({name})"))?;
    std::io::copy(&mut src, &mut out)?;
    out.sync_all()?;
    drop(out);
    dir.rename(&tmp, dir, name)?;
    Ok(())
}

/// A random temporary file name, e.g. `.asqu-copy-1a2b3c4d.tmp`.
fn temp_name(what: &str) -> Result<String> {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(format!(".asqu-{what}-{}.tmp", crate::hash::hex(&b)))
}

/// Refuses to replace a backup whose latest entry this project doesn't have: another
/// computer (say, after a restore) has backed up changes here that would be lost.
fn guard_against_overwrite(store: &Store, existing_info: &str, label: &str) -> Result<()> {
    let Ok(v) = serde_json::from_str::<Value>(existing_info) else { return Ok(()) };
    let Some(theirs) = v["head"].as_str().filter(|h| !h.is_empty()) else { return Ok(()) };
    if store.has_entry_hex(theirs)? {
        return Ok(());
    }
    bail!(
        "the backup at {label} has changes this computer doesn't have (up to #{} from another computer), \
         so it wasn't overwritten. Restore it into a new folder to get them (`asqu restore {label}`), \
         or stop backing up here with `asqu unlink {label}`",
        v["seq"].as_i64().unwrap_or(0)
    )
}

// ---- backup state (shown by `asqu status`) ----

fn state_path(root: &Path) -> PathBuf {
    root.join(DIR).join("backup-state.json")
}

fn read_state(root: &Path) -> Value {
    fs::read_to_string(state_path(root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}))
}

fn record(root: &Path, place: &Place, outcome: &Result<i64>) {
    let mut state = read_state(root);
    let label = place.label();
    let prev = state[&label].clone();
    state[&label] = match outcome {
        Ok(seq) => json!({ "ok": now_ms(), "seq": seq, "tried": now_ms() }),
        Err(e) => json!({ "ok": prev["ok"], "seq": prev["seq"], "tried": now_ms(), "error": format!("{e:#}") }),
    };
    let _ = crate::store::write_file_safely(&state_path(root), serde_json::to_string_pretty(&state).unwrap_or_default().as_bytes());
}

/// Backs up to every linked place now, recording how each went.
pub fn back_up_all(store: &Store) -> Vec<(Place, Result<i64>)> {
    places(store.root())
        .into_iter()
        .map(|p| {
            let r = back_up(store, &p);
            record(store.root(), &p, &r);
            (p, r)
        })
        .collect()
}

/// One line per linked place, for `asqu status`: "Backed up 2 minutes ago to …".
pub fn status_lines(root: &Path) -> Vec<String> {
    let state = read_state(root);
    let shared = everywhere();
    places(root)
        .iter()
        .map(|p| {
            let s = &state[p.label()];
            let all = if shared.contains(p) { ", the backup place for all your projects" } else { "" };
            let name = p.display();
            match (s["error"].as_str(), s["ok"].as_i64()) {
                (Some(e), _) => format!("This project's backup to {name} failed: {e}"),
                (None, Some(ok)) => format!("This project was backed up {} to {name}{all}", ago(ok)),
                _ => format!("This project isn't backed up yet to {name}{all}"),
            }
        })
        .chain(crate::hub::status_line(root))
        .collect()
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub(crate) fn ago(ms: i64) -> String {
    let secs = (now_ms() - ms).max(0) / 1000;
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86400, "day"),
    }
}

fn plural(n: i64, unit: &str) -> String {
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

// ---- automatic backups in the background ----

static AUTO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turns on automatic backups for this process (the `asqu` program does; tests don't).
pub fn enable_auto() {
    AUTO.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn auto_enabled() -> bool {
    AUTO.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn db_changed_ms(root: &Path) -> i64 {
    ["store.db", "store.db-wal"]
        .iter()
        .filter_map(|f| fs::metadata(root.join(DIR).join(f)).and_then(|m| m.modified()).ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .max()
        .unwrap_or(0)
}

/// Has the history changed since the oldest successful backup?
fn needs_backup(root: &Path) -> bool {
    if crate::hub::needs_sync(root) {
        return true;
    }
    let places = places(root);
    if places.is_empty() {
        return false;
    }
    let state = read_state(root);
    let oldest = places.iter().map(|p| state[p.label()]["ok"].as_i64().unwrap_or(0)).min().unwrap_or(0);
    db_changed_ms(root) > oldest
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(DIR).join("backup.lock")
}

fn lock_is_fresh(root: &Path) -> bool {
    fs::metadata(lock_path(root))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < STALE_LOCK)
}

/// Called after something changes: if this project is linked, starts a background copier
/// (unless one is already running; it picks up the new change before it stops).
pub fn nudge(root: &Path) {
    if !AUTO.load(std::sync::atomic::Ordering::Relaxed) || std::env::var("ASQU_AUTO_BACKUP").is_ok_and(|v| v == "0") {
        return;
    }
    if !needs_backup(root) || lock_is_fresh(root) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(["backup", "--auto"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::detach(&mut cmd);
    let _ = cmd.spawn();
}

/// The background copier: backs up (and sends new saves to the Hub, if linked), waits a
/// little, and repeats while changes keep coming.
pub fn run_auto(root: &Path) -> Result<()> {
    loop {
        let lock = lock_path(root);
        if lock_is_fresh(root) {
            return Ok(()); // another copier is on it
        }
        let _ = fs::remove_file(&lock); // stale
        if fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_err() {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            while needs_backup(root) {
                let state = read_state(root);
                let last = state.as_object().map(|o| o.values().filter_map(|v| v["tried"].as_i64()).max().unwrap_or(0));
                let last = last.unwrap_or(0).max(crate::hub::last_tried(root));
                let gap = std::env::var("ASQU_BACKUP_GAP_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(MIN_GAP);
                let wait = (last + gap.as_millis() as i64) - now_ms();
                if wait > 0 {
                    std::thread::sleep(Duration::from_millis(wait as u64));
                }
                let mut store = Store::open_at(root)?;
                let mut failed = back_up_all(&store).iter().any(|(_, r)| r.is_err());
                failed |= crate::hub::sync_and_record(&mut store).is_some_and(|r| r.is_err());
                if failed {
                    break; // try again on the next change, not in a tight loop
                }
            }
            Ok(())
        })();
        let _ = fs::remove_file(&lock);
        // A change that arrived while the lock was being released would otherwise wait.
        if result.is_err() || !needs_backup(root) || failed_last(root) {
            return result;
        }
    }
}

fn failed_last(root: &Path) -> bool {
    let state = read_state(root);
    crate::hub::failed_last(root) || places(root).iter().any(|p| state[p.label()]["error"].is_string())
}

// ---- restoring ----

pub struct Found {
    pub name: String,
    pub seq: i64,
    pub time: String,
}

fn read_info(text: &str) -> Option<Found> {
    let v: Value = serde_json::from_str(text).ok()?;
    let name = v["name"].as_str()?.to_string();
    // The name becomes a folder name: only allow what Asqu itself writes.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(Found { name, seq: v["seq"].as_i64().unwrap_or(0), time: v["time"].as_str().unwrap_or("").into() })
}

/// The backups found at a place.
pub fn find(place: &Place) -> Result<Vec<Found>> {
    let mut found = Vec::new();
    match place {
        Place::Folder(dir) => {
            for e in fs::read_dir(dir).with_context(|| format!("can't read {}", dir.display()))? {
                let p = e?.path();
                if let Some(f) = fs::read_to_string(p.join("info.json")).ok().and_then(|t| read_info(&t)) {
                    found.push(f);
                }
            }
        }
        Place::Cloud { .. } => {
            let cloud = Cloud::new(place)?;
            for k in cloud.list("")? {
                if let Some(name) = k.strip_suffix("/info.json").filter(|n| !n.contains('/')) {
                    let tmp = tempfile_in(&std::env::temp_dir(), "info")?;
                    let text = cloud.get(&format!("{name}/info.json"), &tmp).and_then(|_| Ok(fs::read_to_string(&tmp)?));
                    let _ = fs::remove_file(&tmp);
                    if let Some(f) = text.ok().and_then(|t| read_info(&t)) {
                        found.push(f);
                    }
                }
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

pub struct Restored {
    pub root: PathBuf,
    pub name: String,
    pub seq: i64,
    pub files: usize,
    /// Signing keys from elsewhere that the person accepted (now trusted here).
    pub other_keys: usize,
    /// Open Tries in the backup, set aside until the person brings them back.
    pub set_aside: Vec<String>,
}

/// Brings a project back from a backup into `to` (which must be new or empty), puts its
/// files in place exactly as they were at the last backed-up entry, and keeps it linked.
///
/// The history must be signed by keys this computer knows. Keys it doesn't know (yours
/// from another computer, or someone else's) are only accepted when listed in
/// `trust_keys` (in full); a history with no signatures at all only with
/// `allow_unsigned`. Otherwise someone able to write to the backup place could re-sign
/// or strip the history and slip in files of their own.
pub fn restore(
    place: &Place,
    which: Option<&str>,
    to: Option<&Path>,
    trust_keys: &[String],
    allow_unsigned: bool,
) -> Result<Restored> {
    let found = find(place)?;
    let pick = match which {
        Some(w) => found
            .iter()
            .find(|f| f.name == w || f.name.rsplit_once('-').is_some_and(|(n, _)| n == w))
            .with_context(|| format!("no backup called \"{w}\" at {}", place.label()))?,
        None => match found.as_slice() {
            [] => bail!("no Asqu backups found at {}", place.label()),
            [one] => one,
            many => bail!(
                "{} backups here; pick one with --project:\n{}",
                many.len(),
                many.iter().map(|f| format!("  {}  (#{}, {})", f.name, f.seq, f.time)).collect::<Vec<_>>().join("\n")
            ),
        },
    };
    let name = pick.name.clone();
    let folder_name = name.rsplit_once('-').map(|(n, _)| n.to_string()).unwrap_or_else(|| name.clone());
    let target = match to {
        Some(t) => t.to_path_buf(),
        None => std::env::current_dir()?.join(&folder_name),
    };
    if target.exists() && fs::read_dir(&target)?.next().is_some() {
        bail!("{} already has files in it; restore into a new or empty folder (--to)", target.display());
    }
    let asqu_dir = target.join(DIR);
    fs::create_dir_all(asqu_dir.join("large"))?;

    let fetched = (|| -> Result<()> {
        match place {
            Place::Folder(dir) => {
                // Someone else who can write to the backup place could swap a file for a
                // link to any file on this computer. Only plain files in plain folders
                // are ever copied, never anything a link points to.
                let plain_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
                let plain_file = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
                let src = dir.join(&name);
                if !plain_dir(&src) || !plain_file(&src.join("store.db")) {
                    bail!("the backup \"{name}\" isn't a plain folder with a plain history file (it may be a link), so it wasn't read");
                }
                fs::copy(src.join("store.db"), asqu_dir.join("store.db"))?;
                let large = src.join("large");
                if fs::symlink_metadata(&large).is_ok() && !plain_dir(&large) {
                    bail!("the backup's folder of large files is a link, so it wasn't read");
                }
                for f in fs::read_dir(&large).into_iter().flatten().filter_map(|e| e.ok()) {
                    let n = f.file_name().to_string_lossy().into_owned();
                    if n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()) && plain_file(&f.path()) {
                        fs::copy(f.path(), asqu_dir.join("large").join(&n))?;
                    }
                }
            }
            Place::Cloud { .. } => {
                let cloud = Cloud::new(place)?;
                cloud.get(&format!("{name}/store.db"), &asqu_dir.join("store.db"))?;
                for k in cloud.list(&format!("{name}/large/"))? {
                    let n = k.rsplit('/').next().unwrap_or("");
                    if n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()) {
                        cloud.get(&k, &asqu_dir.join("large").join(n))?;
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = fetched {
        let _ = fs::remove_dir_all(&asqu_dir);
        return Err(e.context("couldn't fetch the backup"));
    }
    // A large file is named after its fingerprint. Anything that doesn't match its name
    // isn't part of the history (damaged, or planted), so it isn't kept.
    for f in fs::read_dir(asqu_dir.join("large")).into_iter().flatten().filter_map(|e| e.ok()) {
        let n = f.file_name().to_string_lossy().into_owned();
        if !fs::read(f.path()).is_ok_and(|data| crate::hash::hex(&crate::hash::of(&data)) == n.to_ascii_lowercase()) {
            let _ = fs::remove_file(f.path());
        }
    }

    let mut store = Store::open_at(&target)?;
    // Check the history before writing a single file from it.
    let problems = store.verify()?;
    // Unknown signers are dealt with below; anything else means the history was damaged.
    let real: Vec<_> = problems.into_iter().filter(|p| !p.contains("unknown key")).collect();
    if !real.is_empty() {
        bail!(
            "the backup's history doesn't check out, so no files were written:\n  {}\n(the copy is left in {} to inspect)",
            real.join("\n  "),
            asqu_dir.display()
        );
    }
    let refuse = |why: String| -> anyhow::Error {
        let _ = fs::remove_dir_all(&asqu_dir);
        let _ = fs::remove_dir(&target); // only if it's empty (it was new or empty)
        anyhow::anyhow!("{why}\nNothing was restored.")
    };
    let signers = store.signers()?;
    if signers.is_empty() && store.head_seq()?.is_some() && !allow_unsigned {
        drop(store);
        return Err(refuse(
            "this backup's history isn't signed by anyone, so Asqu can't tell whether it was changed. \
             If you're sure it's yours, restore it with --allow-unsigned."
                .into(),
        ));
    }
    let known = crate::trust::trusted_keys().unwrap_or_default();
    let unknown: Vec<&Vec<u8>> = signers.iter().filter(|k| !known.iter().any(|t| &t[..] == k.as_slice())).collect();
    // The full key, not a short fingerprint: a short one could be matched by a key made
    // to look alike.
    let missing: Vec<String> = unknown
        .iter()
        .map(|k| crate::hash::hex(k))
        .filter(|full| !trust_keys.iter().any(|t| t.trim().eq_ignore_ascii_case(full)))
        .collect();
    if !missing.is_empty() {
        drop(store);
        return Err(refuse(format!(
            "this backup was signed with {} this computer doesn't know:\n{}\n\
             If it's your key from another computer, the best fix is to copy ~/.asqu/identity.key from there to here.\n\
             Or, if you recognise the key, restore with: --trust-key {}",
            if missing.len() == 1 { "a key" } else { "keys" },
            missing.iter().map(|f| format!("  key {f}")).collect::<Vec<_>>().join("\n"),
            missing.join(" --trust-key ")
        )));
    }
    for k in &unknown {
        crate::trust::trust_key(k)?;
    }
    store.set_meta("backup_name", &name)?;
    let (files, set_aside) = store.restore_files_here()?;
    let seq = store.head_seq()?.unwrap_or(0);
    let root = store.root().to_path_buf();
    // Keep backing up to the same place, continuing the same backup.
    let _ = link(&root, place);
    record(&root, place, &Ok(seq));
    Ok(Restored { root, name, seq, files, other_keys: unknown.len(), set_aside })
}
