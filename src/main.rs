use anyhow::{Context, Result};
use asqu::{hash, Store, Why};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "asqu",
    version,
    about = "Asqu. Ask. See. Undo.",
    after_help = "Copyright 2026 Scott Mackey. Fair Source (FSL-1.1-ALv2): see `asqu license`."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show what changed since the last save
    Status,
    /// Save everything that changed as one Timeline entry
    Save {
        /// Why this change was made (the request behind it)
        #[arg(short, long, default_value = "")]
        why: String,
        /// The AI agent that made the change, if any
        #[arg(long)]
        agent: Option<String>,
    },
    /// Record the person's request before changing files, so every save is labelled with it
    Intent {
        /// The request in the person's own words
        request: Option<String>,
        /// Forget the current request
        #[arg(long)]
        clear: bool,
    },
    /// Add a note to AGENTS.md so any AI records your requests (asks first)
    Setup {
        /// Don't ask, just do it
        #[arg(long)]
        yes: bool,
        /// The AI tools you use, e.g. --ai cursor,codex (see `asqu setup --list`)
        #[arg(long, value_delimiter = ',')]
        ai: Vec<String>,
        /// List the AI tools Asqu can set up
        #[arg(long)]
        list: bool,
        /// Take the notes out again
        #[arg(long)]
        remove: bool,
    },
    /// Show the Timeline, newest first
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Check that history hasn't been altered or damaged. With --receipt: check that the
    /// files here are exactly the ones a signed check receipt vouches for (works on any
    /// computer, with no history needed)
    Verify {
        /// A check receipt to confirm against the files here (default: asqu-receipt.json)
        #[arg(long, num_args = 0..=1, default_missing_value = asqu::receipt::FILE)]
        receipt: Option<std::path::PathBuf>,
        /// Accept receipts signed by this key (the full key), as well as your own.
        /// Also read from ASQU_RECEIPT_KEYS, separated by commas
        #[arg(long, requires = "receipt")]
        key: Vec<String>,
        /// Only accept a check that ran in a fresh copy
        #[arg(long, requires = "receipt")]
        clean: bool,
    },
    /// Run the project's check now and, if it passes, sign a receipt for these exact files
    Check {
        /// Run it in a fresh copy holding only the project's own files (slower, catches more)
        #[arg(long)]
        clean: bool,
        /// Allow this project's check command to run (asked once per project)
        #[arg(long)]
        trust_check: bool,
        /// Also write the receipt to asqu-receipt.json, so it travels with the files
        #[arg(long)]
        save: bool,
    },
    /// Release a version: runs the project's release step (`[release] run` in asqu.toml),
    /// but only on files that passed the check. Only a person can release.
    Release {
        /// The version's name, e.g. 1.2.0
        version: String,
        /// Show what would happen without running anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the signed check receipt for the files as they are now
    Receipt {
        /// Write it to asqu-receipt.json instead, so it travels with the files
        #[arg(long)]
        save: bool,
    },
    /// Go back one or more changes (can itself be undone)
    Undo {
        /// How many changes to go back
        #[arg(default_value_t = 1)]
        steps: i64,
        /// Show what would change without doing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Go back to a point in plain words: "Tuesday", "2 hours ago", "#3", "before the signup change"
    Back {
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
        /// Show what would change without doing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Keep running and save automatically whenever files change
    Watch {
        /// Seconds to wait after the last change before saving
        #[arg(long, default_value_t = 2)]
        settle: u64,
    },
    /// Name the current point as a milestone, e.g. `asqu mark "v1 launched"`
    Mark { name: String },
    /// Show one change in detail, e.g. `asqu show 12`
    Show { seq: i64 },
    /// A plain-English summary of recent activity
    Summary {
        /// Days back (0 = today)
        #[arg(long, default_value_t = 0)]
        days: i64,
    },
    /// Open the Asqu view in your browser (Ask / See / Undo)
    Open {
        #[arg(long, default_value_t = asqu::serve::DEFAULT_PORT)]
        port: u16,
    },
    /// Tries: separate attempts you (or any AI) can keep or toss
    Try {
        #[command(subcommand)]
        cmd: TryCmd,
    },
    /// Print Asqu's actions as tool definitions any AI model can use (function calling)
    Tools {
        /// openai (Ollama, LM Studio, llama.cpp and most others) or anthropic
        #[arg(long, default_value = "openai")]
        format: String,
    },
    /// Run one tool call, e.g. `asqu call asqu_undo '{"steps": 2}'` (prints JSON)
    Call {
        tool: String,
        /// The arguments as a JSON object
        args: Option<String>,
    },
    /// Start a local web API (this computer only) so any program can use Asqu
    Serve {
        #[arg(long, default_value_t = asqu::serve::DEFAULT_PORT)]
        port: u16,
    },
    /// Run any program (an AI tool, a script…) and record what it changes, tagged with its name
    Run {
        /// Name to record (defaults to the program's name)
        #[arg(long)]
        name: Option<String>,
        /// The program and its arguments, after `--`
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Back this project up automatically to a folder (iCloud Drive, Dropbox, a disk…) or
    /// cloud storage (s3://bucket/path), or share it on AsquHub (`asqu link hub`).
    /// With no place, shows where it's backed up and shared.
    Link {
        /// A folder, s3://bucket/path for Cloudflare R2, Amazon S3 and similar, or `hub`
        place: Option<String>,
        /// The Hub's address, for `asqu link hub` (default https://asquhub.com)
        #[arg(long)]
        hub: Option<String>,
        /// Join this existing Hub project instead of starting a new one, for `asqu link hub`
        #[arg(long)]
        project: Option<String>,
        /// The AI tool doing the linking, shown on the approval page, for `asqu link hub`
        #[arg(long)]
        agent: Option<String>,
        /// Cloud storage address, e.g. https://<account-id>.r2.cloudflarestorage.com
        #[arg(long)]
        endpoint: Option<String>,
        /// Cloud storage region (Amazon S3); "auto" for Cloudflare R2
        #[arg(long)]
        region: Option<String>,
        /// Use this place for all your projects, now and in future (set it up once)
        #[arg(long)]
        all: bool,
    },
    /// Stop backing up to a place (the backup already there is kept), or `hub` to stop
    /// sharing on AsquHub (the Hub project is kept)
    Unlink {
        place: String,
        /// Stop using it for all projects (otherwise only this one is taken out)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long)]
        region: Option<String>,
    },
    /// Keep the history small: removes tossed Tries after 7 days and saves inside kept
    /// Tries after 30 days, and compresses old versions. Never removes a Timeline entry.
    /// Runs by itself once a day.
    Tidy {
        /// Show what would be removed, without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Run as the daily background tidy
        #[arg(long, hide = true)]
        auto: bool,
    },
    /// Back up now to every linked place (this also happens automatically)
    Backup {
        /// Run as the background copier
        #[arg(long, hide = true)]
        auto: bool,
    },
    /// Bring a project back from a backup, e.g. on a new computer
    Restore {
        /// The folder or s3://bucket/path it was backed up to
        place: String,
        /// Accept a signing key this computer doesn't know (the full key, shown when
        /// restore stops), e.g. your key from another computer
        #[arg(long = "trust-key")]
        trust_key: Vec<String>,
        /// Restore a history that isn't signed at all
        #[arg(long)]
        allow_unsigned: bool,
        /// Which project, if several are backed up there
        #[arg(long)]
        project: Option<String>,
        /// Folder to restore into (new or empty); defaults to the project's name here
        #[arg(long)]
        to: Option<std::path::PathBuf>,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long)]
        region: Option<String>,
    },
    /// For AI tools' hook settings: record a request, an edit, or the end of a turn.
    /// Reads whatever the tool sends on its input; never gets in its way.
    Event {
        /// prompt, edit or done (left out: worked out from what the tool sends)
        kind: Option<String>,
        /// The AI tool's name, shown in the history ("via Codex")
        #[arg(long)]
        ai: Option<String>,
    },
    /// Show Asqu's licence (Fair Source, FSL-1.1-ALv2)
    License,
    /// Update Asqu to the newest version (it also updates itself automatically, at most once a day)
    Update {
        /// Turn automatic updates off
        #[arg(long)]
        off: bool,
        /// Turn automatic updates back on
        #[arg(long)]
        on: bool,
        /// No output (used by the automatic update)
        #[arg(long, hide = true)]
        quiet: bool,
    },
    /// Older name for `asqu event` (reads the event on stdin)
    #[command(hide = true)]
    Hook,
    /// Release pipeline: sign builds with the key in ASQU_RELEASE_KEY
    #[command(hide = true)]
    SignRelease { files: Vec<std::path::PathBuf> },
    /// Release setup: make a new release signing key (secret goes to the given file)
    #[command(hide = true)]
    ReleaseKeygen { secret_file: std::path::PathBuf },
}

#[derive(Subcommand)]
enum TryCmd {
    /// Start one or more Tries from the latest save and print their folders,
    /// e.g. `asqu try start fast-a fast-b --why "Make signup faster"` for one AI each
    Start {
        #[arg(required = true)]
        names: Vec<String>,
        /// What this Try is for
        #[arg(short, long, default_value = "")]
        why: String,
    },
    /// Record what's in a Try's folder now
    Save {
        /// Defaults to the Try whose folder you're in
        name: Option<String>,
        #[arg(short, long, default_value = "")]
        why: String,
    },
    /// List Tries
    List,
    /// Compare open Tries (all, or those named): runs each one's check and suggests the best
    Compare {
        names: Vec<String>,
        /// Don't run the check
        #[arg(long)]
        no_check: bool,
        /// Allow this project's check command to run (asked once per project)
        #[arg(long)]
        trust_check: bool,
    },
    /// Combine a Try into the main project (after the Check and the risk rule)
    Keep {
        /// Defaults to the Try whose folder you're in
        name: Option<String>,
        /// Approve a high-risk change
        #[arg(long)]
        yes: bool,
        /// Skip the Check
        #[arg(long)]
        no_check: bool,
        /// Where both sides changed a file differently, use the Try's version
        #[arg(long)]
        prefer_try: bool,
        /// After a clash, which version of a file to keep: try:<file> or main:<file> (repeat for more files)
        #[arg(long, value_name = "SIDE:FILE")]
        take: Vec<String>,
        /// Allow this project's check command to run (asked once per project)
        #[arg(long)]
        trust_check: bool,
    },
    /// Bring a Try up to date with the main project (the main project's files aren't changed)
    Update {
        /// Defaults to the Try whose folder you're in
        name: Option<String>,
        /// Show what would change in the Try's folder, without changing any files
        /// (unsaved work is saved first)
        #[arg(long)]
        dry_run: bool,
        /// After a clash, which version of a file to keep: try:<file> or main:<file> (repeat for more files)
        #[arg(long, value_name = "SIDE:FILE")]
        take: Vec<String>,
    },
    /// Throw a Try away (it can be brought back for 7 days). A Git worktree's folder is left as it is
    Toss {
        /// Defaults to the Try whose folder you're in
        name: Option<String>,
    },
    /// Bring back a tossed Try
    Reopen { name: String },
}

fn describe(store: &Store, seq: i64) -> Result<String> {
    let e = store.log(usize::MAX)?.into_iter().find(|e| e.seq == seq).context("entry not found")?;
    let what = if e.request.trim().is_empty() { e.summary.unwrap_or_default() } else { format!("\"{}\"", printable(&e.request)) };
    Ok(format!("#{seq} {what} ({})", when(e.ts)))
}

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%a %d %b %H:%M").to_string())
        .unwrap_or_default()
}

fn go_back(store: &mut Store, seq: i64, dry_run: bool) -> Result<()> {
    let label = describe(store, seq)?;
    if dry_run {
        let plan = store.plan_restore(seq)?;
        println!("Going back to {label} would:");
        if plan.is_empty() {
            println!("  change nothing (files already match).");
        }
        for p in &plan.added {
            println!("  bring back  {p}");
        }
        for p in &plan.modified {
            println!("  restore     {p}");
        }
        for p in &plan.deleted {
            println!("  remove      {p}");
        }
        return Ok(());
    }

    let request = store.log(usize::MAX)?.into_iter().find(|e| e.seq == seq).map(|e| e.request).unwrap_or_default();
    let why = Why { request: format!("Went back to #{seq} (\"{request}\")"), actor: actor(), ..Default::default() };
    let r = store.restore(seq, &why)?;
    if let Some(first) = r.saved_first {
        println!("Saved your unsaved changes first as #{first}.");
    }
    match r.entry {
        None => println!("Already matches {label}. Nothing to do."),
        Some(e) => println!(
            "Went back to {label}: {} restored, {} brought back, {} removed. Saved as #{}. Run `asqu undo` to reverse it.",
            r.changes.modified.len(),
            r.changes.added.len(),
            r.changes.deleted.len(),
            e.seq
        ),
    }
    Ok(())
}

fn update_command(off: bool, on: bool, quiet: bool) -> Result<()> {
    use asqu::update;
    if off || on {
        update::set_auto(on)?;
        println!("Automatic updates are {}.", if on { "on" } else { "off" });
        return Ok(());
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    match update::update(&exe) {
        Ok(update::Outcome::UpToDate) if !quiet => println!("Asqu is up to date ({}).", update::current()),
        Ok(update::Outcome::Updated { to }) => {
            if quiet {
                update::note_updated(&to);
            } else {
                println!("Updated Asqu from {} to {to}.", update::current());
            }
        }
        Err(e) if !quiet => return Err(e),
        _ => {}
    }
    Ok(())
}

fn ask(question: &str) -> Result<String> {
    print!("{question} ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().to_string())
}

fn setup_command(store: &mut Store, yes: bool, remove: bool, ai: Vec<String>, list: bool) -> Result<()> {
    use asqu::aitools;
    use std::io::IsTerminal;
    if list {
        println!("AI tools Asqu can set up (use with --ai):");
        for t in aitools::TOOLS {
            println!("  {:<12} {}", t.id, t.name);
        }
        println!("{} have no way to run a command on events; the AGENTS.md note and", aitools::WITHOUT_HOOKS);
        println!("the file watcher (asqu watch, or asqu open) cover them.");
        return Ok(());
    }
    let root = store.root().to_path_buf();
    let root = root.as_path();
    let interactive = !yes && std::io::stdin().is_terminal();
    // Keep these edits as their own entry, not mixed into anyone's next change.
    let record = |store: &mut Store, request: &str| -> Result<()> {
        store.save(&Why { request: request.into(), actor: actor(), ..Default::default() })?;
        Ok(())
    };
    store.save(&Why { actor: actor(), ..Default::default() })?; // anything unsaved goes first
    if remove {
        for (file, changed) in asqu::setup::remove(root)? {
            if changed {
                println!("Removed Asqu's notes from {file}.");
            }
        }
        for file in aitools::remove_all(root)? {
            println!("Removed Asqu's hooks from {}.", file.strip_prefix(root).unwrap_or(&file).display());
        }
        return record(store, "Removed Asqu's set-up for AI tools");
    }

    // Which AI tools? From --ai, or by asking.
    let mut tools: Vec<&'static aitools::AiTool> = Vec::new();
    for id in ai.iter().filter(|s| !s.trim().is_empty()) {
        tools.push(aitools::find(id).with_context(|| format!("unknown AI tool \"{id}\"; see `asqu setup --list`"))?);
    }
    if ai.is_empty()
        && interactive
        && !matches!(ask("Do you use AI tools in this project? [Y/n]")?.to_lowercase().as_str(), "n" | "no")
    {
        {
            let found = aitools::detected(root);
            println!("Which ones? Asqu switches on recording in each (pick any):");
            for (i, t) in aitools::TOOLS.iter().enumerate() {
                let mark = if found.iter().any(|f| f.id == t.id) { "  (found here)" } else { "" };
                println!("  {:>2}. {}{mark}", i + 1, t.name);
            }
            println!("  Others ({}) are covered by the AGENTS.md note.", aitools::WITHOUT_HOOKS);
            let default = if found.is_empty() { String::new() } else { " (Enter = the ones found here)".to_string() };
            let answer = ask(&format!("Numbers, separated by spaces{default}:"))?;
            if answer.is_empty() {
                tools = found;
            } else {
                for n in answer.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()) {
                    match n.parse::<usize>().ok().and_then(|n| aitools::TOOLS.get(n.wrapping_sub(1))) {
                        Some(t) if !tools.iter().any(|x| x.id == t.id) => tools.push(t),
                        Some(_) => {}
                        None => println!("  (skipped \"{n}\": not on the list)"),
                    }
                }
            }
        }
    }

    // What will change.
    let note = asqu::setup::plan(root);
    let tool_plans: Vec<aitools::Plan> = tools.iter().map(|t| aitools::plan(root, t)).collect::<Result<_>>()?;
    if note.iter().all(|p| p.up_to_date) && tool_plans.iter().all(|p| p.up_to_date) {
        println!("Already set up.");
        claude_md_hint(root);
        return Ok(());
    }
    println!("So that your AI tools save your requests in your own words, Asqu will:");
    for p in &note {
        let what = if p.up_to_date { "already up to date" } else if p.exists { "add a short note (nothing else changes)" } else { "create it with a short note" };
        println!("  {:<34} {what}", p.file);
    }
    for p in &tool_plans {
        let rel = p.file.strip_prefix(root).unwrap_or(&p.file).display().to_string();
        let what = if p.up_to_date { "already set up".to_string() } else if p.exists { format!("add Asqu's hooks for {} (nothing else changes)", p.tool.name) } else { format!("create it, with Asqu's hooks for {}", p.tool.name) };
        println!("  {rel:<34} {what}");
    }
    if !yes {
        if !interactive {
            println!("Run `asqu setup --yes` (with --ai <tools> for hooks) to go ahead.");
            return Ok(());
        }
        if matches!(ask("Go ahead? [Y/n]")?.to_lowercase().as_str(), "n" | "no") {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    for file in asqu::setup::apply(root)? {
        println!("Skipped {file}: it's a link to a file outside this project, so Asqu won't change it.");
    }
    for p in tool_plans.iter().filter(|p| !p.up_to_date) {
        match aitools::apply(root, p.tool) {
            Ok(_) => println!("Switched on recording in {}.", p.tool.name),
            Err(e) => println!("Skipped {}: {e:#}", p.tool.name),
        }
    }
    record(store, "Set up Asqu for AI tools")?;
    println!("Done. Your AI tools now record your requests; start a new session in each to pick it up.");
    if !tool_plans.is_empty() {
        println!("(The hooks run `asqu`, so it needs to be on your PATH, as the installer sets up.)");
    }
    claude_md_hint(root);
    println!("Undo all of this any time with `asqu setup --remove`.");
    Ok(())
}

fn claude_md_hint(root: &std::path::Path) {
    if asqu::setup::claude_md_hides_note(root) {
        println!();
        println!("Note: this project has a CLAUDE.md, so Claude Code reads that instead of AGENTS.md.");
        println!("To include it, add this line to the top of CLAUDE.md:  @AGENTS.md");
    }
}

/// The named Try, or the one whose folder we're in.
fn pick_try(store: &Store, name: Option<String>) -> Result<String> {
    match name {
        // A worktree's Try is named after its folder, tidied up: the folder's own name
        // (or path) is accepted too.
        Some(n) => Ok(store
            .list_tries()
            .ok()
            .filter(|all| !all.iter().any(|t| t.name == n))
            .and_then(|all| {
                let asked = std::path::Path::new(&n);
                all.into_iter()
                    .find(|t| t.worktree && (t.path == asked || t.path.file_name().is_some_and(|f| f == asked.as_os_str())))
                    .map(|t| t.name)
            })
            .unwrap_or(n)),
        None => store
            .try_for_path(&std::env::current_dir()?)
            .context("Which Try? Give its name (see `asqu try list`), or run this inside its folder."),
    }
}

/// `--take try:<file>` / `--take main:<file>`, as a choice per file.
fn takes(take: &[String]) -> Result<std::collections::BTreeMap<String, asqu::tries::Side>> {
    let mut chosen = std::collections::BTreeMap::new();
    for t in take {
        let (side, file) = t
            .split_once(':')
            .and_then(|(s, f)| Some((asqu::tries::Side::parse(s)?, f)))
            .filter(|(_, f)| !f.is_empty())
            .with_context(|| format!("--take {t}: write try:<file> or main:<file>"))?;
        if chosen.insert(file.to_string(), side).is_some_and(|before| before != side) {
            anyhow::bail!("--take names {file} twice, once for each side: choose one");
        }
    }
    Ok(chosen)
}

/// What each side has, for every file the Try and the main project changed differently.
fn print_clashes(questions: &[asqu::tries::Clash]) {
    for q in questions {
        println!("\n{}: {}.", printable(&q.path), q.what);
        for p in &q.parts {
            println!("  Around line {}, the main project now has:", p.line);
            print_quoted(&p.main);
            println!("  The Try has:");
            print_quoted(&p.try_text);
        }
    }
}

fn try_command(store: &mut Store, cmd: TryCmd) -> Result<()> {
    use asqu::tries::{KeepOptions, KeepOutcome};
    match cmd {
        TryCmd::Start { names, why } => {
            if let Some(here) = store.try_for_path(&std::env::current_dir()?) {
                println!(
                    "Note: this folder is already Try \"{here}\". A new Try starts from the main project and won't hold the work done here."
                );
            }
            for (name, dir) in names.iter().zip(store.start_tries(&names, &why, &actor())?) {
                println!("Started Try \"{name}\". Work in:\n  {}", dir.display());
            }
            if let [name] = &names[..] {
                println!("When it's good: `asqu try keep {name}`. To throw it away: `asqu try toss {name}`.");
            } else {
                println!("When they're done: `asqu try compare` shows which looks best.");
            }
        }
        TryCmd::Compare { names, no_check, trust_check } => {
            // Allowing counts only for exactly the command shown here.
            let mut allowed = None;
            if trust_check {
                if let Some(cmd) = asqu::config::load(store.root())?.check {
                    if !asqu::trust::check_is_trusted(store.root(), &cmd) && confirm_check_command(&cmd)? {
                        allowed = Some(cmd);
                    }
                }
            }
            let c = store.compare_tries(&names, !no_check, allowed.as_deref(), &actor())?;
            for (i, t) in c.tries.iter().enumerate() {
                let what = if t.request.is_empty() { String::new() } else { format!(" \"{}\"", printable(&t.request)) };
                println!("{}. {}{what}: {}.", i + 1, t.name, t.strengths());
                if let Some(note) = &t.check_note {
                    println!("   {}.", printable(note));
                }
                if let Some(check) = t.check.as_ref().filter(|c| !c.passed) {
                    for line in check.output.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev() {
                        println!("   | {}", printable(line));
                    }
                }
            }
            match &c.suggested {
                Some((name, why)) => {
                    println!("\nSuggested: {name} ({why}).");
                    if let Some((pick, reason)) = c.decider.as_ref() {
                        let reason = printable(reason);
                        if pick == name {
                            println!("Your decider agrees: {reason}");
                        } else {
                            println!("Your decider suggests {pick} instead: {reason}");
                        }
                    }
                    println!("Keep one with `asqu try keep <name>`, then toss the others with `asqu try toss <name>`.");
                }
                None => println!("\nNo suggestion: {}.", c.no_suggestion.as_deref().unwrap_or("nothing to compare")),
            }
            if !no_check && c.check_command.is_some() && !c.check_allowed {
                println!("The checks didn't run. To allow this project's check (asked once): `asqu try compare --trust-check`");
            }
        }
        TryCmd::Save { name, why } => {
            let name = pick_try(store, name)?;
            let c = store.save_try(&name, &Why { request: why, actor: actor(), ..Default::default() })?;
            if c.is_empty() {
                println!("Nothing changed in Try \"{name}\" since its last save.");
            } else {
                println!("Saved Try \"{name}\": {}.", asqu::describe::summarize(&c, None));
            }
        }
        TryCmd::List => {
            let tries = store.list_tries()?;
            if tries.is_empty() {
                println!("No Tries yet. Start one with `asqu try start <name>`.");
            }
            for t in tries {
                let what = if t.request.is_empty() { String::new() } else { format!("  \"{}\"", printable(&t.request)) };
                println!("{:<20} {:<7} from #{:<4} {} file(s) changed{what}", t.name, t.status, t.base_seq, t.changed);
                if t.worktree {
                    let gone = if t.folder_gone { " (folder removed: Asqu has its work as last saved)" } else { "" };
                    println!("{:<20} Git worktree: {}{gone}", "", printable(&t.path.display().to_string()));
                }
                if let Some(who) = store.reopened_by(&t.name).filter(|_| t.status == "open") {
                    println!("{:<20} tossed earlier; opened again by more work in its folder ({})", "", printable(&who));
                }
            }
        }
        TryCmd::Update { name, dry_run, take } => {
            use asqu::tries::UpdateOutcome;
            let name = pick_try(store, name)?;
            let chosen = takes(&take)?;
            match store.update_try(&name, &chosen, dry_run, &actor())? {
                UpdateOutcome::UpToDate => println!("Try \"{name}\" already has everything in the main project."),
                UpdateOutcome::Updated { changes, dry_run } => {
                    if dry_run {
                        println!("Bringing Try \"{name}\" up to date would change these files in its folder:");
                    } else {
                        println!("Try \"{name}\" is up to date with the main project. Changed in its folder:");
                    }
                    if changes.is_empty() {
                        println!("  nothing (its files stay as they are).");
                    }
                    for p in &changes.added {
                        println!("  added     {}", printable(p));
                    }
                    for p in &changes.modified {
                        println!("  changed   {}", printable(p));
                    }
                    for p in &changes.deleted {
                        println!("  removed   {}", printable(p));
                    }
                    if dry_run {
                        println!("No files were changed. Run it without --dry-run to do it.");
                    }
                }
                UpdateOutcome::Clash { files, questions } => {
                    println!(
                        "Can't bring it up to date yet: the Try and the main project both changed {} in ways that can't be combined. No files were changed.",
                        if files.len() == 1 { "1 file".to_string() } else { format!("{} files", files.len()) }
                    );
                    print_clashes(&questions);
                    let mut again = format!("asqu try update {name}");
                    for (path, side) in &chosen {
                        let side = if *side == asqu::tries::Side::Try { "try" } else { "main" };
                        again += &format!(" --take {side}:{}", shell_word(path).unwrap_or_else(|| "<file>".into()));
                    }
                    let q = shell_word(&files[0]).unwrap_or_else(|| "<file>".into());
                    println!("\nChoose which to keep for each file, then update again, e.g.:");
                    println!("  {again} --take try:{q}     (the Try's version)");
                    println!("  {again} --take main:{q}    (the main project's version)");
                    std::process::exit(1);
                }
            }
        }
        TryCmd::Keep { name, yes, no_check, prefer_try, take, trust_check } => {
            let name = pick_try(store, name)?;
            let chosen = takes(&take)?;
            if store.list_tries()?.iter().any(|t| t.name == name && t.status == "open" && t.folder_gone) {
                println!("The worktree folder for Try \"{name}\" is gone, so this keeps it as it was last saved.");
            }
            // Approvals only count for exactly what the person was shown: ask about the
            // result as it is now, then keep that result (if it changed meanwhile, ask again).
            let mut opts = KeepOptions { skip_check: no_check, prefer_try, take: chosen.clone(), ..Default::default() };
            let outcome = loop {
                let outcome = store.keep_try(&name, opts.clone(), &actor())?;
                match &outcome {
                    KeepOutcome::NeedsApproval { risk, fingerprint, .. } if yes && !opts.approve => {
                        println!("Keeping Try \"{name}\" is high risk: {}.", risk.reasons.join("; "));
                        if !asqu::platform::person_confirms("Keep it anyway?")? {
                            println!("Not approved. Nothing was kept.");
                            std::process::exit(1);
                        }
                        opts.approve = true;
                        opts.expect = Some(fingerprint.clone());
                    }
                    KeepOutcome::CheckNeedsTrust { command, fingerprint } if trust_check && !opts.trust_check => {
                        if !confirm_check_command(command)? {
                            println!("Not allowed. Nothing was kept.");
                            std::process::exit(1);
                        }
                        opts.trust_check = true;
                        opts.expect = Some(fingerprint.clone());
                    }
                    _ => break outcome,
                }
            };
            match outcome {
                KeepOutcome::Kept { seq: None, .. } => {
                    let set_aside = store.list_tries()?.iter().any(|t| t.name == name && t.status == "tossed");
                    if set_aside {
                        println!("You kept the main project's version of everything \"{name}\" changed, so nothing was added.");
                        println!("Closed it. Changed your mind? `asqu try reopen {name}` (for 7 days).");
                    } else {
                        println!("Try \"{name}\" had no changes to keep. Closed it.");
                    }
                }
                KeepOutcome::Kept { seq: Some(seq), risk, changes, check, receipt } => {
                    println!(
                        "Kept Try \"{name}\" as #{seq}: {}. Risk: {} ({}).",
                        asqu::describe::summarize(&changes, None),
                        risk.level.word(),
                        risk.reasons.join("; ")
                    );
                    match check {
                        Some(c) if receipt.is_some() => println!("Check passed: `{}`. Signed a receipt for these files.", printable(&c.command)),
                        Some(c) => println!("Check passed: `{}`.", printable(&c.command)),
                        None if no_check => println!("Check skipped."),
                        None => println!("No check is set up. Add `check = \"...\"` to asqu.toml to test before keeping."),
                    }
                    if store.try_for_path(&std::env::current_dir()?).is_some() {
                        println!("To undo, run `asqu undo` in the main project's folder: {}", store.root().display());
                    } else {
                        println!("Undo with `asqu undo`.");
                    }
                }
                KeepOutcome::Clash { files, questions } => {
                    println!(
                        "Can't keep yet: the Try and the main project both changed {} in ways that can't be combined.",
                        if files.len() == 1 { "1 file".to_string() } else { format!("{} files", files.len()) }
                    );
                    print_clashes(&questions);
                    // The command to run next keeps the choices already made.
                    let mut again = format!("asqu try keep {name}");
                    for (path, side) in &chosen {
                        let side = if *side == asqu::tries::Side::Try { "try" } else { "main" };
                        again += &format!(" --take {side}:{}", shell_word(path).unwrap_or_else(|| "<file>".into()));
                    }
                    println!("\nChoose which to keep for each file, then keep again, e.g.:");
                    let q = shell_word(&files[0]).unwrap_or_else(|| "<file>".into());
                    println!("  {again} --take try:{q}     (the Try's version)");
                    println!("  {again} --take main:{q}    (the main project's version)");
                    if store.try_dir(&name).join(&files[0]).is_file() {
                        println!("To combine them by hand: edit {} in the Try's folder, then use --take try:{q}.", printable(&files[0]));
                    }
                    std::process::exit(1);
                }
                KeepOutcome::NeedsApproval { risk, .. } => {
                    println!("This change is high risk: {}.", risk.reasons.join("; "));
                    println!("If that's OK, keep it with: `asqu try keep {name} --yes`.");
                    std::process::exit(1);
                }
                KeepOutcome::CheckNeedsTrust { command, .. } => {
                    println!("This project wants to run this command as its check before keeping:\n  {}", printable(&command));
                    println!("Only allow it if you trust this project. To allow it (asked once): `asqu try keep {name} --trust-check`");
                    println!("Or keep without running it: `asqu try keep {name} --no-check`");
                    std::process::exit(1);
                }
                KeepOutcome::CheckFailed { check } => {
                    println!("The check failed (`{}`), so nothing was kept. Last output:", printable(&check.command));
                    print_quoted(&check.output);
                    println!("Fix it in {} and keep again.", store.try_dir(&name).display());
                    std::process::exit(1);
                }
            }
        }
        TryCmd::Toss { name } => {
            let name = pick_try(store, name)?;
            let worktree = store.list_tries()?.iter().any(|t| t.name == name && t.worktree);
            store.toss_try(&name, &actor())?;
            println!("Tossed Try \"{name}\". Changed your mind? `asqu try reopen {name}` (for 7 days).");
            if worktree {
println!("Its Git worktree folder is left exactly as it is. If work carries on there, the Try opens again.");
            }
        }
        TryCmd::Reopen { name } => {
            let dir = if store.try_is_set_aside(&name)? {
                println!("Try \"{}\" came from a backup. Tries aren't covered by the signed history,", printable(&name));
                println!("so Asqu can't tell whether it was changed. Look at it before keeping it.");
                if !asqu::platform::person_confirms("Did you make this Try, and do you want it back?")? {
                    println!("Not brought back.");
                    return Ok(());
                }
                store.reopen_set_aside_try(&name)?
            } else {
                store.reopen_try(&name)?
            };
            println!("Try \"{name}\" is back. Work in:\n  {}", dir.display());
        }
    }
    Ok(())
}

/// Runs the check now and returns the signed receipt. Explains and stops if there isn't one.
fn checked_receipt(store: &mut Store, clean: bool, trust_check: bool) -> Result<asqu::receipt::Receipt> {
    use asqu::receipt::CheckOutcome;
    let mut allowed = false;
    let outcome = loop {
        match store.check_now(clean, allowed, &actor())? {
            CheckOutcome::NeedsTrust { command } if trust_check && !allowed => {
                if !confirm_check_command(&command)? {
                    println!("Not allowed. Nothing was run.");
                    std::process::exit(1);
                }
                allowed = true;
            }
            other => break other,
        }
    };
    match outcome {
        CheckOutcome::NoCheck => println!("No check is set up. Add `check = \"...\"` to asqu.toml."),
        CheckOutcome::NeedsTrust { command } => {
            println!("This project wants to run this command as its check:\n  {}", printable(&command));
            println!("Only allow it if you trust this project. To allow it (asked once): `asqu check --trust-check`");
        }
        CheckOutcome::Failed { check } => {
            println!("The check failed (`{}`). Last output:\n{}", printable(&check.command), printable(&check.output));
        }
        CheckOutcome::Passed { check, receipt: Some(r), .. } => {
            let how = if r.clean { " in a fresh copy" } else { "" };
            println!("Check passed{how}: `{}`.", check.command);
            println!("Signed a receipt for these {} files (key {}).", r.file_count, asqu::sign::fingerprint(&r.signer));
            return Ok(r);
        }
        CheckOutcome::Passed { check, receipt: None, no_receipt } => {
            println!("Check passed: `{}`.", check.command);
            println!("No receipt was signed: {}.", no_receipt.unwrap_or_default());
        }
    }
    std::process::exit(1);
}

/// `asqu check`: run the check now and sign a receipt if it passes.
fn check_command(store: &mut Store, clean: bool, trust_check: bool, save: bool) -> Result<()> {
    let r = checked_receipt(store, clean, trust_check)?;
    if save {
        asqu::receipt::write_beside_files(store.root(), &r)?;
        println!("Wrote {}.", asqu::receipt::FILE);
    }
    Ok(())
}

/// `asqu release`: run the project's release step, only on files that passed the check.
fn release_command(store: &mut Store, version: &str, dry_run: bool) -> Result<()> {
    let ok = !version.is_empty()
        && version.len() <= 40
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if !ok {
        anyhow::bail!("version names use letters, numbers and . - _ + (up to 40), e.g. 1.2.0");
    }
    let root = store.root().to_path_buf();
    let cfg = asqu::config::load(&root)?;
    let Some(run) = cfg.release.clone() else {
        println!("No release step is set up. Add one to asqu.toml:\n  [release]\n  run = \"...\"");
        std::process::exit(1);
    };
    // The person is shown this command before saying yes, so it must read exactly as it
    // will run: nothing invisible, nothing that rewrites the screen.
    if run.chars().any(|c| asqu::describe::is_hidden(c) || c == '\t') {
        anyhow::bail!("asqu.toml: the release command contains hidden control characters, so it can't be shown honestly; refusing to run it");
    }
    if store.marks()?.values().flatten().any(|m| m.eq_ignore_ascii_case(version)) {
        anyhow::bail!("\"{version}\" was already released or used as a milestone; pick the next version");
    }

    // The files must have passed this project's check, as they are right now.
    let good = |r: &asqu::receipt::Receipt| (r.clean || !cfg.release_clean) && Some(&r.check) == cfg.check_command(r.clean).as_ref();
    let receipt = match asqu::receipt::current(&root)?.filter(good) {
        Some(r) => r,
        None if dry_run => {
            let how = if cfg.release_clean { " in a fresh copy" } else { "" };
            println!("Would run the check first{how}, then (if you agree): {run}");
            println!("Nothing was run.");
            return Ok(());
        }
        None => {
            println!("These files haven't passed the check yet. Checking them now...");
            checked_receipt(store, cfg.release_clean, false)?
        }
    };

    println!(
        "Release {version}: these {} files passed `{}`{}.",
        receipt.file_count,
        receipt.check,
        if receipt.clean { " in a fresh copy" } else { "" }
    );
    let place = if cfg.release_clean { "in a fresh copy of those files" } else { "in the project folder" };
    println!("The release step will run {place}:\n  {run}");
    if dry_run {
        println!("Nothing was run.");
        return Ok(());
    }
    // Releasing reaches other people, so only the person can start it. Their yes also
    // allows this project's release command (a project can't vouch for itself).
    if !asqu::platform::person_confirms(&format!("Release {version} now?"))? {
        println!("Not released.");
        std::process::exit(1);
    }

    // Between the check and the person's yes, nothing may have changed: not the files,
    // not the command they were shown.
    if asqu::receipt::fingerprint(&root)?.0 != receipt.files || asqu::config::load(&root)?.release.as_deref() != Some(run.as_str()) {
        println!("The project changed after it was checked, so nothing was released. Run `asqu release {version}` again.");
        std::process::exit(1);
    }
    // The release step works from a fresh copy of exactly the checked files (unless the
    // project turned that off), so nothing unchecked on this computer can end up in it.
    let copy = if cfg.release_clean { Some(store.copy_of_checked_files(&receipt, &actor())?) } else { None };
    let status = asqu::platform::shell(&run)
        .current_dir(copy.as_deref().unwrap_or(&root))
        .env("ASQU_RELEASE_VERSION", version)
        .env("ASQU_RELEASE_RECEIPT", asqu::platform::plain_path(&asqu::receipt::stored_path(&root)))
        .env("ASQU_PROJECT_DIR", asqu::platform::plain_path(&root))
        .status();
    if let Some(copy) = &copy {
        let _ = std::fs::remove_dir_all(copy);
    }
    let status = status.with_context(|| format!("couldn't run the release step `{run}`"))?;
    if !status.success() {
        println!("The release step failed, so {version} isn't recorded as released.");
        std::process::exit(1);
    }
    if asqu::receipt::fingerprint(&root)?.0 != receipt.files {
        println!("Note: the project's files changed during the release, so they no longer match what was checked.");
    }
    let seq = store.mark(version, &actor())?;
    println!("Released {version} (#{seq}). Return to it any time with `asqu back \"{version}\"`.");
    Ok(())
}

/// `asqu verify --receipt`: are the files here exactly the ones a receipt vouches for?
/// Needs no history, and never creates one or a key, so it can run anywhere.
fn verify_receipt(file: &std::path::Path, keys: &[String], clean: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    // The files here, even in a worktree recorded as a Try of another folder.
    let root = Store::files_root(&cwd).unwrap_or(cwd);
    let text = std::fs::read_to_string(file).with_context(|| format!("can't read the receipt {}", file.display()))?;
    let receipt = asqu::receipt::Receipt::parse(&text)?;

    let mut accepted = asqu::receipt::known_keys();
    let from_env = std::env::var("ASQU_RECEIPT_KEYS").unwrap_or_default();
    for k in keys.iter().map(String::as_str).chain(from_env.split(',')).filter(|k| !k.trim().is_empty()) {
        accepted.push(asqu::trust::parse_key(k).with_context(|| format!("\"{}\" isn't a full key (64 letters and numbers)", k.trim()))?);
    }
    // The receipt has to be for the check this project asks for, run the way it asks.
    let cfg = asqu::config::load(&root)?;
    let expect = asqu::receipt::Expect {
        keys: accepted,
        check: cfg.check_command(receipt.clean),
        clean: clean || cfg.clean,
        nothing_uncovered: true,
    };
    let mut problems = asqu::receipt::verify(&root, &receipt, &expect)?;
    if expect.check.is_none() {
        problems.push("this project has no check set up, so there's nothing a receipt could vouch for".to_string());
    }
    if problems.is_empty() {
        let how = if receipt.clean { ", in a fresh copy" } else { "" };
        println!(
            "Receipt holds: these {} files passed `{}`{how} ({}).",
            receipt.file_count,
            receipt.check,
            when(receipt.passed_at)
        );
        println!("Checked on the signer's computer (key {}). Not independently verified.", asqu::sign::fingerprint(&receipt.signer));
        return Ok(());
    }
    println!("The receipt doesn't hold:");
    for p in &problems {
        println!("  {p}");
    }
    std::process::exit(1);
}

fn tidy_command(store: &mut Store, dry_run: bool) -> Result<()> {
    let r = store.tidy(dry_run)?;
    let will = if dry_run { "would be " } else { "" };
    let mut said = false;
    if !r.tossed_tries.is_empty() {
        println!("Tossed over 7 days ago, {will}removed for good: {}.", printable(&r.tossed_tries.join(", ")));
        said = true;
    }
    if r.try_saves > 0 {
        println!("{} made over 30 days ago inside Tries that were kept {will}removed (their results stay in the Timeline).", asqu::compare::plural(r.try_saves, "save"));
        said = true;
    }
    if r.versions > 0 {
        println!("{} nothing needs any more {will}removed.", asqu::compare::plural(r.versions, "stored file version"));
        said = true;
    }
    if r.compressed > 0 {
        if dry_run {
            println!("{} saved before the smaller format would be converted, where that makes them smaller.", asqu::compare::plural(r.compressed, "file version"));
        } else {
            println!("{} saved before the smaller format now stored in it.", asqu::compare::plural(r.compressed, "file version"));
        }
        said = true;
    }
    if r.rollback_copy_removed {
        println!("Removed the copy of the history kept for older Asqu versions (over 7 days old).");
        said = true;
    }
    if !said {
        println!("Nothing to tidy.");
    }
    if !dry_run && said && r.bytes_after < r.bytes_before {
        println!("History: {} → {}.", size_words(r.bytes_before), size_words(r.bytes_after));
    }
    println!("Every Timeline entry is kept, always.");
    Ok(())
}

fn size_words(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

fn print_quoted(text: &str) {
    if text.is_empty() {
        println!("    (nothing)");
    }
    for line in text.lines() {
        println!("    | {}", printable(line));
    }
}

use asqu::describe::printable;

/// Shows the person a project's check command and asks whether it may run. A command with
/// hidden characters (which could make it look like something else) is never offered.
fn confirm_check_command(cmd: &str) -> Result<bool> {
    if cmd.chars().any(asqu::describe::is_hidden) {
        println!("This project's check command contains hidden characters, so Asqu won't offer to run it:");
        println!("  {}", printable(cmd));
        println!("Look at `check` in asqu.toml.");
        return Ok(false);
    }
    println!("This project wants to run this command as its check:\n  {cmd}");
    asqu::platform::person_confirms("Only allow it if you trust this project. Allow it?")
}

/// A file name as it can be safely pasted after --take: plain names as they are, others
/// in single quotes (nothing inside them is run). None if it can't be typed safely.
fn shell_word(s: &str) -> Option<String> {
    if s.chars().any(asqu::describe::is_hidden) {
        None
    } else if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) {
        Some(s.to_string())
    } else {
        Some(format!("'{}'", s.replace('\'', "'\\''")))
    }
}

/// Mentions, once each, big files left out of the history.
fn tell_skipped(store: &Store) {
    let Ok(new) = store.newly_skipped_large() else { return };
    if new.is_empty() {
        return;
    }
    let max = asqu::config::load(store.root())
        .ok()
        .and_then(|c| c.max_file_bytes)
        .map(|b| b / 1024 / 1024)
        .unwrap_or(asqu::config::DEFAULT_MAX_FILE_MB);
    eprintln!("asqu: not saving {} big file(s) over {max} MB in the history:", new.len());
    for p in new.iter().take(10) {
        eprintln!("  {p}");
    }
    if new.len() > 10 {
        eprintln!("  …and {} more", new.len() - 10);
    }
    eprintln!("They stay as they are, and undo never touches them. To keep one in the history anyway,");
    eprintln!("add it to asqu.toml, e.g.  include = [\"{}\"]   (or change the limit: max_file_mb = 250)", new[0]);
}

/// `asqu link hub`: asks the Hub for a link, shows the owner where to approve it, waits,
/// keeps the key in ~/.asqu and sends the history so far. For asquhub.com the owner's
/// approval in the browser is what allows it, so an AI may run this (unlike backup links).
fn link_hub(store: &mut Store, hub: Option<&str>, project: Option<&str>, agent: Option<String>) -> Result<()> {
    let hub = asqu::hub::parse_hub(hub.unwrap_or(asqu::hub::DEFAULT_HUB))?;
    // The owner who approves is whoever runs that Hub, so a Hub other than asquhub.com
    // (or one on this computer, for testing) needs the person, like a backup place:
    // otherwise text planted in a project could get an AI to send it somewhere else.
    if !asqu::hub::is_default_or_local(&hub)
        && !asqu::platform::person_confirms(&format!("Share this project's whole history with the Hub at {hub}?"))?
    {
        println!("Not linked.");
        return Ok(());
    }
    // Joining a project that already exists hands this history to whoever owns that
    // project, and they are the one who approves. So only the person can ask for it: an AI
    // told to (by text planted in a project) can't send the history to someone else's.
    if let Some(id) = project {
        if !asqu::platform::person_confirms(&format!(
            "Share this project's whole history with the existing Hub project {} (its owner will be able to read it)?",
            printable(id)
        ))? {
            println!("Not linked.");
            return Ok(());
        }
    }
    if let Some(l) = asqu::hub::link(store.root()) {
        println!(
            "Already linked to the Hub at {} (project \"{}\", {}). To link again, run `asqu unlink hub` first.",
            asqu::hub::hub_name(&l.hub),
            l.project_name,
            l.level()
        );
        return Ok(());
    }
    let client = agent
        .or_else(|| std::env::var("ASQU_AGENT").ok())
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| "Asqu".into());
    let folder = store.root().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let pending = asqu::hub::start(&hub, &client, &asqu::platform::computer_name(), &folder, project)?;
    println!("Open {} and check the code matches {}", pending.approve_url, pending.code);
    println!("Waiting for the project's owner to approve the link (one tap)...");
    if std::env::var_os("ASQU_NO_BROWSER").is_none() {
        asqu::platform::open_url(&pending.approve_url);
    }
    let link = asqu::hub::wait(&hub, &pending)?;
    if project.is_some_and(|id| id != link.project_id) {
        anyhow::bail!("the Hub linked a different project than the one asked for, so nothing was shared");
    }
    asqu::hub::remember(store, &link)?;
    println!(
        "Linked to the Hub at {}: project \"{}\" ({}).",
        asqu::hub::hub_name(&link.hub),
        link.project_name,
        link.level()
    );
    if store.head_seq()?.is_none() {
        store.save(&Why { request: "Starting point".into(), actor: actor(), agent: Some(client), ..Default::default() })?;
    }
    match asqu::hub::sync_and_record(store) {
        Some(Ok(seq)) => println!("Shared the history so far (up to #{seq}). From now on every save is shared automatically."),
        Some(Err(e)) => println!("Linked, but sharing stopped: {e:#}"),
        None => {}
    }
    Ok(())
}

fn actor() -> String {
    asqu::platform::user_name()
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // An AI tool's event must never get in its way: report problems quietly and always
    // succeed. (`hook` is the older name, still accepted.)
    if matches!(cli.command, Command::Event { .. } | Command::Hook) {
        asqu::backup::enable_auto();
        let (kind, ai) = match &cli.command {
            Command::Event { kind, ai } => (kind.as_deref().and_then(asqu::event::Kind::parse), ai.as_deref()),
            _ => (None, None),
        };
        let mut input = String::new();
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
        }
        if let Err(e) = asqu::event::handle(kind, ai, &input, &actor()) {
            eprintln!("asqu: {e:#}");
        }
        return Ok(());
    }

    // Commands that don't touch a project.
    match &cli.command {
        Command::Update { off, on, quiet } => return update_command(*off, *on, *quiet),
        Command::License => {
            print!("{}", include_str!("../LICENSE"));
            return Ok(());
        }
        Command::SignRelease { files } => return asqu::update::sign_release(files),
        Command::ReleaseKeygen { secret_file } => {
            println!("{}", asqu::update::new_release_key(secret_file)?);
            return Ok(());
        }
        Command::Verify { receipt: Some(file), key, clean } => return verify_receipt(file, key, *clean),
        Command::Restore { place, project, to, endpoint, region, trust_key, allow_unsigned } => {
            let place = asqu::backup::Place::parse(place, endpoint.as_deref(), region.as_deref())?;
            if (!trust_key.is_empty() || *allow_unsigned)
                && !asqu::platform::person_confirms("Accept this history even though this computer can't vouch for its signatures?")?
            {
                println!("Not restored.");
                return Ok(());
            }
            println!("Restoring from {}...", place.label());
            let r = asqu::backup::restore(&place, project.as_deref(), to.as_deref(), trust_key, *allow_unsigned)?;
            println!("Restored \"{}\" into {}: {} file(s), history up to #{}.", r.name, r.root.display(), r.files, r.seq);
            println!("It stays backed up to {}.", place.label());
            if !r.set_aside.is_empty() {
                println!(
                    "{} open Tr{} set aside, because Tries aren't covered by the signed history: {}.",
                    r.set_aside.len(),
                    if r.set_aside.len() == 1 { "y was" } else { "ies were" },
                    r.set_aside.iter().map(|n| printable(n)).collect::<Vec<_>>().join(", ")
                );
                println!("If you made it, bring it back with `asqu try reopen <name>` (within 7 days).");
            }
            if r.other_keys > 0 {
                println!("The key it was signed with is now trusted on this computer.");
            }
            return Ok(());
        }
        _ => {}
    }

    // Changes made by this run are copied to linked backup places in the background.
    if !matches!(cli.command, Command::Backup { .. } | Command::Tidy { auto: true, .. }) {
        asqu::backup::enable_auto();
    }

    // Normal use: mention a finished automatic update, and maybe start a quiet check
    // (at most once a day, in the background). Never for AI tool calls or servers.
    if !matches!(cli.command, Command::Call { .. } | Command::Serve { .. } | Command::Tidy { auto: true, .. }) {
        asqu::update::tidy_old_copy();
        if let Some(note) = asqu::update::take_notice() {
            eprintln!("{note}");
        }
        asqu::update::maybe_start_auto_update();
    }

    let mut store = Store::open_or_create(&std::env::current_dir()?)?;
    let project_root = store.root().to_path_buf();

    match cli.command {
        Command::Hook
        | Command::Event { .. }
        | Command::Update { .. }
        | Command::SignRelease { .. }
        | Command::ReleaseKeygen { .. }
        | Command::Restore { .. }
        | Command::License => unreachable!(),
        Command::Link { place: Some(place), hub, project, agent, all, .. } if place == "hub" => {
            if all {
                anyhow::bail!("a Hub link is made for one project at a time; leave out --all");
            }
            link_hub(&mut store, hub.as_deref(), project.as_deref(), agent)?;
        }
        Command::Unlink { place, .. } if place == "hub" => {
            let name = asqu::hub::link(store.root()).map(|l| asqu::hub::hub_name(&l.hub).to_string());
            if asqu::hub::unlink(store.root())? {
                println!(
                    "Stopped sharing this project with the Hub at {}. The Hub project and its history are kept.",
                    name.unwrap_or_default()
                );
            } else {
                println!("This project isn't linked to the Hub.");
            }
        }
        Command::Link { place: None, .. } => {
            let places = asqu::backup::places(store.root());
            if places.is_empty() && asqu::hub::link(store.root()).is_none() {
                println!("This project isn't backed up anywhere yet. Choose a place once for all your projects, e.g.:");
                println!("  asqu link --all ~/Library/Mobile\\ Documents/com~apple~CloudDocs/Asqu   (iCloud Drive)");
                println!("  asqu link --all s3://my-bucket/asqu --endpoint https://<account-id>.r2.cloudflarestorage.com");
                println!("(Leave out --all to back up just this project.)");
                println!("Or share it on AsquHub, for people and AI agents working together: asqu link hub");
            }
            for line in asqu::backup::status_lines(store.root()) {
                println!("{line}");
            }
        }
        Command::Link { place: Some(place), endpoint, region, all, .. } => {
            let place = asqu::backup::Place::parse(&place, endpoint.as_deref(), region.as_deref())?;
            if all {
                // Asking can only be skipped if nothing would change: the place is already
                // used for all projects *and* this project isn't taken out of it.
                let nothing_changes = asqu::backup::everywhere().contains(&place)
                    && asqu::backup::places(store.root()).contains(&place);
                if !nothing_changes
                    && !asqu::platform::person_confirms(&format!(
                        "Back up ALL your Asqu projects to {} from now on?",
                        place.label()
                    ))?
                {
                    println!("Not linked.");
                    return Ok(());
                }
                asqu::backup::link_everywhere(&place)?;
                println!("All your Asqu projects now back up to {} automatically.", place.label());
            } else if !asqu::backup::places(store.root()).contains(&place)
                && !asqu::platform::person_confirms(&format!(
                    "Copy this project's whole history to {} from now on?",
                    place.label()
                ))?
            {
                println!("Not linked.");
                return Ok(());
            }
            if store.head_seq()?.is_none() {
                store.save(&Why { request: "Starting point".into(), actor: actor(), ..Default::default() })?;
            }
            let root = store.root().to_path_buf();
            if all {
                // This project too, even if it had been taken out of that place before.
                asqu::backup::include(&root, &place)?;
            } else if !asqu::backup::link(&root, &place)? {
                println!("Already backed up to {}.", place.label());
            }
            print!("Backing up this project to {}... ", place.label());
            match asqu::backup::back_up_all(&store).into_iter().find(|(p, _)| p == &place).map(|(_, r)| r) {
                Some(Ok(seq)) => {
                    println!("done (history up to #{seq}).");
                    println!("From now on every change is backed up automatically. On another computer:");
                    println!("  asqu restore {}", place.label());
                }
                Some(Err(e)) => {
                    println!("failed.");
                    return Err(e.context("the place is linked, but the first backup failed (it will retry on the next change)"));
                }
                None => {}
            }
        }
        Command::Unlink { place, endpoint, region, all } => {
            let place = match asqu::backup::Place::parse(&place, endpoint.as_deref(), region.as_deref()) {
                Ok(p) => p,
                Err(_) => asqu::backup::places(store.root())
                    .into_iter()
                    .find(|p| p.label() == place)
                    .with_context(|| format!("this project isn't backed up to {place}"))?,
            };
            let question = if all {
                format!("Stop backing up ALL projects to {}?", place.label())
            } else {
                format!("Stop backing up this project to {}?", place.label())
            };
            if !asqu::platform::person_confirms(&question)? {
                println!("Still backing up there.");
                return Ok(());
            }
            if all {
                if asqu::backup::unlink_everywhere(&place)? {
                    println!("Projects no longer back up to {} by default. Backups already there are kept.", place.label());
                } else {
                    println!("{} wasn't used for all projects.", place.label());
                }
            } else if asqu::backup::unlink(store.root(), &place)? {
                println!("Stopped backing up to {}. The backup already there is kept.", place.label());
            } else {
                println!("This project wasn't backed up to {}.", place.label());
            }
        }
        Command::Tidy { auto: true, .. } => {
            let root = store.root().to_path_buf();
            drop(store);
            return asqu::tidy::run_auto(&root);
        }
        Command::Tidy { dry_run, auto: false } => tidy_command(&mut store, dry_run)?,
        Command::Backup { auto: true } => {
            let root = store.root().to_path_buf();
            drop(store);
            return asqu::backup::run_auto(&root);
        }
        Command::Backup { auto: false } => {
            let results = asqu::backup::back_up_all(&store);
            let shared = asqu::hub::sync_and_record(&store);
            if results.is_empty() && shared.is_none() {
                println!("This project isn't backed up anywhere yet. Run `asqu link <folder>` to start.");
            }
            let mut failed = false;
            match shared {
                Some(Ok(seq)) => println!("Shared with the Hub (history up to #{seq})."),
                Some(Err(e)) => {
                    failed = true;
                    println!("Sharing with the Hub stopped: {e:#}");
                }
                None => {}
            }
            for (place, r) in results {
                match r {
                    Ok(seq) => println!("Backed up to {} (history up to #{seq}).", place.label()),
                    Err(e) => {
                        failed = true;
                        println!("Backup to {} failed: {e:#}", place.label());
                    }
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        Command::Run { name, command } if store.try_for_path(&std::env::current_dir()?).is_some() => {
            // Inside a Try's folder: what the program changes there belongs to the Try.
            let in_try = store.try_for_path(&std::env::current_dir()?).unwrap_or_default();
            let name = name.unwrap_or_else(|| asqu::run::default_name(&command));
            let program = command.first().context("no command given")?;
            eprintln!("asqu: recording changes made by {name} in Try \"{in_try}\"…");
            store.save_try(&in_try, &Why { actor: actor(), ..Default::default() })?; // earlier changes aren't the program's
            let status = std::process::Command::new(program)
                .args(&command[1..])
                .status()
                .with_context(|| format!("couldn't run {program}"))?;
            let c = store.save_try(&in_try, &Why { actor: actor(), agent: Some(name.clone()), ..Default::default() })?;
            if c.is_empty() {
                eprintln!("asqu: {name} didn't change any files.");
            } else {
                eprintln!("asqu: {name} changed {} file(s), saved in Try \"{in_try}\". Keep them with `asqu try keep {in_try}`.", c.count());
            }
            std::process::exit(status.code().unwrap_or(1));
        }
        Command::Watch { settle } if store.try_for_path(&std::env::current_dir()?).is_some() => {
            let in_try = store.try_for_path(&std::env::current_dir()?).unwrap_or_default();
            let dir = store.try_dir(&in_try);
            println!("Asqu is watching Try \"{in_try}\" ({}) — changes are saved in the Try automatically. Ctrl+C to stop.", dir.display());
            let mut save = || -> Result<()> {
                let c = store.save_try(&in_try, &Why { actor: actor(), ..Default::default() })?;
                if !c.is_empty() {
                    println!("  saved {} file(s) in Try \"{in_try}\"", c.count());
                }
                Ok(())
            };
            save()?;
            asqu::watch::run(&dir, std::time::Duration::from_secs(settle), save)?;
        }
        Command::Check { .. } | Command::Release { .. } | Command::Mark { .. }
            if store.try_for_path(&std::env::current_dir()?).is_some() =>
        {
            let name = store.try_for_path(&std::env::current_dir()?).unwrap_or_default();
            anyhow::bail!(
                "You're in the folder of Try \"{name}\". This command works on the main project, so run it from its folder: {}\n\
                 The Try's own work is checked when you keep it: `asqu try keep {name}`.",
                store.root().display()
            );
        }
        Command::Run { name, command } => {
            let name = name.unwrap_or_else(|| asqu::run::default_name(&command));
            eprintln!("asqu: recording changes made by {name}…");
            let root = store.root().to_path_buf();
            drop(store);
            let r = asqu::run::run(&root, &name, &command, &actor(), std::time::Duration::from_secs(2))?;
            match r.entries.as_slice() {
                [] => eprintln!("asqu: {name} didn't change any files."),
                seqs => eprintln!(
                    "asqu: {name} made {} change(s), saved as {}. Undo with `asqu undo`.",
                    seqs.len(),
                    seqs.iter().map(|s| format!("#{s}")).collect::<Vec<_>>().join(", ")
                ),
            }
            std::process::exit(r.exit_code);
        }
        Command::Undo { .. } | Command::Back { .. } if store.try_for_path(&std::env::current_dir()?).is_some() => {
            let name = store.try_for_path(&std::env::current_dir()?).unwrap_or_default();
            anyhow::bail!("{}", store.main_only_note(&name));
        }
        Command::Undo { steps, dry_run } => {
            let target = asqu::tools::undo_target(&store, steps)?;
            go_back(&mut store, target, dry_run)?;
        }
        Command::Back { words, dry_run } => {
            let seq = asqu::tools::words_target(&store, &words.join(" "))?;
            go_back(&mut store, seq, dry_run)?;
        }
        Command::Try { cmd } => try_command(&mut store, cmd)?,
        Command::Intent { request, clear } => {
            if clear {
                match store.try_for_path(&std::env::current_dir()?) {
                    Some(name) => store.clear_try_intent(&name)?,
                    None => store.clear_intent(),
                }
                println!("Cleared the current request.");
            } else if let Some(name) = store.try_for_path(&std::env::current_dir()?) {
                let request = request.context("say what the person asked for, e.g. asqu intent \"Make signup faster\"")?;
                store.set_try_intent(&name, &request)?;
                println!("Got it. Changes in Try \"{name}\" will be saved as: \"{}\"", request.trim());
            } else {
                let request = request.context("say what the person asked for, e.g. asqu intent \"Make signup faster\"")?;
                store.set_intent(&request)?;
                println!("Got it. Changes will be saved as: \"{}\"", request.trim());
            }
        }
        Command::Setup { yes, remove, ai, list } => setup_command(&mut store, yes, remove, ai, list)?,
        Command::Mark { name } => {
            let seq = store.mark(&name, &actor())?;
            println!("Marked #{seq} as \"{name}\". Return any time with `asqu back \"{name}\"`.");
        }
        Command::Show { seq } => {
            println!("{}", describe(&store, seq)?);
            if let Some(names) = store.marks()?.remove(&seq) {
                println!("  milestone: {}", names.join(", "));
            }
            for f in store.entry_files(seq)? {
                let word = match f.kind.as_str() { "add" => "added", "delete" => "removed", _ => "changed" };
                println!("  {word:<8} {}", f.path);
            }
        }
        Command::Summary { days } => {
            let label = match days { 0 => "Today".to_string(), 1 => "Since yesterday".to_string(), n => format!("Last {n} days") };
            println!("{label}: {}", store.digest(asqu::see::start_of_day(days))?.sentence());
        }
        Command::Open { port } => {
            let root = store.root().to_path_buf();
            // A brand-new folder: save it as the starting point, so the view isn't empty.
            if store.head_seq()?.is_none() {
                if let Some(r) = store.save(&Why { request: "Starting point".into(), actor: actor(), ..Default::default() })? {
                    println!("Saved this folder as the starting point (#{}, {} file(s)).", r.seq, r.changes.count());
                }
            }
            drop(store);

            // While the view is open, save changes as they happen, so it stays up to date.
            let watch_root = root.clone();
            let who = actor();
            std::thread::spawn(move || {
                let Ok(mut s) = Store::open_at(&watch_root) else { return };
                let _ = asqu::watch::run(&watch_root, std::time::Duration::from_secs(2), || {
                    s.save(&Why { actor: who.clone(), ..Default::default() })?;
                    Ok(())
                });
            });

            // Another `asqu open` (another project) may already use the port: take the next free one.
            let port = (port..port.saturating_add(20))
                .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
                .context("couldn't find a free port for the Asqu view")?;
            let secret = asqu::serve::new_person_secret()?;
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let one_time = asqu::serve::new_person_secret()?;
            asqu::serve::serve(&root, port, &actor(), Some(&secret), Some(&one_time), |addr| {
                // The secret rides after `#`: the browser keeps it, the server never sees or logs it.
                let url = format!("http://{addr}/#person={secret}");
                println!("Opening the Asqu view for \"{name}\" in your browser.");
                println!("While this runs, changes in this folder are saved automatically. Press Ctrl+C to stop.");
                // The link holds the key that lets you approve changes: only show it to a
                // person at a terminal, never to a program (such as an AI) reading the output.
                if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                    println!();
                    println!("If the browser didn't open, use this link. The long code in it is a private key for");
                    println!("this session, so only you can approve changes. Don't share it:");
                    println!("  {url}");
                }
                // The browser gets a one-time address: the first visit hands the page its
                // key and uses the code up. Neither the key nor the code is ever on a
                // command line: the browser opens a page only this person can read, which
                // sends it on. The page is removed once the code has expired.
                if std::env::var_os("ASQU_NO_BROWSER").is_none() {
                    match asqu::serve::handover_page(&format!("http://{addr}/open/{one_time}")) {
                        Ok(page) => {
                            asqu::platform::open_url(&page.to_string_lossy());
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_secs(65));
                                let _ = std::fs::remove_file(page);
                            });
                        }
                        Err(e) => eprintln!("asqu: couldn't open the browser ({e:#}); use the link above."),
                    }
                }
            })?;
        }
        Command::Tools { format } => {
            println!("{}", serde_json::to_string_pretty(&asqu::tools::export(&format)?)?);
        }
        Command::Call { tool, args } => {
            let args: serde_json::Value = match args.as_deref() {
                None | Some("") => serde_json::json!({}),
                Some(a) => serde_json::from_str(a).context("arguments must be JSON, e.g. '{\"steps\": 2}'")?,
            };
            let root = store.root().to_path_buf();
            drop(store);
            // Called from inside a Try's folder (a worktree, say), a tool acts on that Try.
            asqu::tools::set_caller_folder(&std::env::current_dir()?);
            match asqu::tools::call(&root, &tool, &args, &actor()) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                Err(e) => {
                    println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
                    std::process::exit(1);
                }
            }
        }
        Command::Serve { port } => {
            let root = store.root().to_path_buf();
            drop(store);
            asqu::serve::api_token()?; // make sure the key exists before programs look for it
            asqu::serve::serve(&root, port, &actor(), None, None, |addr| {
                println!("Asqu API for {} at http://{addr}", root.display());
                println!("Every request needs the headers `X-Asqu: 1` and `X-Asqu-Token: <key>`, where the key is");
                println!("in ~/.asqu/api-token (readable only by you). Ctrl+C to stop.");
            })?;
        }
        Command::Watch { settle } => {
            let root = store.root().to_path_buf();
            println!("Asqu is watching {} — changes are saved automatically. Ctrl+C to stop.", root.display());
            let mut save = || -> Result<()> {
                let why = Why { actor: actor(), ..Default::default() };
                if let Some(r) = store.save(&why)? {
                    println!("  saved #{} ({} file(s))", r.seq, r.changes.count());
                }
                tell_skipped(&store);
                Ok(())
            };
            save()?;
            asqu::watch::run(&root, std::time::Duration::from_secs(settle), save)?;
        }
        Command::Status if store.try_for_path(&std::env::current_dir()?).is_some() => {
            let name = store.try_for_path(&std::env::current_dir()?).unwrap_or_default();
            let state = store.list_tries()?.into_iter().find(|t| t.name == name).map(|t| t.status).unwrap_or_default();
            let c = store.try_changes(&name)?;
            println!("This folder is Try \"{name}\" ({state}) of the main project in {}.", store.root().display());
            if c.is_empty() {
                println!("Nothing here differs from where it started.");
            } else {
                for p in &c.added {
                    println!("  added     {}", printable(p));
                }
                for p in &c.modified {
                    println!("  changed   {}", printable(p));
                }
                for p in &c.deleted {
                    println!("  removed   {}", printable(p));
                }
                println!("{} file(s) differ from where it started.", c.count());
            }
            println!("Keep its work: `asqu try keep {name}`. Bring in the main project's changes: `asqu try update {name}`.");
        }
        Command::Status => {
            let c = store.status()?;
            tell_skipped(&store);
            if c.is_empty() {
                println!("Nothing changed since the last save.");
            } else {
                for p in &c.added {
                    println!("  added     {p}");
                }
                for p in &c.modified {
                    println!("  changed   {p}");
                }
                for p in &c.deleted {
                    println!("  removed   {p}");
                }
                println!("{} file(s) changed.", c.count());
            }
            let open: Vec<_> = store.list_tries()?.into_iter().filter(|t| t.status == "open").collect();
            if !open.is_empty() {
                let names: Vec<String> = open
                    .iter()
                    .map(|t| if t.folder_gone { format!("{} (folder removed)", t.name) } else { t.name.clone() })
                    .collect();
                println!(
                    "{}: {}. See `asqu try list`.",
                    if open.len() == 1 { "1 open Try".to_string() } else { format!("{} open Tries", open.len()) },
                    printable(&names.join(", "))
                );
            }
            for line in asqu::backup::status_lines(store.root()) {
                println!("{line}");
            }
        }
        Command::Save { why, agent } => {
            if let Some(name) = store.try_for_path(&std::env::current_dir()?) {
                return try_command(&mut store, TryCmd::Save { name: Some(name), why });
            }
            let why = Why { request: why, actor: actor(), agent, ..Default::default() };
            let saved = store.save(&why)?;
            tell_skipped(&store);
            store.clear_intent(); // a manual save marks the current task as finished
            match saved {
                None => println!("Nothing changed since the last save."),
                Some(r) => println!(
                    "Saved #{} ({}): {} added, {} changed, {} removed.",
                    r.seq,
                    hash::short(&r.hash),
                    r.changes.added.len(),
                    r.changes.modified.len(),
                    r.changes.deleted.len()
                ),
            }
        }
        Command::Log { limit } => {
            let marks = store.marks()?;
            let entries = store.log(limit)?;
            if entries.is_empty() {
                println!("No history yet. Run `asqu save` to make the first entry.");
            }
            for e in entries {
                let when = when(e.ts);
                let by = match &e.agent {
                    Some(a) => format!("{} via {a}", e.actor),
                    None => e.actor.clone(),
                };
                // The request if someone said why; Asqu's own description either way.
                let what = match (e.request.trim(), e.summary.as_deref()) {
                    ("", Some(s)) => s.to_string(),
                    (r, Some(s)) => format!("\"{r}\" — {s}"),
                    (r, None) => format!("\"{r}\""),
                };
                let what = printable(&what);
                let flag = marks.get(&e.seq).map(|m| format!("  ★ {}", m.join(", "))).unwrap_or_default();
                println!("#{:<4} {}  {}  {what}  ({by}){flag}", e.seq, hash::short(&e.hash), when);
            }
        }
        Command::Check { clean, trust_check, save } => check_command(&mut store, clean, trust_check, save)?,
        Command::Release { version, dry_run } => release_command(&mut store, &version, dry_run)?,
        Command::Receipt { save } => match asqu::receipt::current(&project_root)? {
            Some(r) if save => {
                asqu::receipt::write_beside_files(&project_root, &r)?;
                println!("Wrote {}.", asqu::receipt::FILE);
            }
            Some(r) => print!("{}", r.to_json()),
            None => {
                println!("There's no check receipt for the files as they are now. Make one with `asqu check`.");
                std::process::exit(1);
            }
        },
        Command::Verify { .. } => {
            let problems = store.verify()?;
            if problems.is_empty() {
                println!("History is intact.");
            } else {
                for p in &problems {
                    println!("  problem: {p}");
                }
                std::process::exit(1);
            }
        }
    }
    // Anything this command changed (Tries, milestones…) is backed up in the background.
    asqu::backup::nudge(&project_root);
    Ok(())
}
