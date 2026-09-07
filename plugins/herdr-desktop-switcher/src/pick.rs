//! Popup summoning and the hand-rolled desktop picker.

use std::collections::HashSet;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value, json};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::api::ApiClient;
use crate::launch::{LaunchOutcome, RuntimePaths, launch_desktop};
use crate::parse::{DesktopConfig, DesktopMode, load_path};
use crate::registry::{InstanceRecord, Registry};

const PLUGIN_ID: &str = "herdr-desktop-switcher";
const PICKER_ENTRYPOINT: &str = "picker";
const FIRST_ROW_Y: usize = 7;
const RESULT_HOLD: Duration = Duration::from_millis(1200);

/// Plugin-action leg: ask Herdr to open the declared pane as a focused popup.
pub async fn summon(paths: &RuntimePaths) -> Result<(), String> {
    let socket = env::var_os("HERDR_SOCKET_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HERDR_SOCKET_PATH is not set".to_string())?;
    let api = ApiClient::connect(&socket)
        .await
        .map_err(|error| error.to_string())?;
    let (width, height) = popup_dimensions(paths);
    let mut pane_env = Map::new();
    for (name, path) in popup_environment(paths) {
        pane_env.insert(
            name.into(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    let params = json!({
        "plugin_id": PLUGIN_ID,
        "entrypoint": PICKER_ENTRYPOINT,
        "placement": "popup",
        "width": width,
        "height": height,
        "focus": true,
        "env": pane_env,
    });

    if let Err(error) = api.request("plugin.pane.open", params).await {
        if error.to_string().contains("popup already open") {
            api.request("popup.close", json!({}))
                .await
                .map_err(|close_error| close_error.to_string())?;
            return Ok(());
        }
        return Err(error.to_string());
    }
    Ok(())
}

fn popup_environment(paths: &RuntimePaths) -> [(&'static str, &Path); 7] {
    [
        ("HERDR_APP_PATH", &paths.app),
        ("HERDR_DESKTOP_COMMAND", &paths.command_shim),
        ("HERDR_DESKTOPS_TOML", &paths.config),
        ("HERDR_DESKTOP_LAUNCHER", &paths.launcher),
        ("HERDR_REAL_BIN", &paths.real_herdr),
        ("HERDR_SSH_BIN", &paths.ssh),
        ("HERDR_DESKTOP_STATE_DIR", &paths.state_dir),
    ]
}

fn popup_dimensions(paths: &RuntimePaths) -> (usize, usize) {
    let Ok(config) = load_path(&paths.config) else {
        return (72, 15);
    };
    let widest = config
        .desktops
        .values()
        .map(|desktop| {
            let detail_width = match &desktop.mode {
                DesktopMode::Local => UnicodeWidthStr::width("This Mac"),
                DesktopMode::Remote {
                    target, session, ..
                } => {
                    UnicodeWidthStr::width(target.as_str())
                        + UnicodeWidthStr::width(session.as_str())
                        + 3
                }
            };
            UnicodeWidthStr::width(desktop.label.as_str()) + detail_width + 32
        })
        .max()
        .unwrap_or(72);
    (
        widest.clamp(64, 96),
        (config.desktops.len() + 13).clamp(15, 28),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopStatus {
    Running,
    Offline,
    Unknown,
}

impl DesktopStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Offline => "Offline",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub detail: String,
    pub status: DesktopStatus,
    remote: bool,
}

/// Build deterministic display rows without probing any remote endpoint.
pub fn build_rows(config: &DesktopConfig, live: &[InstanceRecord]) -> Vec<PickerRow> {
    let running: HashSet<&str> = live
        .iter()
        .map(|record| record.desktop_id.as_str())
        .collect();
    let mut ordered = Vec::with_capacity(config.desktops.len());
    if let Some(desktop) = config.desktop(&config.default) {
        ordered.push(desktop);
    }
    ordered.extend(
        config
            .desktops
            .values()
            .filter(|desktop| desktop.id != config.default),
    );

    ordered
        .into_iter()
        .map(|desktop| {
            let (kind, detail, remote) = match &desktop.mode {
                DesktopMode::Local => ("Local", "This Mac".to_string(), false),
                DesktopMode::Remote {
                    target, session, ..
                } => ("Remote", format!("{target} / {session}"), true),
            };
            PickerRow {
                id: desktop.id.clone(),
                label: desktop.label.clone(),
                kind,
                detail,
                status: if running.contains(desktop.id.as_str()) {
                    DesktopStatus::Running
                } else {
                    DesktopStatus::Unknown
                },
                remote,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Notice {
    Busy { title: String },
    Error { title: String, detail: String },
    Outcome { title: String, detail: String },
}

#[derive(Debug, Clone)]
pub struct PickerState {
    pub rows: Vec<PickerRow>,
    pub selected: usize,
    offset: usize,
    notice: Option<Notice>,
}

impl PickerState {
    pub fn new(rows: Vec<PickerRow>) -> Self {
        Self {
            rows,
            selected: 0,
            offset: 0,
            notice: None,
        }
    }

    pub fn move_up(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = self.selected.checked_sub(1).unwrap_or(self.rows.len() - 1);
    }

    pub fn move_down(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1) % self.rows.len();
        }
    }

    fn ensure_selected_visible(&mut self, capacity: usize) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(capacity) {
            self.offset = self.selected + 1 - capacity;
        }
    }

    fn start_launch(&mut self) {
        if let Some(row) = self.rows.get(self.selected) {
            self.notice = Some(Notice::Busy {
                title: format!("Opening {}...", row.label),
            });
        }
    }

    fn apply_launch_error(&mut self, error: &str) {
        let Some(row) = self.rows.get_mut(self.selected) else {
            return;
        };
        row.status = if row.remote && clearly_remote_failure(error) {
            DesktopStatus::Offline
        } else {
            DesktopStatus::Unknown
        };
        self.notice = Some(Notice::Error {
            title: format!("Could not open {}", row.label),
            detail: sanitize_text(error),
        });
    }

    fn apply_outcome(&mut self, outcome: &LaunchOutcome) {
        let Some(row) = self.rows.get_mut(self.selected) else {
            return;
        };
        row.status = DesktopStatus::Running;
        self.notice = Some(Notice::Outcome {
            title: outcome_copy(outcome).into(),
            detail: outcome_detail(outcome, &row.label),
        });
    }

    fn set_system_error(&mut self, title: impl Into<String>, detail: impl Into<String>) {
        self.notice = Some(Notice::Error {
            title: title.into(),
            detail: sanitize_text(&detail.into()),
        });
    }
}

/// Popup leg: render the picker, launch or focus one desktop, then exit.
pub fn picker(paths: &RuntimePaths) -> Result<(), String> {
    let mut state = match load_path(&paths.config) {
        Ok(config) => {
            let registry = Registry::new(&paths.state_dir);
            match registry.prune() {
                Ok(live) => PickerState::new(build_rows(&config, &live)),
                Err(error) => {
                    let mut state = PickerState::new(build_rows(&config, &[]));
                    state.set_system_error("Running status unavailable", error.to_string());
                    state
                }
            }
        }
        Err(error) => {
            let mut state = PickerState::new(Vec::new());
            state.set_system_error("Desktop list unavailable", error.to_string());
            state
        }
    };

    if !interactive_terminal() {
        print_plain(&state)?;
        return Err("picker requires an interactive terminal".into());
    }

    run_picker(paths, &mut state)
}

fn run_picker(paths: &RuntimePaths, state: &mut PickerState) -> Result<(), String> {
    let _terminal = RawTerminal::enable().map_err(|error| error.to_string())?;
    drain_input();
    let mut output = io::stdout().lock();
    let mut size = terminal_size();
    state.ensure_selected_visible(viewport_capacity(size.rows));
    draw(&mut output, state, size).map_err(|error| error.to_string())?;

    loop {
        let key = read_event(size)?;
        let mut activate = false;
        match key {
            Key::Up => state.move_up(),
            Key::Down => state.move_down(),
            Key::Enter => activate = !state.rows.is_empty(),
            Key::Click { y } => {
                if let Some(visible) = (y as usize).checked_sub(FIRST_ROW_Y) {
                    let index = state.offset + visible;
                    let capacity = viewport_capacity(size.rows);
                    if visible < capacity && index < state.rows.len() {
                        state.selected = index;
                        activate = true;
                    }
                }
            }
            Key::Cancel => {
                drain_input();
                return Ok(());
            }
            Key::Resize | Key::Other => {}
        }

        size = terminal_size();
        state.ensure_selected_visible(viewport_capacity(size.rows));
        if !activate {
            draw(&mut output, state, size).map_err(|error| error.to_string())?;
            continue;
        }

        state.start_launch();
        draw(&mut output, state, size).map_err(|error| error.to_string())?;
        let desktop_id = state.rows[state.selected].id.clone();
        let outcome = launch_desktop(paths, &desktop_id).map_err(|error| error.to_string());
        drain_input();
        match outcome {
            Ok(outcome) => {
                state.apply_outcome(&outcome);
                draw(&mut output, state, terminal_size()).map_err(|error| error.to_string())?;
                thread::sleep(RESULT_HOLD);
                return Ok(());
            }
            Err(error) => {
                state.apply_launch_error(&error);
                size = terminal_size();
                draw(&mut output, state, size).map_err(|draw_error| draw_error.to_string())?;
            }
        }
    }
}

fn outcome_copy(outcome: &LaunchOutcome) -> &'static str {
    match outcome {
        LaunchOutcome::Launched { .. } => "Launched",
        LaunchOutcome::Focused { .. } => "Focused",
    }
}

fn outcome_detail(outcome: &LaunchOutcome, label: &str) -> String {
    match outcome {
        LaunchOutcome::Launched { .. } => format!("Herdr client launched for {label}."),
        LaunchOutcome::Focused { .. } => format!("Existing Herdr client focused for {label}."),
    }
}

fn clearly_remote_failure(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    [
        "failed bounded non-interactive ssh preflight",
        "could not resolve hostname",
        "connection refused",
        "connection timed out",
        "operation timed out",
        "network is unreachable",
        "no route to host",
        "host is down",
        "host key verification failed",
        "permission denied (publickey",
        "connection closed",
        "connection reset",
        "ssh_exchange_identification",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate to terminal-cell width without splitting a Unicode scalar value.
pub fn truncate_to_width(value: &str, maximum: usize) -> String {
    if UnicodeWidthStr::width(value) <= maximum {
        return value.to_string();
    }
    if maximum == 0 {
        return String::new();
    }
    let ellipsis = '…';
    let ellipsis_width = ellipsis.width().unwrap_or(1);
    if maximum <= ellipsis_width {
        return ellipsis.to_string();
    }
    let available = maximum - ellipsis_width;
    let mut result = String::new();
    let mut width = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > available {
            break;
        }
        result.push(character);
        width += character_width;
    }
    result.push(ellipsis);
    result
}

fn pad_to_width(value: &str, width: usize) -> String {
    let value = truncate_to_width(value, width);
    let padding = width.saturating_sub(UnicodeWidthStr::width(value.as_str()));
    format!("{value}{}", " ".repeat(padding))
}

/// Produce one fixed-width, unstyled row. Selection remains visible without color.
pub fn render_row_text(
    row: &PickerRow,
    selected: bool,
    width: usize,
    preferred_label_width: usize,
) -> String {
    let marker = if selected { "> " } else { "  " };
    let status = row.status.label();
    if width <= 11 {
        return truncate_to_width(&format!("{marker}{}", row.label), width);
    }
    if width < 36 {
        let label_width = width - 11;
        return format!(
            "{marker}{}  {status}",
            pad_to_width(&row.label, label_width)
        );
    }
    if width < 48 {
        let label_width = width - 19;
        return format!(
            "{marker}{}  {:6}  {status}",
            pad_to_width(&row.label, label_width),
            row.kind
        );
    }

    let label_width = preferred_label_width.clamp(10, width.saturating_sub(31));
    let detail_width = width.saturating_sub(label_width + 21);
    format!(
        "{marker}{}  {:6}  {}  {status}",
        pad_to_width(&row.label, label_width),
        row.kind,
        pad_to_width(&row.detail, detail_width)
    )
}

fn header_text(width: usize, preferred_label_width: usize) -> String {
    let header = PickerRow {
        id: String::new(),
        label: "DESKTOP".into(),
        kind: "TYPE",
        detail: "TARGET / SESSION".into(),
        status: DesktopStatus::Unknown,
        remote: false,
    };
    let mut line = render_row_text(&header, false, width, preferred_label_width);
    if line.ends_with("Unknown") {
        line.truncate(line.len() - "Unknown".len());
        line.push_str("STATUS ");
    }
    line
}

fn preferred_label_width(rows: &[PickerRow]) -> usize {
    rows.iter()
        .map(|row| UnicodeWidthStr::width(row.label.as_str()))
        .max()
        .unwrap_or(10)
        .clamp(10, 20)
}

fn viewport_capacity(rows: usize) -> usize {
    rows.saturating_sub(11).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    cols: usize,
    rows: usize,
}

fn terminal_size() -> TerminalSize {
    // SAFETY: ioctl writes into a valid winsize value and does not retain it.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            return TerminalSize {
                cols: size.ws_col as usize,
                rows: size.ws_row as usize,
            };
        }
    }
    TerminalSize { cols: 80, rows: 24 }
}

fn draw(output: &mut impl Write, state: &PickerState, size: TerminalSize) -> io::Result<()> {
    let styled = color_enabled();
    let body_width = size.cols.saturating_sub(8).max(1);
    let label_width = preferred_label_width(&state.rows);
    let capacity = viewport_capacity(size.rows);
    let rule = "─".repeat(body_width);
    let list_label = if state.rows.len() > capacity {
        format!(
            "DESKTOPS   {}-{} of {}",
            state.offset + 1,
            (state.offset + capacity).min(state.rows.len()),
            state.rows.len()
        )
    } else {
        "DESKTOPS".into()
    };
    write!(output, "\x1b[2J\x1b[H")?;
    move_to(output, 2)?;
    write!(
        output,
        "    {}",
        style(&truncate_to_width(&list_label, body_width), "2", styled)
    )?;
    move_to(output, 3)?;
    write!(output, "    {}", style("Open a Herdr desktop", "1", styled))?;
    move_to(output, 4)?;
    write!(
        output,
        "    {}",
        style(
            &truncate_to_width(
                "Running clients are focused; others are launched.",
                body_width
            ),
            "2",
            styled
        )
    )?;
    move_to(output, 5)?;
    write!(output, "    {}", style(&rule, "2", styled))?;
    move_to(output, 6)?;
    write!(
        output,
        "    {}",
        style(&header_text(body_width, label_width), "2", styled)
    )?;

    for (visible, row) in state
        .rows
        .iter()
        .skip(state.offset)
        .take(capacity)
        .enumerate()
    {
        move_to(output, FIRST_ROW_Y + visible)?;
        let selected = state.offset + visible == state.selected;
        let text = render_row_text(row, selected, body_width, label_width);
        if selected {
            write!(output, "    {}", style(&text, "7;1", styled))?;
        } else if let Some(body) = text.strip_suffix(row.status.label()) {
            let status_style = match row.status {
                DesktopStatus::Unknown => "2",
                DesktopStatus::Running | DesktopStatus::Offline => "1",
            };
            write!(
                output,
                "    {body}{}",
                style(row.status.label(), status_style, styled)
            )?;
        } else {
            write!(output, "    {text}")?;
        }
    }

    let divider_y = size.rows.saturating_sub(4).max(FIRST_ROW_Y + 1);
    move_to(output, divider_y)?;
    write!(output, "    {}", style(&rule, "2", styled))?;
    let notice_y = divider_y + 1;
    if let Some(notice) = &state.notice {
        match notice {
            Notice::Busy { title } => {
                move_to(output, notice_y)?;
                write!(
                    output,
                    "    {}",
                    style(&truncate_to_width(title, body_width), "1", styled)
                )?;
            }
            Notice::Error { title, detail } => {
                move_to(output, notice_y)?;
                write!(
                    output,
                    "    {}",
                    style(&truncate_to_width(title, body_width), "1", styled)
                )?;
                for (line, text) in wrap_text(detail, body_width, 2).iter().enumerate() {
                    move_to(output, notice_y + 1 + line)?;
                    write!(output, "    {text}")?;
                }
            }
            Notice::Outcome { title, detail } => {
                move_to(output, notice_y)?;
                write!(
                    output,
                    "    {}",
                    style(&truncate_to_width(title, body_width), "1", styled)
                )?;
                move_to(output, notice_y + 1)?;
                write!(
                    output,
                    "    {}",
                    style(&truncate_to_width(detail, body_width), "2", styled)
                )?;
            }
        }
    }
    move_to(output, size.rows)?;
    let footer = truncate_to_width("Arrows / j k move   Enter open   Esc / q close", body_width);
    write!(output, "    {}", style(&footer, "2", styled))?;
    output.flush()
}

fn move_to(output: &mut impl Write, row: usize) -> io::Result<()> {
    write!(output, "\x1b[{row};1H")
}

fn style(value: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{value}\x1b[0m")
    } else {
        value.to_string()
    }
}

fn color_enabled() -> bool {
    env::var_os("NO_COLOR").is_none() && env::var("TERM").is_ok_and(|term| term != "dumb")
}

fn wrap_text(value: &str, width: usize, maximum_lines: usize) -> Vec<String> {
    if width == 0 || maximum_lines == 0 || value.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut remaining = value.trim();
    while !remaining.is_empty() && lines.len() < maximum_lines {
        if UnicodeWidthStr::width(remaining) <= width {
            lines.push(remaining.to_string());
            break;
        }
        let mut split = 0;
        let mut cells = 0;
        for (index, character) in remaining.char_indices() {
            let next = cells + character.width().unwrap_or(0);
            if next > width {
                break;
            }
            cells = next;
            if character.is_whitespace() {
                split = index;
            }
        }
        if split == 0 {
            lines.push(truncate_to_width(remaining, width));
            remaining = "";
        } else {
            lines.push(remaining[..split].trim_end().to_string());
            remaining = remaining[split..].trim_start();
        }
    }
    if !remaining.is_empty()
        && let Some(last) = lines.last_mut()
    {
        *last = truncate_to_width(&format!("{last} {remaining}"), width);
    }
    lines
}

fn print_plain(state: &PickerState) -> Result<(), String> {
    let mut output = io::stdout().lock();
    writeln!(output, "Open a Herdr desktop").map_err(|error| error.to_string())?;
    writeln!(output, "Running clients are focused; others are launched.")
        .map_err(|error| error.to_string())?;
    for row in &state.rows {
        writeln!(
            output,
            "{}\t{}\t{}\t{}",
            row.label,
            row.kind,
            row.detail,
            row.status.label()
        )
        .map_err(|error| error.to_string())?;
    }
    if let Some(notice) = &state.notice {
        match notice {
            Notice::Busy { title } => writeln!(output, "{title}"),
            Notice::Error { title, detail } | Notice::Outcome { title, detail } => {
                writeln!(output, "{title}: {detail}")
            }
        }
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn interactive_terminal() -> bool {
    // SAFETY: isatty only inspects the two process-owned file descriptors.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    Cancel,
    Click { y: u32 },
    Resize,
    Other,
}

fn read_event(previous_size: TerminalSize) -> Result<Key, String> {
    loop {
        if stdin_readable(120) {
            return read_key();
        }
        if terminal_size() != previous_size {
            return Ok(Key::Resize);
        }
    }
}

fn read_key() -> Result<Key, String> {
    Ok(match read_byte()? {
        b'\r' | b'\n' => Key::Enter,
        b'q' | 0x03 => Key::Cancel,
        b'k' => Key::Up,
        b'j' => Key::Down,
        0x1b => {
            if !stdin_readable(300) {
                return Ok(Key::Cancel);
            }
            match read_byte()? {
                b'[' => {
                    let mut final_byte = 0;
                    let mut parameters = Vec::new();
                    while stdin_readable(60) {
                        let byte = read_byte()?;
                        if (0x40..=0x7e).contains(&byte) {
                            final_byte = byte;
                            break;
                        }
                        parameters.push(byte);
                    }
                    match final_byte {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'M' | b'm' if parameters.first() == Some(&b'<') => {
                            let fields: Vec<u32> = String::from_utf8_lossy(&parameters[1..])
                                .split(';')
                                .filter_map(|field| field.parse().ok())
                                .collect();
                            match (fields.first(), fields.get(2), final_byte) {
                                (Some(64), _, b'M') => Key::Up,
                                (Some(65), _, b'M') => Key::Down,
                                (Some(0), Some(&y), b'M') => Key::Click { y },
                                _ => Key::Other,
                            }
                        }
                        _ => Key::Other,
                    }
                }
                b'O' => match if stdin_readable(60) { read_byte()? } else { 0 } {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    _ => Key::Other,
                },
                _ => Key::Other,
            }
        }
        _ => Key::Other,
    })
}

fn read_byte() -> Result<u8, String> {
    let mut byte = 0;
    loop {
        // SAFETY: read receives a valid one-byte buffer and process-owned fd.
        let count = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                &mut byte as *mut u8 as *mut libc::c_void,
                1,
            )
        };
        match count {
            1 => return Ok(byte),
            0 => return Err("stdin closed".into()),
            _ if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return Err(io::Error::last_os_error().to_string()),
        }
    }
}

fn stdin_readable(timeout_ms: i32) -> bool {
    let mut descriptor = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll receives one initialized descriptor for the process-owned fd.
    unsafe { libc::poll(&mut descriptor, 1, timeout_ms) > 0 }
}

fn drain_input() {
    while stdin_readable(0) {
        if read_byte().is_err() {
            break;
        }
    }
}

struct RawTerminal {
    original: libc::termios,
}

impl RawTerminal {
    fn enable() -> io::Result<Self> {
        // SAFETY: termios calls operate on the process-owned stdin descriptor.
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut original) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            let terminal = Self { original };
            io::stdout().write_all(b"\x1b[?25l\x1b[?1000h\x1b[?1006h")?;
            Ok(terminal)
        }
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        // SAFETY: original came from this process-owned stdin descriptor.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
        let _ = io::stdout().write_all(b"\x1b[0m\x1b[?1000l\x1b[?1006l\x1b[?25h");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_str;

    const CONFIG: &str = r#"
version = 1
default = "devbox"
[desktops.local]
mode = "local"
label = "Local"
[desktops.devbox]
mode = "remote"
label = "Devbox with a very long display label"
target = "dev"
session = "development"
keybindings = "local"
"#;

    fn rows() -> Vec<PickerRow> {
        let config = parse_str(CONFIG).expect("valid config");
        let live = [InstanceRecord {
            desktop_id: "local".into(),
            pid: 42,
            launch_date_unix_ms: 1,
            bundle_id: "com.gustavocaiano.herdr".into(),
            bundle_path: "/Applications/Herdr.app".into(),
            mode: "local".into(),
            target: None,
            session: None,
            keybindings: None,
        }];
        build_rows(&config, &live)
    }

    #[test]
    fn rows_put_default_first_and_use_registry_status_only() {
        let rows = rows();
        assert_eq!(rows[0].id, "devbox");
        assert_eq!(rows[0].kind, "Remote");
        assert_eq!(rows[0].detail, "dev / development");
        assert_eq!(rows[0].status, DesktopStatus::Unknown);
        assert_eq!(rows[1].id, "local");
        assert_eq!(rows[1].kind, "Local");
        assert_eq!(rows[1].status, DesktopStatus::Running);
    }

    #[test]
    fn truncation_and_rows_respect_terminal_cell_width() {
        assert_eq!(truncate_to_width("Desktop alpha", 8), "Desktop…");
        assert_eq!(truncate_to_width("界面 desktop", 6), "界面 …");
        for width in [20, 40, 64] {
            let text = render_row_text(&rows()[0], true, width, 20);
            assert!(UnicodeWidthStr::width(text.as_str()) <= width);
            if width >= 20 {
                assert!(text.contains("Unknown"));
            }
        }
    }

    #[test]
    fn navigation_wraps_and_keeps_errors_visible() {
        let mut state = PickerState::new(rows());
        state.apply_launch_error("launcher configuration is incomplete");
        let notice = state.notice.clone();
        state.move_up();
        assert_eq!(state.selected, 1);
        assert_eq!(state.notice, notice);
        state.move_down();
        assert_eq!(state.selected, 0);
        assert_eq!(state.notice, notice);
    }

    #[test]
    fn only_clear_remote_failures_transition_to_offline() {
        let mut state = PickerState::new(rows());
        state.apply_launch_error(
            "remote desktop target \"dev\" failed bounded non-interactive SSH preflight: connection refused",
        );
        assert_eq!(state.rows[0].status, DesktopStatus::Offline);
        assert!(matches!(state.notice, Some(Notice::Error { .. })));

        state.apply_launch_error("desktop launch helper is missing");
        assert_eq!(state.rows[0].status, DesktopStatus::Unknown);
        assert!(matches!(state.notice, Some(Notice::Error { .. })));
    }

    #[test]
    fn outcome_copy_is_truthful_and_marks_running() {
        let launched = LaunchOutcome::Launched { pid: 10 };
        let focused = LaunchOutcome::Focused { pid: 11 };
        assert_eq!(outcome_copy(&launched), "Launched");
        assert_eq!(outcome_copy(&focused), "Focused");
        assert_eq!(
            outcome_detail(&launched, "Devbox"),
            "Herdr client launched for Devbox."
        );
        assert_eq!(
            outcome_detail(&focused, "Devbox"),
            "Existing Herdr client focused for Devbox."
        );

        let mut state = PickerState::new(rows());
        state.apply_outcome(&focused);
        assert_eq!(state.rows[0].status, DesktopStatus::Running);
        assert!(matches!(
            state.notice,
            Some(Notice::Outcome { ref title, .. }) if title == "Focused"
        ));
    }

    #[test]
    fn error_copy_removes_terminal_controls_but_persists_the_message() {
        let mut state = PickerState::new(rows());
        state.apply_launch_error("Host key failed\n\x1b[31mcheck ~/.ssh/config");
        assert!(matches!(
            state.notice,
            Some(Notice::Error { ref detail, .. })
                if detail == "Host key failed [31mcheck ~/.ssh/config"
        ));
    }
}
