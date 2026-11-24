//! The few things that differ between Mac, Linux and Windows, in one place.

use std::path::PathBuf;
use std::process::Command;

/// The person's home folder.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))
        .map(PathBuf::from)
}

/// The person's login name, for "who made this change".
pub fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

/// The computer's name as people know it, e.g. "Scott's MacBook" (best effort).
pub fn computer_name() -> String {
    let from = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let name = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else if cfg!(target_os = "macos") {
        from("scutil", &["--get", "ComputerName"]).or_else(|| from("hostname", &[]))
    } else {
        from("hostname", &[])
    };
    name.filter(|n| !n.is_empty()).unwrap_or_else(|| "this computer".into())
}

/// Opens a web address in the person's browser (best effort, quietly).
pub fn open_url(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        // Not `cmd /c start`, which would read `&` and `^` in the address as commands.
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // On Linux the opener can stay running for as long as the browser it started does:
    // never wait for it there.
    if cfg!(any(target_os = "macos", windows)) {
        let _ = cmd.status();
    } else {
        let _ = cmd.stdin(std::process::Stdio::null()).spawn();
    }
}

/// Lets a background process carry on on its own: not stopped by Ctrl+C in the
/// terminal, and on Windows without flashing a window.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// A command line run by the system's shell (`sh -c` or `cmd /C`), e.g. a project's check.
pub fn shell(command_line: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        // Hand the line over exactly as written. The usual way of passing it would put a
        // backslash before every quote mark, which the command prompt doesn't understand,
        // so a command with a quoted path would fail. `/S` with one pair of outer quotes
        // makes it take everything between them as it is.
        c.arg("/S").arg("/C");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("\"{command_line}\""));
        }
        #[cfg(not(windows))]
        c.arg(command_line);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command_line);
        c
    }
}

/// Where to send output nobody needs.
pub fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// The program's file name: `asqu` or `asqu.exe`.
pub fn exe_name() -> &'static str {
    if cfg!(windows) {
        "asqu.exe"
    } else {
        "asqu"
    }
}

/// Asks the person at the keyboard to confirm something only they may decide (approving a
/// risky change, trusting a check command, sending the history to a backup place).
///
/// An AI or other program running `asqu` isn't at a real terminal, so it's refused: it
/// has to ask the person, who confirms here or in the Asqu view.
///
/// The limit: this stops AI tools approving things in normal use (by accident, or told to
/// by text in a project) and other users of the computer. A program running as this
/// person that sets out to cheat (faking a terminal, reading Asqu's files) can't be
/// stopped by anything running under the same login; only a separate user or sandbox can.
pub fn person_confirms(question: &str) -> anyhow::Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    // Development and test builds can stand in for the person; release builds never.
    if cfg!(debug_assertions) && std::env::var("ASQU_TEST_PERSON").is_ok_and(|v| v == "1") {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "only the person can do this, so it has to be run by you in a terminal \
             (or done in the Asqu view: asqu open). An AI or script can't confirm it."
        );
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// A path as other programs expect it. Windows' long form (`\\?\C:\...`) is exact, but
/// the command prompt and many tools can't use it, so it's shortened to `C:\...` when
/// handing a path to a project's own commands. Other paths are left as they are.
pub fn plain_path(path: &std::path::Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}
