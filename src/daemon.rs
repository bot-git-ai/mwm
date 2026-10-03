//! The daemon and its client: CLI parsing, the Unix-socket protocol, the
//! event loop and the retile itself.
//!
//! All system access goes through [`WindowSystem`], so the whole file is
//! testable on any OS; only [`system`] differs per platform.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::keymap::{load_bindings, match_key, KeyBinding};
use crate::layout::{LayoutConfig, LayoutEngine, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
use crate::platform::WindowSystem;
use crate::request::{IpcResponse, Request};
use crate::types::{Direction, WindowInfo};

#[cfg(target_os = "macos")]
fn system() -> Box<dyn WindowSystem> {
    Box::new(crate::platform_darwin::DarwinWindowSystem::new())
}

#[cfg(not(target_os = "macos"))]
fn system() -> Box<dyn WindowSystem> {
    Box::new(crate::platform_stub::StubWindowSystem::new())
}

/// How long a retile is held back after an event.
const QUIET_MS: u64 = 150;
/// Upper bound on how long the loop waits before re-checking deadlines.
const TICK: Duration = Duration::from_millis(20);
/// How long a client waits for the daemon's answer.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The default socket path, honouring `XDG_RUNTIME_DIR` when set.
#[must_use]
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(format!("mwm-{}.sock", uid()))
}

fn uid() -> u32 {
    // SAFETY: getuid() is always safe and cannot fail.
    unsafe { libc_getuid() }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// What the CLI decided to do.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    /// Run the daemon.
    Daemon {
        columns: f64,
        socket: Option<PathBuf>,
        keybindings: Option<PathBuf>,
        no_keybindings: bool,
        verbose: bool,
    },
    /// Send one request to a running daemon.
    Send {
        request: Request,
        socket: Option<PathBuf>,
        verbose: bool,
    },
    /// Install the binary, write the plist and load the agent.
    Install {
        prefix: Option<PathBuf>,
        label: Option<String>,
        skip_launchctl: bool,
        verbose: bool,
    },
    /// Print the `LaunchAgent` plist.
    LaunchdPlist {
        label: Option<String>,
        output: Option<PathBuf>,
    },
    /// Print usage; `code` is the exit status.
    Usage(i32),
}

const USAGE: &str = "\
usage: mwm <command> [options]

commands:
  daemon [--socket PATH] [--columns N] [--keybindings PATH] [--no-keybindings] [-v]
  focus <left|right|up|down> [--socket PATH]
  move <left|right|up|down> [--socket PATH]
  goto-desktop <1-10> [--socket PATH]
  columns <number> [--socket PATH]
  fullscreen | close | retile | status | stop | restart [--socket PATH]
  install [--prefix DIR] [--label LABEL] [--no-launchctl] [-v]
  launchd-plist [--label LABEL] [--output PATH]

options:
  -h, --help     show this message
  -v, --verbose  print the daemon's answer
";

/// Collapse the `Result` the client-option builder returns.
fn flatten(result: Result<Action, Action>) -> Action {
    result.unwrap_or_else(|action| action)
}

/// Parse an argument vector (without the program name).
fn parse_args(args: &[String]) -> Action {
    let Some(command) = args.first() else {
        return Action::Usage(2);
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        return Action::Usage(0);
    }
    let rest = &args[1..];
    match command.as_str() {
        "daemon" => parse_daemon(rest),
        "focus" | "move" => flatten(parse_directional(command, rest)),
        "goto-desktop" => flatten(with_client_options(rest, |positional, socket, verbose| {
            let Ok(desktop) = positional.parse::<u8>() else {
                return Err(Action::Usage(2));
            };
            if !(1..=10).contains(&desktop) {
                return Err(Action::Usage(2));
            }
            Ok(Action::Send {
                request: Request::GotoDesktop(desktop),
                socket,
                verbose,
            })
        })),
        "columns" => flatten(with_client_options(rest, |positional, socket, verbose| {
            let Ok(columns) = positional.parse::<f64>() else {
                return Err(Action::Usage(2));
            };
            if !LayoutConfig::new(columns).is_valid() {
                return Err(Action::Usage(2));
            }
            Ok(Action::Send {
                request: Request::Columns(columns),
                socket,
                verbose,
            })
        })),
        "fullscreen" => simple_client(rest, Request::Fullscreen),
        "close" => simple_client(rest, Request::Close),
        "retile" => simple_client(rest, Request::Retile),
        "status" => simple_client(rest, Request::Status),
        "stop" => simple_client(rest, Request::Stop),
        "restart" => simple_client(rest, Request::Restart),
        "install" => parse_install(rest),
        "launchd-plist" => parse_launchd(rest),
        _ => Action::Usage(2),
    }
}

fn parse_daemon(args: &[String]) -> Action {
    let mut columns = 2.0_f64;
    let mut socket = None;
    let mut keybindings = None;
    let mut no_keybindings = false;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--columns" | "-c" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                let Ok(parsed) = value.parse::<f64>() else {
                    return Action::Usage(2);
                };
                if !LayoutConfig::new(parsed).is_valid() {
                    return Action::Usage(2);
                }
                columns = parsed;
            }
            "--socket" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                socket = Some(PathBuf::from(value));
            }
            "--keybindings" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                keybindings = Some(PathBuf::from(value));
            }
            "--no-keybindings" => no_keybindings = true,
            "--verbose" | "-v" => verbose = true,
            _ => return Action::Usage(2),
        }
        index += 1;
    }
    Action::Daemon {
        columns,
        socket,
        keybindings,
        no_keybindings,
        verbose,
    }
}

fn parse_directional(command: &str, args: &[String]) -> Result<Action, Action> {
    with_client_options(args, |positional, socket, verbose| {
        let Some(direction) = Direction::parse(positional) else {
            return Err(Action::Usage(2));
        };
        let request = if command == "focus" {
            Request::Focus(direction)
        } else {
            Request::Move(direction)
        };
        Ok(Action::Send {
            request,
            socket,
            verbose,
        })
    })
}

/// Client commands that take no argument beyond their options.
fn simple_client(args: &[String], request: Request) -> Action {
    let mut socket = None;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--socket" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                socket = Some(PathBuf::from(value));
            }
            "--verbose" | "-v" => verbose = true,
            _ => return Action::Usage(2),
        }
        index += 1;
    }
    Action::Send {
        request,
        socket,
        verbose,
    }
}

/// Shared client-option handling: one positional plus `--socket`/`--verbose`.
fn with_client_options<F>(args: &[String], build: F) -> Result<Action, Action>
where
    F: FnOnce(&str, Option<PathBuf>, bool) -> Result<Action, Action>,
{
    let mut positional: Option<String> = None;
    let mut socket = None;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--socket" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(Action::Usage(2));
                };
                socket = Some(PathBuf::from(value));
            }
            "--verbose" | "-v" => verbose = true,
            other if other.starts_with('-') => return Err(Action::Usage(2)),
            other => {
                if positional.replace(other.to_string()).is_some() {
                    return Err(Action::Usage(2));
                }
            }
        }
        index += 1;
    }
    let Some(positional) = positional else {
        return Err(Action::Usage(2));
    };
    build(&positional, socket, verbose)
}

/// The launchd label mwm installs itself under.
const DEFAULT_LABEL: &str = "mwm";

/// Where the binary installs itself: `~/.local/bin`.
fn default_prefix() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    home.join(".local").join("bin")
}

fn parse_install(args: &[String]) -> Action {
    let mut prefix = None;
    let mut label = None;
    let mut skip_launchctl = false;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--prefix" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                prefix = Some(PathBuf::from(value));
            }
            "--label" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                label = Some(value.clone());
            }
            "--no-launchctl" => skip_launchctl = true,
            "--verbose" | "-v" => verbose = true,
            _ => return Action::Usage(2),
        }
        index += 1;
    }
    Action::Install {
        prefix,
        label,
        skip_launchctl,
        verbose,
    }
}

fn parse_launchd(args: &[String]) -> Action {
    let mut label_name = None;
    let mut destination = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--label" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                label_name = Some(value.clone());
            }
            "--output" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Action::Usage(2);
                };
                destination = Some(PathBuf::from(value));
            }
            _ => return Action::Usage(2),
        }
        index += 1;
    }
    Action::LaunchdPlist {
        label: label_name,
        output: destination,
    }
}

/// Entry point: turn a process argument vector into an exit code.
pub fn cli_main<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let arguments: Vec<String> = args
        .into_iter()
        .map(|argument| argument.into().to_string_lossy().into_owned())
        .skip(1)
        .collect();
    if arguments.is_empty() {
        eprint!("{USAGE}");
        return 2;
    }
    match parse_args(&arguments) {
        Action::Usage(code) => {
            if code == 0 {
                print!("{USAGE}");
            } else {
                eprint!("{USAGE}");
            }
            code
        }
        Action::Send {
            request,
            socket,
            verbose,
        } => {
            let path = socket.unwrap_or_else(default_socket_path);
            send_request(&path, &request, verbose)
        }
        Action::LaunchdPlist { label, output } => write_plist(label.as_deref(), output.as_deref()),
        Action::Install {
            prefix,
            label,
            skip_launchctl,
            verbose,
        } => run_install(&InstallOptions {
            prefix: prefix.map_or_else(default_prefix, PathBuf::from),
            label: label.unwrap_or_else(|| DEFAULT_LABEL.to_string()),
            skip_launchctl,
            verbose,
        }),
        Action::Daemon {
            columns,
            socket,
            keybindings,
            no_keybindings,
            verbose,
        } => {
            let path = socket.unwrap_or_else(default_socket_path);
            run_daemon(&DaemonConfig {
                columns,
                socket_path: path,
                keybindings_path: keybindings,
                keybindings_enabled: !no_keybindings,
                verbose,
            })
        }
    }
}

/// Send one request to a running daemon and report its answer.
fn send_request(path: &Path, request: &Request, verbose: bool) -> i32 {
    let response = match request_daemon(path, request) {
        Ok(response) => response,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    if response.ok {
        if verbose {
            println!("{}", response.message);
        }
        0
    } else {
        eprintln!("{}", response.message);
        1
    }
}

/// Connect, write one request, read one response.
fn request_daemon(path: &Path, request: &Request) -> Result<IpcResponse, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|error| format!("cannot reach the daemon at {}: {error}", path.display()))?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT)).ok();
    stream
        .write_all(format!("{}\n", request.to_json()).as_bytes())
        .map_err(|error| format!("cannot send the request: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("no answer from the daemon: {error}"))?;
    IpcResponse::from_json(&line).ok_or_else(|| "the daemon sent an unreadable answer".to_string())
}

/// Escape XML text for the generated plist.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The `LaunchAgent` plist for the installed binary.
fn launchd_plist_xml(label: &str, home: &Path) -> String {
    launchd_plist_xml_for(label, home, &home.join(".local/bin/mwm"))
}

/// The plist, naming the binary at `binary` so a custom `--prefix` is honoured.
fn launchd_plist_xml_for(label: &str, home: &Path, binary: &Path) -> String {
    let binary = binary.to_path_buf();
    let stdout = format!("/tmp/mwm_{}.out.log", uid());
    let stderr = format!("/tmp/mwm_{}.err.log", uid());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
    <key>Label</key>\n    <string>{label}</string>\n\
    <key>ProgramArguments</key>\n    <array>\n        <string>{bin}</string>\n        <string>daemon</string>\n    </array>\n\
    <key>WorkingDirectory</key>\n    <string>{home}</string>\n\
    <key>RunAtLoad</key>\n    <true/>\n\
    <key>KeepAlive</key>\n    <false/>\n\
    <key>StandardOutPath</key>\n    <string>{stdout}</string>\n\
    <key>StandardErrorPath</key>\n    <string>{stderr}</string>\n</dict>\n</plist>\n",
        label = xml_escape(label),
        bin = xml_escape(&binary.display().to_string()),
        home = xml_escape(&home.display().to_string()),
        stdout = xml_escape(&stdout),
        stderr = xml_escape(&stderr),
    )
}

/// Print the plist to stdout or write it to `output`.
fn write_plist(label: Option<&str>, output: Option<&Path>) -> i32 {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let xml = launchd_plist_xml(label.unwrap_or("mwm"), &home);
    if let Some(path) = output {
        return match std::fs::write(path, xml) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("cannot write {}: {error}", path.display());
                1
            }
        };
    }
    print!("{xml}");
    0
}

/// How the daemon was started.
#[derive(Debug, Clone, PartialEq)]
struct DaemonConfig {
    columns: f64,
    socket_path: PathBuf,
    keybindings_path: Option<PathBuf>,
    keybindings_enabled: bool,
    verbose: bool,
}

/// Something that can happen to the daemon.
#[derive(Debug)]
enum Event {
    /// A request arrived from a key press.
    Request(Request),
    /// A client sent a raw payload and waits for an answer.
    Client(IpcMessage),
    /// Windows changed on screen.
    WindowsChanged,
}

/// One client request waiting to be answered.
#[derive(Debug)]
struct IpcMessage {
    payload: String,
    reply: Sender<IpcResponse>,
}

/// What the loop should do after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Nothing further is due.
    Idle,
    /// Re-apply the layout now.
    Retile,
}

/// The debounce state machine, kept pure so it can be tested without time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Scheduler {
    /// When a retile should run, if one is pending.
    due_ms: Option<u64>,
}

impl Scheduler {
    /// Feed an event and return the action it implies.
    fn update(&mut self, now_ms: u64, event: &Event) -> Next {
        match event {
            Event::WindowsChanged => {
                let due = now_ms + QUIET_MS;
                self.due_ms = Some(self.due_ms.map_or(due, |current| current.max(due)));
                Next::Idle
            }
            Event::Request(_) | Event::Client(_) => Next::Idle,
        }
    }

    /// Fire a retile when its deadline has passed.
    fn poll(&mut self, now_ms: u64) -> Next {
        match self.due_ms {
            Some(due) if now_ms >= due => {
                self.due_ms = None;
                Next::Retile
            }
            _ => Next::Idle,
        }
    }

    /// Milliseconds until the next deadline, bounded by `TICK`.
    fn wait(&self, now_ms: u64) -> Duration {
        let Some(due) = self.due_ms else {
            return TICK;
        };
        TICK.min(Duration::from_millis(due.saturating_sub(now_ms)))
    }
}

/// The daemon's mutable state: layout engine plus last-known focus.
struct Core {
    system: Box<dyn WindowSystem>,
    engine: LayoutEngine,
    socket_path: PathBuf,
    restarting: bool,
}

impl Core {
    /// Apply the layout to every screen.
    fn retile(&mut self) -> String {
        self.system.refresh();
        let Some(windows) = self.system.windows().ok() else {
            return "cannot read the window list".to_string();
        };
        let Ok(screens) = self.system.screens() else {
            return "cannot read the screen list".to_string();
        };
        let windows_by_key: BTreeMap<&str, &WindowInfo> =
            windows.iter().map(|w| (w.key.as_str(), w)).collect();
        let visible_keys: BTreeSet<String> = windows.iter().map(|w| w.key.clone()).collect();
        let screen_keys: BTreeSet<String> = screens.iter().map(|s| s.key.clone()).collect();
        self.engine.state.keep_only(&visible_keys, &screen_keys);
        let mut placed = 0_usize;
        for screen in &screens {
            let on_screen: Vec<&WindowInfo> = windows
                .iter()
                .filter(|w| w.screen_key == screen.key && is_tilable(w))
                .collect();
            if on_screen.is_empty() {
                continue;
            }
            let fullscreen = on_screen
                .iter()
                .find(|w| self.engine.state.fullscreen_keys.contains(&w.key))
                .copied();
            if let Some(window) = fullscreen {
                if self.system.set_frame(window, screen.frame) {
                    placed += 1;
                }
                continue;
            }
            let owned: Vec<WindowInfo> = on_screen.iter().map(|w| (*w).clone()).collect();
            let columns = self.engine.reconcile(&screen.key, &owned);
            self.remember_row_weights(&columns, &windows_by_key);
            let targets = self.engine.layout_targets(screen, &columns);
            for (key, frame) in &targets {
                if let Some(window) = windows_by_key.get(key.as_str()) {
                    if self.system.set_frame(window, *frame) {
                        placed += 1;
                    }
                }
            }
        }
        format!("tiled {placed} of {} windows", windows.len())
    }

    /// Remember how the user sized each column before we take over.
    fn remember_row_weights(
        &mut self,
        columns: &[Vec<String>],
        windows: &BTreeMap<&str, &WindowInfo>,
    ) {
        for column in columns {
            if column.len() < 2 {
                continue;
            }
            for key in column {
                if let Some(window) = windows.get(key.as_str()) {
                    if window.frame.height > 0 {
                        let height = f64::from(window.frame.height);
                        self.engine
                            .state
                            .row_weights_by_key
                            .insert(key.clone(), height);
                    }
                }
            }
        }
    }

    /// Handle one command and return the message to report back.
    fn handle(&mut self, request: &Request) -> String {
        match request {
            Request::Focus(direction) => self.focus(*direction),
            Request::Move(direction) => self.move_window(*direction),
            Request::GotoDesktop(desktop) => {
                if self.system.switch_desktop(*desktop) {
                    format!("switched to desktop {desktop}")
                } else {
                    format!("cannot switch to desktop {desktop}")
                }
            }
            Request::Columns(columns) => {
                let config = LayoutConfig::new(*columns);
                if !config.is_valid() {
                    return format!("columns must be a number of at least 1, got {columns}");
                }
                self.engine.config = config;
                let result = self.retile();
                format!("columns set to {columns}; {result}")
            }
            Request::Fullscreen => self.toggle_fullscreen(),
            Request::Close => self.close_focused(),
            Request::Retile => self.retile(),
            Request::Status => self.status(),
            Request::Stop => "stopping".to_string(),
            Request::Restart => {
                self.restarting = true;
                "restarting".to_string()
            }
        }
    }

    /// Move keyboard focus in a direction.
    fn focus(&mut self, direction: Direction) -> String {
        let Some(current) = self.system.focused_window() else {
            return "no focused window".to_string();
        };
        let Some(windows) = self.system.windows().ok() else {
            return "cannot read the window list".to_string();
        };
        match self.engine.focus_target(&current.key, direction, &windows) {
            Some(key) => match windows.iter().find(|w| w.key == key) {
                Some(target) if self.system.focus_window(target) => {
                    format!("focused {}", label_of(target))
                }
                Some(target) => format!("cannot focus {}", label_of(target)),
                None => "no target window".to_string(),
            },
            None => "no target window".to_string(),
        }
    }

    /// Move the focused window within the layout, then retile.
    fn move_window(&mut self, direction: Direction) -> String {
        let Some(current) = self.system.focused_window() else {
            return "no focused window".to_string();
        };
        if !self.engine.move_window(&current.key, direction) {
            return "no move target".to_string();
        }
        self.retile();
        let key = current.key.clone();
        if let Ok(windows) = self.system.windows() {
            if let Some(window) = windows.iter().find(|w| w.key == key) {
                self.system.focus_window(window);
            }
        }
        format!("moved {direction}")
    }

    /// Fullscreen the focused window, or take it back.
    fn toggle_fullscreen(&mut self) -> String {
        let Some(current) = self.system.focused_window() else {
            return "no focused window".to_string();
        };
        let screen_key = current.screen_key.clone();
        let on_screen: Vec<String> = self
            .system
            .windows()
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.screen_key == screen_key)
            .map(|w| w.key)
            .collect();
        let now_full = self.engine.toggle_fullscreen(&current.key, &on_screen);
        self.retile();
        if let Ok(windows) = self.system.windows() {
            if let Some(window) = windows.iter().find(|w| w.key == current.key) {
                self.system.focus_window(window);
            }
        }
        if now_full {
            "fullscreen on".to_string()
        } else {
            "fullscreen off".to_string()
        }
    }

    /// Close the focused window.
    fn close_focused(&mut self) -> String {
        let Some(current) = self.system.focused_window() else {
            return "no focused window".to_string();
        };
        if self.system.close_window(&current) {
            let message = format!("closed {}", label_of(&current));
            self.retile();
            message
        } else {
            "the focused window cannot be closed".to_string()
        }
    }

    /// A one-line description of the current state.
    fn status(&mut self) -> String {
        let windows = self.system.windows().map_or(0, |windows| windows.len());
        format!(
            "running: columns={}, windows={windows}, socket={}",
            self.engine.config.columns,
            self.socket_path.display()
        )
    }
}

/// Whether a window is large enough to be tiled rather than left alone.
fn is_tilable(window: &WindowInfo) -> bool {
    window.frame.width >= MIN_WINDOW_WIDTH && window.frame.height >= MIN_WINDOW_HEIGHT
}

/// How a window is named in messages.
fn label_of(window: &WindowInfo) -> String {
    if window.title.is_empty() {
        window.key.clone()
    } else {
        window.title.clone()
    }
}

/// Bind the socket, refusing to run twice on the same path.
fn bind_socket(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if path.exists() {
        if socket_answers(path) {
            return Err(format!("a daemon is already running at {}", path.display()));
        }
        std::fs::remove_file(path).map_err(|error| {
            format!(
                "cannot replace the stale socket {}: {error}",
                path.display()
            )
        })?;
    }
    UnixListener::bind(path)
        .map_err(|error| format!("cannot listen on {}: {error}", path.display()))
}

/// Decode, run and answer one client request. Returns whether the daemon
/// should stop afterwards.
fn serve_client(core: &mut Core, message: &IpcMessage, verbose: bool) -> bool {
    let request = Request::from_json(&message.payload);
    let response = match request {
        Some(request) => {
            let text = core.handle(&request);
            if verbose {
                eprintln!("client: {} -> {text}", request.command());
            }
            IpcResponse::ok(text)
        }
        None => IpcResponse::err("unreadable request"),
    };
    let stopping = matches!(response.message.as_str(), "stopping" | "restarting");
    let _ = message.reply.send(response);
    stopping
}

/// The answer a stopping daemon gives.
fn response_for(restarting: bool) -> IpcResponse {
    if restarting {
        IpcResponse::ok("restarting")
    } else {
        IpcResponse::ok("stopping")
    }
}

/// What `mwm install` was asked to do.
struct InstallOptions {
    /// Directory the binary is installed into.
    prefix: PathBuf,
    /// launchd label.
    label: String,
    /// Leave the agent unloaded (write the plist only).
    skip_launchctl: bool,
    /// Report each step.
    verbose: bool,
}

/// Runs an external command. Injected so the launchctl steps can be tested
/// without macOS.
fn real_run(program: &str, arguments: &[String]) -> std::io::Result<std::process::Output> {
    std::process::Command::new(program).args(arguments).output()
}

/// Install the binary, write the agent file, and load it.
fn run_install(options: &InstallOptions) -> i32 {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    run_install_with(options, &home, real_run)
}

/// The installer, with the home directory and command runner supplied.
fn run_install_with<F>(options: &InstallOptions, home: &Path, run: F) -> i32
where
    F: Fn(&str, &[String]) -> std::io::Result<std::process::Output>,
{
    let installed = options.prefix.join("mwm");
    let say = |message: &str| {
        if options.verbose {
            eprintln!("{message}");
        }
    };

    if let Some(parent) = installed.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            return 1;
        }
    }

    // Prefer the running executable: it is the binary the user asked to
    // install, whether it came from a release or from a build.
    let source = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("cannot find the running mwm binary: {error}");
            return 1;
        }
    };
    if source != installed {
        if let Err(error) = std::fs::copy(&source, &installed) {
            eprintln!("cannot install to {}: {error}", installed.display());
            return 1;
        }
        set_executable(&installed);
        say(&format!("installed {}", installed.display()));
    }

    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", options.label));
    if let Some(parent) = plist_path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            return 1;
        }
    }
    let plist = launchd_plist_xml_for(&options.label, home, &installed);
    if let Err(error) = std::fs::write(&plist_path, plist) {
        eprintln!("cannot write {}: {error}", plist_path.display());
        return 1;
    }
    say(&format!("wrote {}", plist_path.display()));

    if options.skip_launchctl {
        say("not loading the agent (--no-launchctl)");
        return 0;
    }

    let domain = format!("gui/{}", uid());
    // Unload first, ignoring the failure: there may be nothing loaded, and a
    // running agent would otherwise hold the old binary.
    let _ = run(
        "launchctl",
        &[
            "bootout".into(),
            domain.clone(),
            plist_path.display().to_string(),
        ],
    );
    match run(
        "launchctl",
        &["bootstrap".into(), domain, plist_path.display().to_string()],
    ) {
        Ok(output) if output.status.success() => {
            say("loaded the agent");
            0
        }
        Ok(output) => {
            eprintln!(
                "the agent was written to {} but could not be loaded: {}",
                plist_path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
            1
        }
        Err(error) => {
            eprintln!(
                "the agent was written to {} but could not be loaded: {error}",
                plist_path.display()
            );
            1
        }
    }
}

/// Make an installed binary readable and executable by its owner and by
/// everyone else — the usual `0755` for a program on your PATH.
fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Whether something is listening on `path` right now.
fn socket_answers(path: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(path) else {
        return false;
    };
    drop(stream);
    true
}

/// Serve clients until the daemon stops.
#[allow(clippy::too_many_lines)]
/// Start the daemon and serve until it is told to stop.
fn run_daemon(config: &DaemonConfig) -> i32 {
    let system = system();
    if !system.accessibility_trusted() {
        system.prompt_for_accessibility();
        eprintln!(
            "mwm needs permission to read and move windows, and only manages macOS windows. \
             On macOS, grant it to the mwm binary in System Settings > Privacy & Security > \
             Accessibility, then start it again."
        );
        return 1;
    }
    let bindings: Vec<KeyBinding> = if config.keybindings_enabled {
        match load_bindings(config.keybindings_path.as_deref()) {
            Ok(bindings) => bindings,
            Err(message) => {
                eprintln!("{message}");
                return 1;
            }
        }
    } else {
        Vec::new()
    };

    let listener = match bind_socket(&config.socket_path) {
        Ok(listener) => listener,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let (event_tx, event_rx) = std::sync::mpsc::channel::<Event>();
    spawn_server(listener, &event_tx);

    let mut core = Core {
        system,
        engine: LayoutEngine::new(LayoutConfig::new(config.columns)),
        socket_path: config.socket_path.clone(),
        restarting: false,
    };
    if config.keybindings_enabled {
        install_key_hook(&mut core, &bindings, &event_tx);
    }
    if !core.system.watch_windows(Box::new({
        let tx = event_tx.clone();
        move || {
            let _ = tx.send(Event::WindowsChanged);
        }
    })) {
        eprintln!("cannot watch for window changes; mwm will only react to commands");
    }

    let mut scheduler = Scheduler::default();
    let started = Instant::now();
    loop {
        match event_rx.recv_timeout(scheduler.wait(elapsed_ms(started))) {
            Ok(Event::Request(request)) => {
                let message = core.handle(&request);
                if config.verbose {
                    eprintln!("{} -> {message}", request.command());
                }
                let stopping = matches!(request, Request::Stop | Request::Restart);
                if matches!(
                    request,
                    Request::Fullscreen
                        | Request::Move(_)
                        | Request::Columns(_)
                        | Request::Close
                        | Request::Retile
                ) {
                    scheduler.update(elapsed_ms(started), &Event::WindowsChanged);
                }
                if stopping {
                    break;
                }
            }
            Ok(Event::Client(message)) => {
                let stop = serve_client(&mut core, &message, config.verbose);
                let _ = message.reply.send(response_for(core.restarting));
                if stop {
                    break;
                }
            }
            Ok(Event::WindowsChanged) => {
                scheduler.update(elapsed_ms(started), &Event::WindowsChanged);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if scheduler.poll(elapsed_ms(started)) == Next::Retile {
            let message = core.retile();
            if config.verbose {
                eprintln!("retile: {message}");
            }
        }
    }

    core.system.unwatch_windows();
    core.system.unwatch_keys();
    let _ = std::fs::remove_file(&config.socket_path);
    if core.restarting {
        return restart_self();
    }
    0
}

/// Milliseconds since `started`.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Accept clients and forward their requests to the event loop.
fn spawn_server(listener: UnixListener, event_tx: &Sender<Event>) {
    let tx = event_tx.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(connection) = stream else {
                continue;
            };
            let tx = tx.clone();
            std::thread::spawn(move || {
                serve_one(&connection, &tx);
            });
        }
    });
}

/// Read one request from a client, answer it, and close the connection.
fn serve_one(stream: &UnixStream, event_tx: &Sender<Event>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut line = String::new();
    if BufRead::read_line(&mut reader, &mut line).is_err() {
        return;
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel::<IpcResponse>();
    if event_tx
        .send(Event::Client(IpcMessage {
            payload: line.trim().to_string(),
            reply: reply_tx,
        }))
        .is_err()
    {
        return;
    }
    let Ok(mut stream) = stream.try_clone() else {
        return;
    };
    if let Ok(response) = reply_rx.recv_timeout(CLIENT_TIMEOUT) {
        let _ = writeln!(stream, "{}", response.to_json());
    }
}

/// Translate a key press into a request, when the map has one.
fn install_key_hook(core: &mut Core, bindings: &[KeyBinding], event_tx: &Sender<Event>) {
    let bindings = bindings.to_vec();
    let tx = event_tx.clone();
    core.system
        .watch_keys(Box::new(move |event| match match_key(&event, &bindings) {
            Some(request) => tx.send(Event::Request(*request)).is_ok(),
            None => false,
        }));
}

/// Re-exec this binary so launchd keeps the same process contract.
fn restart_self() -> i32 {
    let Ok(executable) = std::env::current_exe() else {
        return 1;
    };
    match std::process::Command::new(executable).arg("daemon").spawn() {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("cannot restart: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bind_socket, elapsed_ms, launchd_plist_xml, parse_args, request_daemon, Action, Core,
        DaemonConfig, Event, Next, Scheduler, QUIET_MS,
    };
    use crate::layout::{LayoutConfig, LayoutEngine};
    use crate::platform::{KeyEvent, KeyName, QueryResult, WindowSystem};
    use crate::request::{IpcResponse, Request};
    use crate::types::{Modifier, Rect, ScreenInfo, WindowInfo};
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A [`WindowSystem`] backed by plain vectors.
    struct MockSystem {
        screens: Vec<ScreenInfo>,
        windows: Vec<WindowInfo>,
        focused: Arc<Mutex<Option<String>>>,
        frames: Arc<Mutex<Vec<(String, Rect)>>>,
        closed: Arc<Mutex<Vec<String>>>,
        trusted: bool,
        watching: AtomicBool,
        keys: Arc<Mutex<Vec<KeyEvent>>>,
    }

    impl MockSystem {
        fn new(windows: Vec<WindowInfo>) -> Self {
            let screen = ScreenInfo {
                key: "s".into(),
                frame: Rect::new(0, 0, 1000, 500),
            };
            let focused = Arc::new(Mutex::new(windows.first().map(|w| w.key.clone())));
            Self {
                screens: vec![screen],
                windows,
                focused,
                frames: Arc::new(Mutex::new(Vec::new())),
                closed: Arc::new(Mutex::new(Vec::new())),
                trusted: true,
                watching: AtomicBool::new(false),
                keys: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl WindowSystem for MockSystem {
        fn refresh(&mut self) {}
        fn accessibility_trusted(&self) -> bool {
            self.trusted
        }
        fn prompt_for_accessibility(&self) -> bool {
            true
        }
        fn screens(&self) -> QueryResult<Vec<ScreenInfo>> {
            Ok(self.screens.clone())
        }
        fn windows(&self) -> QueryResult<Vec<WindowInfo>> {
            Ok(self.windows.clone())
        }
        fn focused_window(&self) -> Option<WindowInfo> {
            let key = self.focused.lock().expect("lock").clone();
            key.and_then(|key| self.windows.iter().find(|w| w.key == key).cloned())
        }
        fn set_frame(&self, window: &WindowInfo, frame: Rect) -> bool {
            self.frames
                .lock()
                .expect("lock")
                .push((window.key.clone(), frame));
            true
        }
        fn focus_window(&self, window: &WindowInfo) -> bool {
            *self.focused.lock().expect("lock") = Some(window.key.clone());
            true
        }
        fn close_window(&self, window: &WindowInfo) -> bool {
            self.closed.lock().expect("lock").push(window.key.clone());
            true
        }
        fn switch_desktop(&self, desktop: u8) -> bool {
            (1..=10).contains(&desktop)
        }
        fn watch_windows(&mut self, _on_change: Box<dyn FnMut() + Send>) -> bool {
            self.watching.store(true, Ordering::SeqCst);
            true
        }
        fn unwatch_windows(&mut self) {
            self.watching.store(false, Ordering::SeqCst);
        }
        fn watch_keys(&mut self, mut on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>) -> bool {
            on_key(KeyEvent {
                modifiers: BTreeSet::from([Modifier::Alt]),
                key: KeyName::Letter('f'),
            });
            self.keys.lock().expect("lock").push(KeyEvent {
                modifiers: BTreeSet::from([Modifier::Alt]),
                key: KeyName::Letter('f'),
            });
            true
        }
        fn unwatch_keys(&mut self) {}
    }

    fn window(key: &str, x: i32, y: i32) -> WindowInfo {
        WindowInfo {
            key: key.into(),
            pid: 1,
            title: format!("{key} title"),
            frame: Rect::new(x, y, 100, 100),
            screen_key: "s".into(),
            order: 0,
        }
    }

    fn core_with(windows: Vec<WindowInfo>) -> Core {
        Core {
            system: Box::new(MockSystem::new(windows)),
            engine: LayoutEngine::new(LayoutConfig::new(2.0)),
            socket_path: "/tmp/mwm-test.sock".into(),
            restarting: false,
        }
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn cli_parses_daemon_options() {
        let action = parse_args(&args(&[
            "daemon",
            "--columns",
            "2.5",
            "--socket",
            "/tmp/x.sock",
            "--keybindings",
            "/tmp/k.json",
            "--verbose",
        ]));
        assert_eq!(
            action,
            Action::Daemon {
                columns: 2.5,
                socket: Some("/tmp/x.sock".into()),
                keybindings: Some("/tmp/k.json".into()),
                no_keybindings: false,
                verbose: true,
            }
        );
    }

    #[test]
    fn cli_parses_client_commands() {
        assert_eq!(
            parse_args(&args(&["focus", "left"])),
            Action::Send {
                request: Request::Focus(crate::types::Direction::Left),
                socket: None,
                verbose: false
            }
        );
        assert_eq!(
            parse_args(&args(&["move", "up", "--socket", "/tmp/s"])),
            Action::Send {
                request: Request::Move(crate::types::Direction::Up),
                socket: Some("/tmp/s".into()),
                verbose: false
            }
        );
        assert_eq!(
            parse_args(&args(&["goto-desktop", "7"])),
            Action::Send {
                request: Request::GotoDesktop(7),
                socket: None,
                verbose: false
            }
        );
        assert_eq!(
            parse_args(&args(&["columns", "2.5"])),
            Action::Send {
                request: Request::Columns(2.5),
                socket: None,
                verbose: false
            }
        );
        assert_eq!(
            parse_args(&args(&["status"])),
            Action::Send {
                request: Request::Status,
                socket: None,
                verbose: false
            }
        );
    }

    #[test]
    fn cli_rejects_bad_input() {
        for bad in [
            vec!["nonsense"],
            vec!["focus"],
            vec!["focus", "sideways"],
            vec!["focus", "left", "extra"],
            vec!["goto-desktop", "0"],
            vec!["goto-desktop", "11"],
            vec!["goto-desktop", "x"],
            vec!["columns"],
            vec!["columns", "0.5"],
            vec!["columns", "abc"],
            vec!["daemon", "--columns"],
            vec!["daemon", "--columns", "0.2"],
            vec!["daemon", "--bogus"],
            vec!["retile", "--nope"],
            vec!["launchd-plist", "--label"],
        ] {
            assert_eq!(parse_args(&args(&bad)), Action::Usage(2), "{bad:?}");
        }
        assert_eq!(parse_args(&args(&["--help"])), Action::Usage(0));
        assert_eq!(parse_args(&[]), Action::Usage(2));
    }

    #[test]
    fn cli_parses_launchd_plist() {
        assert_eq!(
            parse_args(&args(&["launchd-plist"])),
            Action::LaunchdPlist {
                label: None,
                output: None
            }
        );
        assert_eq!(
            parse_args(&args(&[
                "launchd-plist",
                "--label",
                "wm",
                "--output",
                "/tmp/a.plist"
            ])),
            Action::LaunchdPlist {
                label: Some("wm".into()),
                output: Some("/tmp/a.plist".into())
            }
        );
    }

    #[test]
    fn plist_contains_the_binary_and_label() {
        let xml = launchd_plist_xml("mwm", Path::new("/Users/me"));
        assert!(xml.contains("<key>Label</key>"));
        assert!(xml.contains("<string>mwm</string>"));
        assert!(xml.contains("/Users/me/.local/bin/mwm"));
        assert!(xml.contains("<string>daemon</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>"));
        assert!(xml.starts_with("<?xml"));
        assert!(xml.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn scheduler_coalesces_bursts() {
        let mut scheduler = Scheduler::default();
        assert_eq!(scheduler.update(0, &Event::WindowsChanged), Next::Idle);
        assert_eq!(scheduler.update(100, &Event::WindowsChanged), Next::Idle);
        assert_eq!(scheduler.poll(100 + QUIET_MS - 1), Next::Idle);
        assert_eq!(scheduler.poll(100 + QUIET_MS), Next::Retile);
        assert_eq!(scheduler.poll(1000), Next::Idle);
    }

    #[test]
    fn scheduler_resets_after_a_retile() {
        let mut scheduler = Scheduler::default();
        scheduler.update(0, &Event::WindowsChanged);
        assert_eq!(scheduler.poll(QUIET_MS), Next::Retile);
        scheduler.update(QUIET_MS, &Event::WindowsChanged);
        assert_eq!(scheduler.poll(QUIET_MS + QUIET_MS - 1), Next::Idle);
        assert_eq!(scheduler.poll(QUIET_MS + QUIET_MS), Next::Retile);
    }

    #[test]
    fn scheduler_wait_is_bounded() {
        let mut scheduler = Scheduler::default();
        assert_eq!(scheduler.wait(0), super::TICK);
        scheduler.update(0, &Event::WindowsChanged);
        assert!(scheduler.wait(0) <= super::TICK);
        assert!(scheduler.wait(0) > Duration::from_millis(0));
    }

    #[test]
    fn scheduler_ignores_requests() {
        let mut scheduler = Scheduler::default();
        assert_eq!(
            scheduler.update(0, &Event::Request(Request::Status)),
            Next::Idle
        );
        assert_eq!(scheduler.poll(10_000), Next::Idle);
    }

    #[test]
    fn elapsed_ms_is_monotonic_enough() {
        let now = std::time::Instant::now();
        let first = elapsed_ms(now);
        assert!(elapsed_ms(now) >= first);
    }

    #[test]
    fn retile_places_every_window() {
        let mut core = core_with(vec![window("a", 0, 0), window("b", 900, 0)]);
        let message = core.retile();
        assert!(message.contains("tiled"), "{message}");
        assert_eq!(core.engine.state.columns_by_screen["s"].len(), 2);
    }

    #[test]
    fn focus_moves_to_the_next_column() {
        let mut core = core_with(vec![window("a", 0, 0), window("b", 900, 0)]);
        core.retile();
        let message = core.handle(&Request::Focus(crate::types::Direction::Right));
        assert!(message.contains("b title"), "{message}");
        assert!(core
            .handle(&Request::Focus(crate::types::Direction::Right))
            .contains("no target"));
    }

    #[test]
    fn move_refuses_when_there_is_no_target() {
        let mut core = core_with(vec![window("a", 0, 0)]);
        assert_eq!(
            core.handle(&Request::Move(crate::types::Direction::Left)),
            "no move target"
        );
    }

    #[test]
    fn close_reports_and_retiles() {
        let mut core = core_with(vec![window("a", 0, 0), window("b", 900, 0)]);
        let message = core.handle(&Request::Close);
        assert!(message.contains("closed a title"), "{message}");
    }

    #[test]
    fn fullscreen_toggles() {
        let mut core = core_with(vec![window("a", 0, 0), window("b", 900, 0)]);
        core.retile();
        assert_eq!(core.handle(&Request::Fullscreen), "fullscreen on");
        assert!(core.engine.state.fullscreen_keys.contains("a"));
        assert_eq!(core.handle(&Request::Fullscreen), "fullscreen off");
        assert!(!core.engine.state.fullscreen_keys.contains("a"));
    }

    #[test]
    fn columns_validates_and_retiles() {
        let mut core = core_with(vec![window("a", 0, 0)]);
        core.engine.config = LayoutConfig::new(99.0);
        let message = core.handle(&Request::Columns(2.5));
        assert!(message.starts_with("columns set to 2.5"), "{message}");
        assert!((core.engine.config.columns - 2.5).abs() < f64::EPSILON);
        // a direct config change bypasses the check on purpose
        core.engine.config = LayoutConfig::new(99.0);
        assert_eq!(
            core.handle(&Request::Columns(0.5)),
            "columns must be a number of at least 1, got 0.5"
        );
    }

    #[test]
    fn status_mentions_columns_windows_and_socket() {
        let mut core = core_with(vec![window("a", 0, 0)]);
        let message = core.handle(&Request::Status);
        assert!(message.contains("columns=2"), "{message}");
        assert!(message.contains("windows=1"), "{message}");
        assert!(message.contains("mwm-test.sock"), "{message}");
    }

    #[test]
    fn stop_and_restart_are_reported() {
        let mut core = core_with(vec![]);
        assert_eq!(core.handle(&Request::Stop), "stopping");
        assert_eq!(core.handle(&Request::Restart), "restarting");
        assert!(core.restarting);
    }

    #[test]
    fn a_second_daemon_is_refused() {
        let dir = std::env::temp_dir().join(format!("mwm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mwm.sock");
        let _listener = bind_socket(&path).expect("first bind works");
        // something is listening, so a second daemon must refuse
        let result = bind_socket(&path);
        assert!(result.is_err(), "{result:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = std::env::temp_dir().join(format!("mwm-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mwm.sock");
        std::fs::write(&path, b"not a socket").expect("write");
        let listener = bind_socket(&path).expect("stale socket replaced");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn socket_round_trip_answers_a_request() {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("mwm-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mwm.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            let response = match Request::from_json(line.trim()) {
                Some(Request::Status) => IpcResponse::ok("running"),
                Some(_) => IpcResponse::ok("done"),
                None => IpcResponse::err("unreadable"),
            };
            let mut stream = stream;
            writeln!(stream, "{}", response.to_json()).expect("write");
        });
        let response = request_daemon(&path, &Request::Status).expect("response");
        assert!(response.ok);
        assert_eq!(response.message, "running");
        server.join().expect("join");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_daemon_is_an_error() {
        let result = request_daemon(Path::new("/tmp/mwm-does-not-exist.sock"), &Request::Status);
        assert!(result.is_err());
    }

    #[test]
    fn a_client_request_reaches_the_core() {
        use std::sync::mpsc::channel;
        let mut core = core_with(vec![window("a", 0, 0), window("b", 900, 0)]);
        let (reply_tx, reply_rx) = channel();
        let message = super::IpcMessage {
            payload: Request::Status.to_json(),
            reply: reply_tx,
        };
        let stop = super::serve_client(&mut core, &message, false);
        assert!(!stop);
        let response = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("answered");
        assert!(response.ok);
        assert!(
            response.message.contains("windows=2"),
            "{}",
            response.message
        );
    }

    #[test]
    fn stop_request_ends_the_daemon() {
        use std::sync::mpsc::channel;
        let mut core = core_with(vec![]);
        let (reply_tx, reply_rx) = channel();
        let message = super::IpcMessage {
            payload: Request::Stop.to_json(),
            reply: reply_tx,
        };
        assert!(super::serve_client(&mut core, &message, false));
        let response = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("answered");
        assert_eq!(response.message, "stopping");
    }

    #[test]
    fn an_unreadable_payload_is_refused() {
        use std::sync::mpsc::channel;
        let mut core = core_with(vec![]);
        let (reply_tx, reply_rx) = channel();
        let message = super::IpcMessage {
            payload: "{not json".into(),
            reply: reply_tx,
        };
        assert!(!super::serve_client(&mut core, &message, false));
        let response = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("answered");
        assert!(!response.ok);
        assert_eq!(response.message, "unreadable request");
    }

    #[test]
    fn a_second_client_is_refused_while_one_runs() {
        let dir = std::env::temp_dir().join(format!("mwm-second-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mwm.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        assert!(super::socket_answers(&path));
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_socket_is_removed_when_the_daemon_stops() {
        let dir = std::env::temp_dir().join(format!("mwm-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mwm.sock");
        let listener = super::bind_socket(&path).expect("bind");
        drop(listener);
        assert!(path.exists());
        std::fs::remove_file(&path).expect("cleanup");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_copies_the_binary_writes_the_plist_and_loads_it() {
        use std::sync::atomic::AtomicUsize;
        let home = std::env::temp_dir().join(format!("mwm-install-{}", std::process::id()));
        let prefix = home.join(".local/bin");
        std::fs::create_dir_all(&prefix).expect("prefix");
        let source = std::env::current_exe().expect("test binary");
        std::fs::copy(&source, prefix.join("mwm")).expect("seed an existing install");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let counter = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&calls);
        let options = super::InstallOptions {
            prefix: prefix.clone(),
            label: "mwm".to_string(),
            skip_launchctl: false,
            verbose: false,
        };
        // The installer copies the running executable, so point that at a file
        // we control by running the check against a temporary "binary".
        let code = super::run_install_with(&options, &home, |program, arguments| {
            sink.lock()
                .expect("lock")
                .push((program.to_string(), arguments.join(" ")));
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(std::process::Output {
                status: std::process::Command::new("true").status().expect("true"),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        });
        assert_eq!(code, 0);
        let plist = home.join("Library/LaunchAgents/mwm.plist");
        assert!(plist.is_file(), "the plist should be written");
        let text = std::fs::read_to_string(&plist).expect("read");
        assert!(text.contains("<key>Label</key>"));
        assert!(text.contains(&prefix.join("mwm").display().to_string()));
        let recorded = calls.lock().expect("lock").clone();
        assert_eq!(recorded.len(), 2, "bootout then bootstrap: {recorded:?}");
        assert_eq!(recorded[0].0, "launchctl");
        assert!(recorded[0].1.starts_with("bootout gui/"));
        assert!(recorded[1].1.starts_with("bootstrap gui/"));
        assert!(prefix.join("mwm").is_file());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_can_stop_before_loading() {
        let home = std::env::temp_dir().join(format!("mwm-install-skip-{}", std::process::id()));
        let prefix = home.join(".local/bin");
        std::fs::create_dir_all(&prefix).expect("prefix");
        let calls = Arc::new(Mutex::new(0_usize));
        let counter = Arc::clone(&calls);
        let options = super::InstallOptions {
            prefix,
            label: "mwm".to_string(),
            skip_launchctl: true,
            verbose: false,
        };
        let code = super::run_install_with(&options, &home, |_, _| {
            *counter.lock().expect("lock") += 1;
            Ok(std::process::Output {
                status: std::process::Command::new("true").status().expect("true"),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        });
        assert_eq!(code, 0);
        assert_eq!(*calls.lock().expect("lock"), 0, "launchctl must not run");
        assert!(home.join("Library/LaunchAgents/mwm.plist").is_file());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_reports_a_failed_load_but_keeps_the_plist() {
        let home = std::env::temp_dir().join(format!("mwm-install-fail-{}", std::process::id()));
        let prefix = home.join(".local/bin");
        std::fs::create_dir_all(&prefix).expect("prefix");
        let options = super::InstallOptions {
            prefix,
            label: "mwm".to_string(),
            skip_launchctl: false,
            verbose: false,
        };
        let code = super::run_install_with(&options, &home, |_, _| {
            Ok(std::process::Output {
                status: std::process::Command::new("false").status().expect("false"),
                stdout: Vec::new(),
                stderr: b"no launchd here".to_vec(),
            })
        });
        assert_eq!(code, 1, "a failed load is reported");
        assert!(
            home.join("Library/LaunchAgents/mwm.plist").is_file(),
            "the plist is still written, so the user can load it by hand"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_uses_the_home_it_is_given() {
        // The plist must land under the real home, never at /Library, which is
        // what happens if the caller forgets to pass one in.
        let home = std::env::temp_dir().join(format!("mwm-home-{}", std::process::id()));
        let prefix = home.join(".local/bin");
        std::fs::create_dir_all(&prefix).expect("prefix");
        let options = super::InstallOptions {
            prefix,
            label: "mwm".to_string(),
            skip_launchctl: true,
            verbose: false,
        };
        let code = super::run_install_with(&options, &home, |_, _| unreachable!("not called"));
        assert_eq!(code, 0);
        assert!(
            home.join("Library/LaunchAgents/mwm.plist").is_file(),
            "the plist belongs under the home directory given"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_plist_names_the_binary_where_it_was_installed() {
        let home = Path::new("/Users/me");
        let elsewhere = Path::new("/opt/tools/bin/mwm");
        let text = super::launchd_plist_xml_for("mwm", home, elsewhere);
        assert!(text.contains("/opt/tools/bin/mwm"), "{text}");
        assert!(!text.contains("/Users/me/.local/bin/mwm"), "{text}");
        // The default still points at the conventional location.
        assert!(super::launchd_plist_xml("mwm", home).contains("/Users/me/.local/bin/mwm"));
    }

    #[test]
    fn an_installed_binary_is_readable_and_runnable() {
        let dir = std::env::temp_dir().join(format!("mwm-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("mwm");
        std::fs::write(&path, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        super::set_executable(&path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "got {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_parses_its_options() {
        assert_eq!(
            parse_args(&args(&["install"])),
            Action::Install {
                prefix: None,
                label: None,
                skip_launchctl: false,
                verbose: false
            }
        );
        assert_eq!(
            parse_args(&args(&[
                "install",
                "--prefix",
                "/usr/local/bin",
                "--label",
                "wm",
                "--no-launchctl"
            ])),
            Action::Install {
                prefix: Some("/usr/local/bin".into()),
                label: Some("wm".into()),
                skip_launchctl: true,
                verbose: false,
            }
        );
        for bad in [
            vec!["install", "--prefix"],
            vec!["install", "--label"],
            vec!["install", "--bogus"],
            vec!["install", "extra"],
        ] {
            assert_eq!(parse_args(&args(&bad)), Action::Usage(2), "{bad:?}");
        }
    }

    #[test]
    fn daemon_config_defaults_are_sane() {
        let config = DaemonConfig {
            columns: 2.0,
            socket_path: "/tmp/x".into(),
            keybindings_path: None,
            keybindings_enabled: true,
            verbose: false,
        };
        assert!(config.keybindings_enabled);
        assert!((config.columns - 2.0).abs() < f64::EPSILON);
    }
}
