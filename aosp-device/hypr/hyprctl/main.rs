//! hyprctl for the Hypr-Android shell (Phase E12, docs/hypr-ipc.md).
//!
//! PcSystemUI serves Hyprland's two IPC sockets in the abstract namespace:
//! `@hypr/.socket.sock` takes one request per connection (`[flags]/command args`, or
//! `[[BATCH]]cmd;cmd`) and answers as Hyprland's hyprctl prints it; `@hypr/.socket2.sock`
//! streams `EVENT>>DATA` lines. This client builds the request from its arguments the way
//! Hyprland's hyprctl does, and adds what needs the client side: `events` (the event stream on
//! stdout), `rollinglog` (logcat of the shell's tags) and the interactive `repl`.

use std::env;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::net::Shutdown;
#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
#[cfg(not(target_os = "android"))]
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::process::{exit, Command};

const REQUEST_SOCKET: &str = "hypr/.socket.sock";
const EVENT_SOCKET: &str = "hypr/.socket2.sock";
const LOG_TAGS: &[&str] = &["HyprIpc", "HyprShell", "HyprTile", "HyprFocus", "HyprLua", "HyprKeybinds"];

const USAGE: &str = "usage: hyprctl [(opt)flags] [command] [(opt)args]

commands:
    activewindow        Gets the active window name and its properties
    activeworkspace     Gets the active workspace and its properties
    animations          Gets the current config's info about animations and beziers
    binds               Lists all registered binds
    clients             Lists all windows with their properties
    configerrors        Lists all current config parsing errors
    cursorpos           Gets the current cursor position in global layout coordinates
    decorations <window> Lists all decorations and their info
    descriptions        Returns a JSON with all config options, their descriptions and types
    devices             Lists all connected keyboards and mice
    dismissnotify [n]   Dismisses all or up to n notifications
    dispatch <dsp> [args] Issue a dispatcher (or a Lua `hl.dsp...` expression)
    eval <lua>          Runs Lua in the configuration's state
    events              Prints the event socket's EVENT>>DATA lines as they come
    getoption <option>  Gets the config option status (values)
    getprop <window> <prop> Gets a window property (alpha, alphainactive, pinned, ...)
    globalshortcuts     Lists all global shortcuts
    instances           Lists all running instances with their info
    keyword <name> <value> Issue a keyword to set a config option
    kill                Issue a kill to get into a kill mode: click a window to kill it
    layers              Lists all the surface layers
    layouts             Lists all layouts available
    locked              Prints whether the session is locked
    monitors [all]      Lists active outputs with their properties
    notify <icon> <ms> <color> <text> Sends a notification
    output create|remove Adds or removes an overlay display
    reload              Issue a reload to force reload the config
    repl [lua]          Lua REPL, or the result of one expression
    rollinglog [-f]     Prints the tail of the shell's log (-f follows)
    setcursor           (not available on Android)
    seterror <color> <message...> Sets the error string (or `disable`)
    setprop <window> <prop> <value> Sets a window property
    splash              Prints the current random splash
    submap              Prints the current submap
    switchxkblayout <device> next|prev Switches the keyboard layout
    version             Prints the version, flags, commit and branch of the build
    workspacerules      Gets the list of defined workspace rules
    workspaces          Lists all workspaces with their properties

flags:
    -j              Output in JSON
    -r              Refresh state after issuing the command (accepted, always the case)
    --batch         Execute a batch of commands, separated by ';'
    --instance (-i) Use a specific instance (there is one: `android`)
    --quiet (-q)    Disable output of `ok` replies
";

fn connect(name: &str) -> io::Result<UnixStream> {
    let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
    UnixStream::connect_addr(&addr)
}

fn connect_or_exit(name: &str) -> UnixStream {
    connect(name).unwrap_or_else(|e| {
        eprintln!("Couldn't connect to @{name} ({e}): is PcSystemUI running, and may this user use it?");
        exit(3)
    })
}

/// One request on the request socket; the whole answer.
fn request(text: &str) -> String {
    let mut s = connect_or_exit(REQUEST_SOCKET);
    if let Err(e) = s.write_all(text.as_bytes()).and_then(|_| s.shutdown(Shutdown::Write)) {
        eprintln!("Couldn't write the request: {e}");
        exit(4);
    }
    let mut reply = Vec::new();
    if let Err(e) = s.read_to_end(&mut reply) {
        eprintln!("Couldn't read the reply: {e}");
        exit(5);
    }
    String::from_utf8_lossy(&reply).into_owned()
}

fn is_error(reply: &str) -> bool {
    let r = reply.trim_start();
    ["error", "unknown request", "Invalid", "permission denied", "Window not found", "prop not found", "no such option"]
        .iter()
        .any(|p| r.starts_with(p))
}

fn print_reply(reply: &str, quiet: bool) {
    if quiet && reply.trim() == "ok" {
        return;
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(reply.as_bytes());
    if !reply.is_empty() && !reply.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
}

/// `events`: the event socket on stdout, line by line, until it closes.
fn events() -> ! {
    let s = connect_or_exit(EVENT_SOCKET);
    let reader = io::BufReader::new(s);
    let mut out = io::stdout().lock();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line == "permission denied" {
            eprintln!("hyprctl: the event socket refused this user (ipc.allow)");
            exit(6);
        }
        if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    exit(0)
}

/// `rollinglog [-f]`: the shell's tags from logcat (Android's log, not a file of its own).
fn rollinglog(follow: bool) -> ! {
    let mut cmd = Command::new("/system/bin/logcat");
    cmd.arg("-v").arg("time");
    if !follow {
        cmd.arg("-d").arg("-t").arg("200");
    }
    cmd.arg("-s").args(LOG_TAGS.iter().map(|t| format!("{t}:V")));
    match cmd.status() {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("hyprctl: cannot run logcat: {e}");
            exit(1)
        }
    }
}

/// `repl` without code: one `repl <line>` request per line of stdin.
fn repl() -> ! {
    let stdin = io::stdin();
    let tty = stdin.is_terminal();
    loop {
        if tty {
            eprint!("> ");
        }
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let code = line.trim();
        if code.is_empty() {
            continue;
        }
        print_reply(&request(&format!("/repl {code}")), false);
    }
    if tty {
        eprintln!();
    }
    exit(0)
}

/// Adds the global flags to each command of a batch that has none, `;` escapes kept.
fn batch(cmds: &str, flags: &str) -> String {
    if flags.is_empty() {
        return format!("[[BATCH]]{cmds}");
    }
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = cmds.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push(c);
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == ';' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    let with_flags: Vec<String> = parts
        .iter()
        .map(|p| {
            let t = p.trim();
            let first = t.split(' ').next().unwrap_or("");
            if t.is_empty() || first.contains('/') { t.to_string() } else { format!("{flags}/{t}") }
        })
        .collect();
    format!("[[BATCH]]{}", with_flags.join(";"))
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut flags = String::new();
    let mut quiet = false;
    let mut batch_cmds: Option<String> = None;
    // Flags come before the command, as in Hyprland's hyprctl.
    while let Some(a) = args.first().cloned() {
        match a.as_str() {
            "-j" => flags.push('j'),
            "-r" => flags.push('r'),
            "-a" => flags.push('a'),
            "-q" | "--quiet" => quiet = true,
            "-i" | "--instance" => {
                args.remove(0);
                let inst = args.first().cloned().unwrap_or_default();
                if !(inst == "0" || inst == "android") {
                    eprintln!("hyprctl: no instance {inst} (there is one: android)");
                    exit(1);
                }
            }
            "--batch" => {
                args.remove(0);
                batch_cmds = Some(args.join(" "));
                args.clear();
                break;
            }
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                exit(0);
            }
            _ if a.starts_with("--") || (a.starts_with('-') && a.len() == 2 && !a[1..].chars().all(|c| c.is_ascii_digit())) => {
                eprintln!("hyprctl: unknown flag {a}\n\n{USAGE}");
                exit(1);
            }
            _ => break,
        }
        args.remove(0);
    }
    if let Some(cmds) = batch_cmds {
        let reply = request(&batch(&cmds, &flags));
        print_reply(&reply, quiet);
        exit(if is_error(&reply) { 1 } else { 0 });
    }
    let Some(command) = args.first().cloned() else {
        print!("{USAGE}");
        exit(1)
    };
    let rest = &args[1..];
    match command.as_str() {
        "events" => events(),
        "rollinglog" => rollinglog(rest.iter().any(|a| a == "-f" || a == "--follow")),
        "repl" if rest.is_empty() => repl(),
        _ => {}
    }
    let mut text = format!("{flags}/{command}");
    if !rest.is_empty() {
        text.push(' ');
        text.push_str(&rest.join(" "));
    }
    let reply = request(&text);
    print_reply(&reply, quiet);
    exit(if is_error(&reply) { 1 } else { 0 });
}

#[cfg(test)]
mod tests {
    use super::batch;

    #[test]
    fn batch_flags() {
        assert_eq!(batch("clients;dispatch workspace 1", ""), "[[BATCH]]clients;dispatch workspace 1");
        assert_eq!(batch("clients ; r/monitors", "j"), "[[BATCH]]j/clients;r/monitors");
        assert_eq!(batch("eval a=1\\;b=2;clients", "j"), "[[BATCH]]j/eval a=1\\;b=2;j/clients");
    }
}
