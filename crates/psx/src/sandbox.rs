//! port run's sandbox: each role's session under bubblewrap, so its limits
//! hold whatever harness runs it.
//!
//! The implementer sees only what it needs: the system's folders, its own
//! repository (its notes/, specs/, access rules, .mcp.json and git's
//! config and hooks read-only), the Rust toolchain, the harness's own
//! program, git's identity, a home of its own (its harness's settings and
//! transcripts stay there) and what protocol.toml shares with it. The other
//! repositories, the disc, psx's own folder and the person's home are not
//! there at all, and psx is masked wherever the sandbox has it.
//!
//! The investigator and the reviewer see everything, and write their own
//! repository and the person's home (their harness's login and
//! transcripts) but never the port's other repositories. Their access
//! rules, .mcp.json, protocol.toml (whose commands port run runs outside)
//! and git's config and hooks are read-only.
//!
//! psx and clean mode need what the sandbox keeps out (publish cross writes
//! to the implementer; clean mode runs the original from the investigator's
//! repository), so their MCP servers run outside, one per connection, on a
//! socket the sandbox has at /tmp/psx-mcp/: psx mcp and psx-clean mcp pass
//! their stdio through to it when it is there, whatever harness started
//! them. The network stays: the harness needs its model's service.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::Error;
use crate::roles::Role;

/// Where the servers' sockets are inside the sandbox.
const INSIDE: &str = "/tmp/psx-mcp";

/// Whether bubblewrap runs here.
pub(crate) fn available() -> bool {
    cfg!(target_os = "linux")
        && Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute())
}

/// A program as the shell finds it.
fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        return Some(PathBuf::from(cmd));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(cmd)).find(|p| p.is_file())
}

/// What a harness's program needs: the links on the way to it (a
/// `current` link to a release, say), each as (link, where it points), and
/// its own folder or the package it belongs to (a Node program's
/// package.json).
fn program_needs(cmd: &str) -> (Vec<(PathBuf, PathBuf)>, Vec<PathBuf>) {
    let mut links = Vec::new();
    let Some(mut p) = which(cmd) else { return (links, vec![]) };
    for _ in 0..16 {
        // Each folder on the way that is itself a link.
        for a in p.ancestors().skip(1) {
            if let Ok(to) = std::fs::read_link(a) {
                if !links.iter().any(|l: &(PathBuf, PathBuf)| l.0 == a) {
                    links.push((a.to_path_buf(), to));
                }
            }
        }
        match std::fs::read_link(&p) {
            Ok(to) => p = if to.is_absolute() { to } else { p.parent().unwrap_or(Path::new("/")).join(to) },
            Err(_) => break,
        }
    }
    let Ok(real) = p.canonicalize() else { return (links, vec![]) };
    let mut dirs: Vec<PathBuf> = real.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(pkg) = real.ancestors().skip(1).take(6).find(|a| a.join("package.json").is_file()) {
        dirs.push(pkg.to_path_buf());
    }
    (links, dirs)
}

/// One session's mounts, as bubblewrap's arguments.
struct Mounts {
    args: Vec<OsString>,
    /// What the sandbox has, to mask psx only where it is.
    seen: Vec<PathBuf>,
}

impl Mounts {
    fn push(&mut self, a: &[&std::ffi::OsStr]) {
        self.args.extend(a.iter().map(|s| s.to_os_string()));
    }
    fn ro(&mut self, p: &Path) {
        if p.exists() {
            self.push(&["--ro-bind".as_ref(), p.as_os_str(), p.as_os_str()]);
            self.seen.push(p.to_path_buf());
        }
    }
    fn rw(&mut self, p: &Path) {
        if p.exists() {
            self.push(&["--bind".as_ref(), p.as_os_str(), p.as_os_str()]);
            self.seen.push(p.to_path_buf());
        }
    }
    /// A system folder, or the link a merged /usr makes of it.
    fn system(&mut self, p: &Path) {
        match std::fs::read_link(p) {
            Ok(to) => self.push(&["--symlink".as_ref(), to.as_os_str(), p.as_os_str()]),
            Err(_) => self.ro(p),
        }
    }
    fn has(&self, p: &Path) -> bool {
        self.seen.iter().any(|s| p.starts_with(s))
    }
}

/// What a role's session may see and write.
pub(crate) struct Policy<'a> {
    pub role: Role,
    /// Its own repository.
    pub repo: &'a Path,
    /// The port's other repositories, read-only for the investigator and
    /// the reviewer.
    pub others: &'a [PathBuf],
    /// Shared from the person's home, writable and read-only.
    pub share: &'a [PathBuf],
    pub share_ro: &'a [PathBuf],
    /// The implementer's own home folder.
    pub home: Option<&'a Path>,
    /// The role's harness program ([agents.ROLE] command), there besides
    /// what runs: port run --shell runs a shell, to start the harness from.
    pub harness: Option<&'a str>,
    /// The person at the terminal (port run --shell): the session keeps it,
    /// which a headless one may not (it could type into it).
    pub interactive: bool,
}

/// Files in a role's repository it may not change: its access rules and
/// servers, protocol.toml (port run runs its commands outside), git's
/// config and hooks (run outside by the person's git), and the
/// implementer's notes and specs (psx's).
fn read_only(role: Role) -> &'static [&'static str] {
    match role {
        Role::Implementer => &[".claude", ".mcp.json", ".git/config", ".git/hooks", "notes", "specs"],
        _ => &[".claude", ".mcp.json", ".git/config", ".git/hooks", "protocol.toml"],
    }
}

/// bubblewrap's command line around `cmd`.
pub(crate) fn wrap(p: &Policy, cmd: &[String], servers: &Servers) -> crate::Result<Vec<OsString>> {
    for s in p.share.iter().chain(p.share_ro) {
        if !s.exists() {
            return Err(Error::invalid(format!(
                "protocol.toml's [agents.{}] shares {}, which is not there",
                p.role.name(),
                s.display()
            )));
        }
    }
    let mut m = Mounts { args: vec!["bwrap".into()], seen: vec![] };
    let home_dir = home();
    if p.role == Role::Implementer {
        for d in ["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/sys"] {
            m.system(Path::new(d));
        }
        // A resolver kept under /run.
        if let Some(d) = Path::new("/etc/resolv.conf").canonicalize().ok().filter(|r| r.starts_with("/run")) {
            m.ro(d.parent().unwrap_or(&d));
        }
        m.push(&[
            "--dev".as_ref(),
            "/dev".as_ref(),
            "--proc".as_ref(),
            "/proc".as_ref(),
            "--tmpfs".as_ref(),
            "/tmp".as_ref(),
        ]);
        if let (Some(h), Some(own)) = (&home_dir, p.home) {
            std::fs::create_dir_all(own).map_err(|e| Error::io(own, e))?;
            m.push(&["--bind".as_ref(), own.as_os_str(), h.as_os_str()]);
        }
        // The toolchain: read-only, but for cargo's downloads.
        let cargo =
            std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| home_dir.as_ref().map(|h| h.join(".cargo")));
        if let Some(c) = cargo {
            m.ro(&c);
            m.rw(&c.join("registry"));
            m.rw(&c.join("git"));
        }
        let rustup =
            std::env::var_os("RUSTUP_HOME").map(PathBuf::from).or_else(|| home_dir.as_ref().map(|h| h.join(".rustup")));
        if let Some(r) = rustup {
            m.ro(&r);
        }
        // Programs on the PATH from the home folder, and the harness's own.
        if let Some(path) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&path) {
                if home_dir.as_ref().is_some_and(|h| d.starts_with(h)) && !m.has(&d) {
                    m.ro(&d);
                }
            }
        }
        let (mut links, mut dirs) = program_needs(&cmd[0]);
        if let Some(h) = p.harness.filter(|h| *h != cmd[0]) {
            let (l, d) = program_needs(h);
            links.extend(l);
            dirs.extend(d);
        }
        for d in dirs {
            if !m.has(&d) {
                m.ro(&d);
            }
        }
        // Links in folders the sandbox does not have (system links are there).
        for (link, to) in links {
            if home_dir.as_ref().is_some_and(|h| link.starts_with(h)) && !m.has(&link) {
                m.push(&["--symlink".as_ref(), to.as_os_str(), link.as_os_str()]);
            }
        }
        for e in &servers.programs {
            if !m.has(e) {
                m.ro(e);
            }
        }
        if let Some(h) = &home_dir {
            m.ro(&h.join(".gitconfig"));
            m.ro(&h.join(".config/git"));
        }
    } else {
        m.push(&["--ro-bind".as_ref(), "/".as_ref(), "/".as_ref()]);
        m.seen.push(PathBuf::from("/"));
        m.push(&[
            "--dev".as_ref(),
            "/dev".as_ref(),
            "--proc".as_ref(),
            "/proc".as_ref(),
            "--tmpfs".as_ref(),
            "/tmp".as_ref(),
        ]);
        // The home folder as the person has it (the harness's login and
        // transcripts), and what the fresh /tmp would hide.
        if let Some(h) = &home_dir {
            m.rw(h);
        }
        let harness = p.harness.map(|h| program_needs(h).1).unwrap_or_default();
        for o in program_needs(&cmd[0]).1.into_iter().chain(harness).chain(servers.programs.iter().cloned()) {
            if o.starts_with("/tmp") {
                m.ro(&o);
            }
        }
        // The port's other repositories, read-only wherever they are, and
        // psx's own programs, which port run runs outside.
        for o in p.others {
            m.ro(o);
        }
        if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            // Built from source (target/debug/psx): the source too.
            let source =
                d.ancestors().skip(1).take(3).find(|a| a.join("Cargo.toml").is_file() && a.join("crates/psx").is_dir());
            m.ro(source.unwrap_or(&d));
        }
    }
    for s in p.share_ro {
        m.ro(s);
    }
    for s in p.share {
        m.rw(s);
    }
    m.rw(p.repo);
    for r in read_only(p.role) {
        m.ro(&p.repo.join(r));
    }
    for (name, sock) in &servers.sockets {
        let inside = Path::new(INSIDE).join(format!("{name}.sock"));
        m.push(&["--bind".as_ref(), sock.as_os_str(), inside.as_os_str()]);
    }
    // psx itself, wherever the implementer's sandbox has it.
    if p.role == Role::Implementer {
        let mut psx: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).map(|d| d.join("psx")).collect())
            .unwrap_or_default();
        psx.extend(std::env::current_exe().ok());
        // A link (~/.local/bin/psx to a build) cannot be mounted on: mask
        // what it names, and a link to what the sandbox lacks names nothing.
        let mut psx: Vec<PathBuf> = psx.iter().filter_map(|x| x.canonicalize().ok()).collect();
        psx.sort();
        psx.dedup();
        let masked: Vec<PathBuf> = psx.into_iter().filter(|x| x.is_file() && m.has(x)).collect();
        for x in masked {
            m.push(&["--ro-bind".as_ref(), "/dev/null".as_ref(), x.as_os_str()]);
        }
    }
    m.push(&["--setenv".as_ref(), "TMPDIR".as_ref(), "/tmp".as_ref()]);
    m.push(&[
        "--unshare-pid".as_ref(),
        "--unshare-ipc".as_ref(),
        "--unshare-uts".as_ref(),
        "--die-with-parent".as_ref(),
    ]);
    if !p.interactive {
        m.push(&["--new-session".as_ref()]);
    }
    m.push(&["--chdir".as_ref(), p.repo.as_os_str(), "--".as_ref()]);
    m.args.extend(cmd.iter().map(OsString::from));
    Ok(m.args)
}

/// Copies until the reader ends, as each read comes: io::copy may splice
/// between a pipe and a socket and hold what a server answers.
fn pump(r: &mut impl std::io::Read, w: &mut impl Write) {
    let mut buf = [0u8; 64 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if w.write_all(&buf[..n]).and_then(|_| w.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

/// psx's and clean mode's MCP servers for one session, outside its
/// sandbox: each server in the role's .mcp.json whose program is psx or
/// psx-clean, started as .mcp.json says for each connection to its socket.
pub(crate) struct Servers {
    pub sockets: Vec<(String, PathBuf)>,
    /// The servers' programs, which the sandbox needs to pass stdio through.
    pub programs: Vec<PathBuf>,
    dir: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<Child>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Servers {
    pub(crate) fn none() -> Servers {
        Servers {
            sockets: vec![],
            programs: vec![],
            dir: None,
            stop: Arc::new(AtomicBool::new(true)),
            children: Arc::new(Mutex::new(vec![])),
            threads: vec![],
        }
    }

    /// Starts listening for the role's servers; their errors go to `log`.
    pub(crate) fn start(repo: &Path, log: &Path) -> crate::Result<Servers> {
        let mut s = Servers::none();
        s.stop.store(false, Ordering::Relaxed);
        let mcp: Value = std::fs::read(repo.join(".mcp.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let Some(list) = mcp["mcpServers"].as_object() else { return Ok(s) };
        // A short folder: a socket's path is limited to about 100 bytes.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir =
            std::env::temp_dir().join(format!("psx-port-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        for def in list.values() {
            let Some(program) = def["command"].as_str() else { continue };
            let inside = match Path::new(program).file_stem().and_then(|s| s.to_str()) {
                Some("psx") => "psx",
                Some("psx-clean") => "clean",
                _ => continue,
            };
            if s.sockets.iter().any(|x| x.0 == inside) {
                continue;
            }
            std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
            let sock = dir.join(format!("{inside}.sock"));
            let listener = UnixListener::bind(&sock).map_err(|e| Error::io(&sock, e))?;
            listener.set_nonblocking(true).map_err(|e| Error::io(&sock, e))?;
            let args: Vec<String> =
                def["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect();
            let env: Vec<(String, String)> = def["env"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect();
            let found = which(program).and_then(|p| p.canonicalize().ok()).unwrap_or_else(|| PathBuf::from(program));
            let (program, repo, log) = (program.to_string(), repo.to_path_buf(), log.to_path_buf());
            let (stop, children) = (s.stop.clone(), s.children.clone());
            s.threads.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let conn = match listener.accept() {
                        Ok((c, _)) => c,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        Err(_) => return,
                    };
                    let _ = conn.set_nonblocking(false);
                    let errors = std::fs::OpenOptions::new().create(true).append(true).open(&log);
                    let child = Command::new(&program)
                        .args(&args)
                        .envs(env.iter().map(|(k, v)| (k, v)))
                        .current_dir(&repo)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(errors.map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
                        .spawn();
                    let Ok(mut child) = child else {
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                            let _ = writeln!(f, "port run: could not start {program}");
                        }
                        continue;
                    };
                    let (Some(mut stdin), Some(mut stdout), Ok(mut into), Ok(mut back)) =
                        (child.stdin.take(), child.stdout.take(), conn.try_clone(), conn.try_clone())
                    else {
                        let _ = child.kill();
                        continue;
                    };
                    std::thread::spawn(move || {
                        pump(&mut into, &mut stdin);
                    });
                    std::thread::spawn(move || {
                        pump(&mut stdout, &mut back);
                        let _ = back.shutdown(std::net::Shutdown::Both);
                    });
                    if let Ok(mut c) = children.lock() {
                        c.push(child);
                    }
                }
            }));
            s.sockets.push((inside.to_string(), sock));
            s.programs.push(found);
        }
        s.dir = Some(dir);
        Ok(s)
    }
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Ok(mut c) = self.children.lock() {
            for mut child in c.drain(..) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Some(d) = &self.dir {
            for (_, s) in &self.sockets {
                let _ = std::fs::remove_file(s);
            }
            let _ = std::fs::remove_dir(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// Each mount as (kind, source, destination).
    fn mounts(argv: &[OsString]) -> Vec<(String, String, String)> {
        let a: Vec<String> = argv.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        let mut out = vec![];
        let mut i = 0;
        while i < a.len() {
            if matches!(a[i].as_str(), "--ro-bind" | "--bind" | "--symlink") {
                out.push((a[i].clone(), a[i + 1].clone(), a[i + 2].clone()));
                i += 3;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn each_role_sees_what_its_policy_says() {
        let d = std::env::temp_dir().join(format!("psx-policy-{}", std::process::id()));
        let (inv, imp, rev) = (d.join("inv"), d.join("clean"), d.join("review"));
        for r in [&inv, &imp, &rev] {
            for sub in ["notes", "specs", ".claude", ".git/hooks"] {
                std::fs::create_dir_all(r.join(sub)).unwrap();
            }
            std::fs::write(r.join(".git/config"), "").unwrap();
            std::fs::write(r.join("protocol.toml"), "").unwrap();
        }
        let none = Servers::none();
        let s = |p: &Path| p.display().to_string();
        let cmd = vec!["true".to_string()];

        let others = [inv.clone(), rev.clone()];
        let home = d.join("home");
        let policy = Policy {
            role: Role::Implementer,
            repo: &imp,
            others: &others,
            share: &[],
            share_ro: &[],
            home: Some(&home),
            harness: None,
            interactive: false,
        };
        let argv = wrap(&policy, &cmd, &none).unwrap();
        let m = mounts(&argv);
        let text = argv.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
        assert!(!text.contains(&s(&inv)) && !text.contains(&s(&rev)), "{text}");
        assert!(m.contains(&("--bind".into(), s(&imp), s(&imp))));
        for ro in ["notes", "specs", ".claude", ".git/config", ".git/hooks"] {
            assert!(m.contains(&("--ro-bind".into(), s(&imp.join(ro)), s(&imp.join(ro)))), "{ro}");
        }
        assert!(text.contains("--new-session"));
        // Its home is its own.
        assert!(m.iter().any(|x| x.0 == "--bind" && x.1 == s(&home)));

        let others = [imp.clone(), rev.clone()];
        let policy = Policy {
            role: Role::Investigator,
            repo: &inv,
            others: &others,
            share: &[],
            share_ro: &[],
            home: None,
            harness: None,
            interactive: true,
        };
        let m = mounts(&wrap(&policy, &cmd, &none).unwrap());
        let at = |x: &(String, String, String)| m.iter().position(|y| y == x).unwrap();
        // The others read-only after the own repository's folder is writable, and after the home.
        for o in [&imp, &rev] {
            at(&("--ro-bind".into(), s(o), s(o)));
        }
        let own = at(&("--bind".into(), s(&inv), s(&inv)));
        assert!(at(&("--ro-bind".into(), s(&inv.join("protocol.toml")), s(&inv.join("protocol.toml")))) > own);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_server_outside_answers_through_its_socket() {
        let d = std::env::temp_dir().join(format!("psx-servers-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // A server that answers each line with itself, under psx's name.
        let psx = d.join("psx");
        std::os::unix::fs::symlink("/bin/cat", &psx).unwrap();
        let mcp = serde_json::json!({"mcpServers": {"psx": {"command": psx, "args": []}}});
        std::fs::write(d.join(".mcp.json"), mcp.to_string()).unwrap();
        let servers = Servers::start(&d, &d.join("mcp.log")).unwrap();
        assert_eq!(servers.sockets.len(), 1);
        let s = std::os::unix::net::UnixStream::connect(&servers.sockets[0].1).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut lines = BufReader::new(s.try_clone().unwrap()).lines();
        let mut w = s;
        // A client that waits before it speaks, and speaks twice.
        std::thread::sleep(Duration::from_millis(300));
        writeln!(w, "one").unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "one");
        std::thread::sleep(Duration::from_millis(300));
        writeln!(w, "two").unwrap();
        assert_eq!(lines.next().unwrap().unwrap(), "two");
        drop(servers);
        let _ = std::fs::remove_dir_all(&d);
    }
}
