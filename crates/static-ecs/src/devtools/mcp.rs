//! A Model Context Protocol server over a running app, for agents.
//!
//! JSON-RPC 2.0, one message per line, over stdio, a unix socket, or an
//! in-process channel. Requests are served on the app's own thread, between
//! frames, by [`Session::pump`] (or [`Session::serve`] for an agent-driven
//! loop), so every tool sees a consistent world.
//!
//! Tools are designed for progressive disclosure: `overview` first, then
//! narrower tools; lists page with `offset`/`limit` and say how to go deeper;
//! entities are `index`v`generation` (`12v0`) and systems `#id`, stable for
//! the session.
//!
//! ```no_run
//! # use static_ecs::prelude::*;
//! # struct Update; impl Schedule for Update {}
//! let mut app = App::new().add_systems(Update, |_: Query<&u32>| {}).build();
//! let mut tools = static_ecs::devtools::mcp::Session::unix("/tmp/game.sock").unwrap();
//! tools.label("Update", Update);
//! loop {
//!     tools.pump(&mut app); // serves agent requests; blocks while paused
//!     app.emit(&Update);
//! }
//! ```

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use super::history::condition;
use super::snapshot::{Fork, Snapshot};
use super::value::{short_name, short_type};
use super::{Step, Writer};
use crate::dispatch::{Dispatch, Fits, FromEvent};
use crate::{App, Entity, Schedule, Storage, World};

pub use json::Json;

/// One incoming message and where its reply goes.
struct Incoming {
    line: String,
    reply: Box<dyn Fn(&str) + Send>,
}

type Action<S, L> = Box<dyn FnMut(&mut App<S, L>, LabelAction) -> String>;
enum LabelAction {
    Emit(u64),
    Step,
}

/// An MCP server session for one app.
pub struct Session<S: Storage, L> {
    incoming: Receiver<Incoming>,
    labels: Vec<(&'static str, Action<S, L>)>,
    paused: bool,
    snapshots: Vec<Snapshot<S>>,
    quit: bool,
}

/// An in-process client, for tests and embedding.
pub struct Client {
    outgoing: Sender<Incoming>,
    next_id: u64,
}
impl Client {
    /// Send one raw JSON-RPC message; the reply, if any, arrives once the
    /// session pumps.
    pub fn send(&self, line: &str) -> Receiver<String> {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let _ = self.outgoing.send(Incoming {
            line: line.to_string(),
            reply: Box::new(move |reply| {
                let _ = tx.lock().unwrap().send(reply.to_string());
            }),
        });
        rx
    }
    /// Call a tool; returns its reply receiver.
    pub fn call(&mut self, tool: &str, arguments: Json) -> Receiver<String> {
        self.next_id += 1;
        let message = Json::object([
            ("jsonrpc", Json::from("2.0")),
            ("id", Json::Num(self.next_id as f64)),
            ("method", Json::from("tools/call")),
            (
                "params",
                Json::object([("name", Json::from(tool)), ("arguments", arguments)]),
            ),
        ]);
        self.send(&message.to_string())
    }
}

impl<S: Storage, L> Session<S, L> {
    fn new(incoming: Receiver<Incoming>) -> Self {
        Self {
            incoming,
            labels: Vec::new(),
            paused: false,
            snapshots: Vec::new(),
            quit: false,
        }
    }
    /// Serve over this process's stdin and stdout. The app must not print
    /// to stdout meanwhile.
    pub fn stdio() -> Self {
        let (tx, rx) = channel();
        let out = Arc::new(Mutex::new(std::io::stdout()));
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let out = out.clone();
                let reply = Box::new(move |reply: &str| {
                    let mut out = out.lock().unwrap();
                    let _ = writeln!(out, "{reply}");
                    let _ = out.flush();
                });
                if tx.send(Incoming { line, reply }).is_err() {
                    break;
                }
            }
        });
        Self::new(rx)
    }
    /// Serve on a unix socket at `path`, one client at a time; bridge it to
    /// an agent's stdio with `nc -U` or the `static-ecs-mcp` tool.
    pub fn unix(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let _ = std::fs::remove_file(path);
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Ok(writer) = stream.try_clone() else {
                    continue;
                };
                let writer = Arc::new(Mutex::new(writer));
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let writer = writer.clone();
                    let reply = Box::new(move |reply: &str| {
                        let mut writer = writer.lock().unwrap();
                        let _ = writeln!(writer, "{reply}");
                        let _ = writer.flush();
                    });
                    if tx.send(Incoming { line, reply }).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(Self::new(rx))
    }
    /// A session and an in-process client.
    pub fn channel() -> (Self, Client) {
        let (tx, rx) = channel();
        (
            Self::new(rx),
            Client {
                outgoing: tx,
                next_id: 0,
            },
        )
    }
    /// Let agents emit and step label `E`, named `name`, with `event`.
    pub fn label<E: Schedule>(&mut self, name: &'static str, event: E) -> &mut Self
    where
        E::In: FromEvent<E>,
        L: Dispatch<E, World<S>> + Fits<World<S>>,
    {
        self.labels.push((
            name,
            Box::new(move |app: &mut App<S, L>, action| match action {
                LabelAction::Emit(count) => {
                    let before = app.world.tick();
                    for _ in 0..count {
                        app.emit(&event);
                    }
                    let changes = app.world.changes_since(before);
                    format!(
                        "emitted `{name}` {count}x; now frame {}; {} rows changed (see `changes`)",
                        app.world.history().frames().len(),
                        changes.len()
                    )
                }
                LabelAction::Step => {
                    let outcome = app.step(&event);
                    step_report(app, outcome)
                }
            }),
        ));
        self
    }
}

fn step_report<S: Storage, L>(app: &App<S, L>, outcome: super::StepOutcome) -> String {
    let mut out = String::new();
    match &outcome.step {
        Some(Step::System { id, name, ran }) => {
            let _ = write!(
                out,
                "step {}: system #{id} `{}` {}",
                outcome.index,
                short_type(name),
                if *ran {
                    "ran"
                } else {
                    "skipped (a parameter was unavailable)"
                }
            );
        }
        Some(Step::Condition { name, result }) => {
            let _ = write!(
                out,
                "step {}: condition `{}` = {result}{}",
                outcome.index,
                short_type(name),
                if *result {
                    ""
                } else {
                    " (guarded systems skipped)"
                }
            );
        }
        Some(Step::Task { id, name, polled }) => {
            let _ = write!(
                out,
                "step {}: task #{id} `{}` {}",
                outcome.index,
                short_type(name),
                if *polled { "polled" } else { "not due" }
            );
        }
        None => {
            let _ = write!(
                out,
                "pass finished: commands applied, frame {} recorded",
                app.world.history().frames().len().saturating_sub(1)
            );
        }
    }
    if !outcome.changes.is_empty() {
        let _ = write!(out, "\nchanged:");
        for change in outcome.changes.iter().take(20) {
            let _ = write!(
                out,
                "\n  {} {} {:?}",
                id(change.entity),
                short_name(change.component),
                change.kind
            );
        }
        if outcome.changes.len() > 20 {
            let _ = write!(
                out,
                "\n  … {} more (use `changes`)",
                outcome.changes.len() - 20
            );
        }
    }
    out
}

/// An entity as tools print it: `index`v`generation`.
pub fn id(entity: Entity) -> String {
    format!("{}v{}", entity.index(), entity.generation())
}
fn parse_entity<S: Storage>(world: &World<S>, text: &str) -> Result<Entity, String> {
    let (index, generation) = text
        .split_once('v')
        .ok_or_else(|| format!("`{text}` is not an entity id like `12v0`"))?;
    let (index, generation) = (
        index.parse().map_err(|_| format!("bad entity `{text}`"))?,
        generation
            .parse()
            .map_err(|_| format!("bad entity `{text}`"))?,
    );
    world
        .entity_at(index, generation)
        .ok_or_else(|| format!("entity {text} is not alive"))
}
fn writer(w: Writer) -> String {
    w.describe()
}

impl<S: Storage, L: Fits<World<S>>> Session<S, L> {
    /// Serve every queued request; while an agent has paused the app, keep
    /// serving (blocking) until it resumes or disconnects.
    pub fn pump(&mut self, app: &mut App<S, L>) {
        loop {
            let message = if self.paused {
                match self.incoming.recv() {
                    Ok(message) => message,
                    Err(_) => {
                        self.paused = false;
                        return;
                    }
                }
            } else {
                match self.incoming.try_recv() {
                    Ok(message) => message,
                    Err(_) => return,
                }
            };
            self.handle(app, message);
        }
    }
    /// Serve until the client calls `quit` or disconnects: the agent drives
    /// frames with `emit` and `step`.
    pub fn serve(&mut self, app: &mut App<S, L>) {
        while !self.quit {
            match self.incoming.recv() {
                Ok(message) => self.handle(app, message),
                Err(_) => return,
            }
        }
    }
    fn handle(&mut self, app: &mut App<S, L>, message: Incoming) {
        let request = match json::parse(&message.line) {
            Ok(request) => request,
            Err(error) => {
                (message.reply)(&rpc_error(
                    Json::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
                return;
            }
        };
        let rpc_id = request.get("id").cloned();
        let method = request.get("method").and_then(Json::as_str).unwrap_or("");
        let result = match method {
            "initialize" => {
                let version = request
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Json::as_str)
                    .unwrap_or("2025-06-18")
                    .to_string();
                Ok(Json::object([
                    ("protocolVersion", Json::from(version.as_str())),
                    (
                        "capabilities",
                        Json::object([("tools", Json::object::<&str, _>([]))]),
                    ),
                    (
                        "serverInfo",
                        Json::object([
                            ("name", Json::from("static-ecs")),
                            ("version", Json::from(env!("CARGO_PKG_VERSION"))),
                        ]),
                    ),
                    ("instructions", Json::from(INSTRUCTIONS)),
                ]))
            }
            "ping" => Ok(Json::object::<&str, _>([])),
            "tools/list" => Ok(Json::object([("tools", tools())])),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(Json::Null);
                let name = params
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Json::object::<&str, _>([]));
                let (text, error) = match self.call(app, &name, &arguments) {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                Ok(Json::object([
                    (
                        "content",
                        Json::Arr(vec![Json::object([
                            ("type", Json::from("text")),
                            ("text", Json::from(text.as_str())),
                        ])]),
                    ),
                    ("isError", Json::Bool(error)),
                ]))
            }
            _ if method.starts_with("notifications/") => return,
            _ => Err((-32601, format!("unknown method `{method}`"))),
        };
        let Some(rpc_id) = rpc_id else { return };
        let reply = match result {
            Ok(result) => Json::object([
                ("jsonrpc", Json::from("2.0")),
                ("id", rpc_id),
                ("result", result),
            ])
            .to_string(),
            Err((code, text)) => rpc_error(rpc_id, code, &text),
        };
        (message.reply)(&reply);
    }

    fn call(&mut self, app: &mut App<S, L>, tool: &str, args: &Json) -> Result<String, String> {
        let text = |key: &str| args.get(key).and_then(Json::as_str);
        let number =
            |key: &str, default: u64| args.get(key).and_then(Json::as_u64).unwrap_or(default);
        let required = |key: &str| text(key).ok_or_else(|| format!("missing argument `{key}`"));
        match tool {
            "overview" => Ok(self.overview(app)),
            "systems" => {
                let label = text("label");
                let full = text("detail") == Some("full");
                let (offset, limit) = (number("offset", 0) as usize, number("limit", 40) as usize);
                let systems: Vec<_> = app
                    .systems()
                    .into_iter()
                    .filter(|s| label.is_none_or(|l| short_name(s.label) == l || s.label == l))
                    .collect();
                let mut out = String::new();
                for s in systems.iter().skip(offset).take(limit) {
                    let kind = if s.hook {
                        "hook"
                    } else if s.condition {
                        "condition"
                    } else if s.exclusive {
                        "exclusive"
                    } else {
                        "system"
                    };
                    let _ = write!(
                        out,
                        "#{} {kind} `{}` under `{}`{}",
                        s.id,
                        short_type(s.name),
                        short_name(s.label),
                        if s.path.is_empty() {
                            String::new()
                        } else {
                            format!(" at {}", s.path)
                        }
                    );
                    if full {
                        let touches: Vec<_> = s
                            .touches
                            .iter()
                            .map(|t| {
                                format!(
                                    "{:?} {}{}",
                                    t.kind,
                                    short_type(t.component),
                                    if t.joined { " (join)" } else { "" }
                                )
                            })
                            .collect();
                        let _ = write!(
                            out,
                            "\n    registered {}:{}\n    touches: {}",
                            s.location.file(),
                            s.location.line(),
                            touches.join(", ")
                        );
                    }
                    out.push('\n');
                }
                let _ = write!(out, "{}", page(offset, limit, systems.len(), "systems"));
                if !full {
                    out.push_str("\nPass detail=\"full\" for registration sites and accesses.");
                }
                Ok(out)
            }
            "entities" => {
                let world = &app.world;
                let names = |key: &str| -> Result<Vec<usize>, String> {
                    match args.get(key) {
                        Some(Json::Arr(items)) => items
                            .iter()
                            .map(|n| {
                                let n = n.as_str().unwrap_or("");
                                world
                                    .component_index(n)
                                    .ok_or_else(|| format!("no component `{n}`"))
                            })
                            .collect(),
                        _ => Ok(Vec::new()),
                    }
                };
                let (with, without) = (names("with")?, names("without")?);
                let (offset, limit) = (number("offset", 0) as usize, number("limit", 20) as usize);
                let tables: Vec<_> = world
                    .tables
                    .iter()
                    .filter(|t| !t.resource && !t.entities.is_empty())
                    .filter(|t| {
                        with.iter().all(|&i| t.present[i]) && without.iter().all(|&i| !t.present[i])
                    })
                    .collect();
                let total: usize = tables.iter().map(|t| t.entities.len()).sum();
                let mut out = String::new();
                if with.is_empty() && without.is_empty() {
                    let _ = writeln!(
                        out,
                        "{total} entities in {} non-empty tables:",
                        tables.len()
                    );
                    let mut shapes: Vec<_> = tables
                        .iter()
                        .map(|t| (t.entities.len(), component_list(world, &t.present)))
                        .collect();
                    shapes.sort_by_key(|shape| std::cmp::Reverse(shape.0));
                    for (count, components) in shapes.iter().take(limit) {
                        let _ = writeln!(out, "  {count:>6} × ({components})");
                    }
                    out.push_str(
                        "Filter with with=[...]/without=[...] to list ids; `entity` shows one.",
                    );
                    return Ok(out);
                }
                let ids: Vec<_> = tables
                    .iter()
                    .flat_map(|t| t.entities.iter().map(move |&e| (e, *t)))
                    .skip(offset)
                    .take(limit)
                    .collect();
                for (entity, table) in ids {
                    let _ = writeln!(
                        out,
                        "{} ({})",
                        id(entity),
                        component_list(world, &table.present)
                    );
                }
                out.push_str(&page(offset, limit, total, "entities"));
                Ok(out)
            }
            "entity" => {
                let entity = parse_entity(&app.world, required("id")?)?;
                let mut out = format!("{}:\n", id(entity));
                for (name, value) in app.world.describe(entity).unwrap_or_default() {
                    let _ = writeln!(out, "  {} = {value}", short_name(name));
                }
                out.push_str("`timeline` shows its history; `explain` who wrote a component.");
                Ok(out)
            }
            "changes" => {
                let since = match args.get("since_frame").and_then(Json::as_u64) {
                    Some(frame) => app
                        .world
                        .history()
                        .frames()
                        .get(frame as usize)
                        .map_or(0, |f| f.first_tick.saturating_sub(1)),
                    None => number("since_tick", 0),
                };
                let (offset, limit) = (number("offset", 0) as usize, number("limit", 30) as usize);
                let changes = app.world.changes_since(since);
                let mut out = format!(
                    "live rows changed after tick {since} (now {}):\n",
                    app.world.tick()
                );
                for c in changes.iter().skip(offset).take(limit) {
                    let _ = writeln!(
                        out,
                        "  {} {} {:?} @{} by {}",
                        id(c.entity),
                        short_name(c.component),
                        c.kind,
                        c.tick,
                        writer(c.writer)
                    );
                }
                out.push_str(&page(offset, limit, changes.len(), "changes"));
                Ok(out)
            }
            "explain" => {
                let entity = parse_entity(&app.world, required("entity")?)?;
                let component = required("component")?;
                let depth = number("depth", 2) as usize;
                app.world
                    .explain(entity, component, depth)
                    .map(|e| e.render())
                    .ok_or_else(|| format!("{} has no `{component}`", id(entity)))
            }
            "timeline" => {
                let entity_text = required("entity")?;
                let (index, generation) = entity_text
                    .split_once('v')
                    .and_then(|(i, g)| Some((i.parse::<u32>().ok()?, g.parse::<u32>().ok()?)))
                    .ok_or_else(|| format!("`{entity_text}` is not an entity id like `12v0`"))?;
                let (offset, limit) = (number("offset", 0) as usize, number("limit", 30) as usize);
                let timeline: Vec<_> = app
                    .world
                    .history()
                    .structural()
                    .filter(|s| s.entity.index() == index && s.entity.generation() == generation)
                    .map(|s| {
                        (
                            s.tick,
                            s.frame,
                            format!(
                                "{:?} {}",
                                s.kind,
                                s.components
                                    .iter()
                                    .map(|c| short_name(c))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            s.writer,
                        )
                    })
                    .chain(
                        app.world
                            .history()
                            .records()
                            .filter(|r| {
                                r.entity.index() == index && r.entity.generation() == generation
                            })
                            .map(|r| {
                                (
                                    r.tick,
                                    r.frame,
                                    format!("{} = {}", short_name(r.component), r.value),
                                    r.writer,
                                )
                            }),
                    )
                    .collect();
                let mut timeline = timeline;
                timeline.sort_by_key(|e| e.0);
                let mut out = format!("{entity_text}:\n");
                for (tick, frame, what, w) in timeline.iter().skip(offset).take(limit) {
                    let _ = writeln!(out, "  frame {frame} @{tick}: {what}  by {}", writer(*w));
                }
                out.push_str(&page(offset, limit, timeline.len(), "events"));
                Ok(out)
            }
            "history" => {
                let component = required("component")?;
                let entity = text("entity")
                    .map(|e| parse_entity(&app.world, e))
                    .transpose()?;
                let history = app.world.history();
                match text("mode").unwrap_or("first") {
                    "first" => {
                        let test = condition(required("condition")?)?;
                        Ok(match history.first(entity, component, &*test) {
                            Some(r) => format!(
                                "first at frame {} (tick {}): {} {} = {} by {}",
                                r.frame,
                                r.tick,
                                id(r.entity),
                                short_name(r.component),
                                r.value,
                                writer(r.writer)
                            ),
                            None => "never (in recorded history)".to_string(),
                        })
                    }
                    "when" => {
                        let test = condition(required("condition")?)?;
                        let runs = history.when(entity, component, &*test);
                        let mut out = format!("{} runs of frames where it held:\n", runs.len());
                        for (e, first, last) in runs.iter().take(number("limit", 30) as usize) {
                            let _ = writeln!(out, "  {} frames {first}..={last}", id(*e));
                        }
                        Ok(out)
                    }
                    "values" => {
                        let entity = entity.ok_or("mode=values needs `entity`")?;
                        let mut out = String::new();
                        for r in history.records_of(entity, component) {
                            let _ = writeln!(
                                out,
                                "  frame {} @{}: {} by {}",
                                r.frame,
                                r.tick,
                                r.value,
                                writer(r.writer)
                            );
                        }
                        Ok(if out.is_empty() {
                            "no records".to_string()
                        } else {
                            out
                        })
                    }
                    "respond" => {
                        let trigger = condition(required("condition")?)?;
                        let then_component = text("then_component").unwrap_or(component);
                        let then = condition(required("then")?)?;
                        let within = number("within", 1);
                        let misses = history.violations_of_response(
                            component,
                            &*trigger,
                            then_component,
                            &*then,
                            within,
                        );
                        let mut out = format!("{} violations:\n", misses.len());
                        for (e, frame) in misses.iter().take(30) {
                            let _ = writeln!(
                                out,
                                "  {} triggered at frame {frame}, no response within {within}",
                                id(*e)
                            );
                        }
                        Ok(out)
                    }
                    other => Err(format!(
                        "unknown mode `{other}` (first, when, values, respond)"
                    )),
                }
            }
            "why_not" => {
                let system = required("system")?
                    .trim_start_matches('#')
                    .parse()
                    .map_err(|_| "bad system id".to_string())?;
                let entity = parse_entity(&app.world, required("entity")?)?;
                let reasons = app
                    .why_not(system, entity)
                    .ok_or_else(|| format!("no system #{system}"))?;
                Ok(if reasons.is_empty() {
                    format!("{} is a row of every query of #{system}", id(entity))
                } else {
                    reasons.join("\n")
                })
            }
            "why_component" => {
                let component = required("component")?;
                app.why_component(component)
                    .map(|r| r.join("\n"))
                    .ok_or_else(|| format!("no component `{component}` in this world"))
            }
            "why_order" => {
                let parse = |key: &str| -> Result<u32, String> {
                    required(key)?
                        .trim_start_matches('#')
                        .parse()
                        .map_err(|_| format!("bad system id in `{key}`"))
                };
                let (a, b) = (parse("first")?, parse("second")?);
                app.why_order(a, b)
                    .ok_or_else(|| "unknown system".to_string())
            }
            "tasks" => {
                let tasks = app.tasks();
                let mut out = format!("{} tasks:\n", tasks.len());
                for t in &tasks {
                    let stuck =
                        matches!(t.state, super::TaskState::WaitingFor(_)) && t.idle_frames >= 60;
                    let _ = writeln!(
                        out,
                        "  #{} `{}` ({}:{}): {:?}; {} polls, {} wakes, idle {} labels{}{}",
                        t.id,
                        short_type(t.name),
                        t.location.file(),
                        t.location.line(),
                        t.state,
                        t.polls,
                        t.wakes,
                        t.idle_frames,
                        if t.woken { ", woken" } else { "" },
                        if stuck {
                            "  ← waiting long: is that label ever emitted?"
                        } else {
                            ""
                        }
                    );
                }
                Ok(out)
            }
            "emit" | "step" => {
                let name = required("label")?;
                let count = number("count", 1);
                let (_, action) = self
                    .labels
                    .iter_mut()
                    .find(|(n, _)| *n == name)
                    .ok_or_else(|| format!("label `{name}` is not registered with the session"))?;
                Ok(action(
                    app,
                    if tool == "emit" {
                        LabelAction::Emit(count)
                    } else {
                        LabelAction::Step
                    },
                ))
            }
            "pause" => {
                self.paused = true;
                Ok("paused: the app's loop waits in `pump` until `resume`".to_string())
            }
            "resume" => {
                self.paused = false;
                Ok("resumed".to_string())
            }
            "quit" => {
                self.quit = true;
                self.paused = false;
                Ok("bye".to_string())
            }
            "set" => {
                let entity = parse_entity(&app.world, required("entity")?)?;
                let (component, field, value) = (
                    required("component")?,
                    text("field").unwrap_or(""),
                    required("value")?,
                );
                match app.world.set_field(entity, component, field, value) {
                    Ok(v) => Ok(format!("{}.{component}.{field} = {v}", id(entity))),
                    Err(e) => Err(format!(
                        "{e}\neditable fields: {}",
                        app.world
                            .editable_fields(component)
                            .unwrap_or_default()
                            .join(", ")
                    )),
                }
            }
            "snapshot" => match text("action").unwrap_or("take") {
                "take" => {
                    let snapshot = app.snapshot().map_err(|e| e.to_string())?;
                    self.snapshots.push(snapshot);
                    Ok(format!(
                        "snapshot {} taken at frame {}",
                        self.snapshots.len() - 1,
                        self.snapshots.last().unwrap().frame
                    ))
                }
                "list" => {
                    let mut out = String::new();
                    for (i, s) in self.snapshots.iter().enumerate() {
                        let _ = writeln!(out, "  snapshot {i}: frame {}, tick {}", s.frame, s.tick);
                    }
                    let _ = write!(
                        out,
                        "automatic snapshots at frames {:?}",
                        app.snapshot_frames()
                    );
                    Ok(out)
                }
                "restore" => {
                    let index = number("id", u64::MAX) as usize;
                    let snapshot = self.snapshots.get(index).ok_or("no such snapshot")?;
                    app.world.restore(snapshot).map_err(|e| e.to_string())?;
                    Ok(format!(
                        "restored snapshot {index} (frame {})",
                        snapshot.frame
                    ))
                }
                "auto" => {
                    let (every, keep) = (number("every", 1), number("keep", 64) as usize);
                    app.set_auto_snapshots(every, keep);
                    Ok(format!("snapshotting every {every} frames, keeping {keep}"))
                }
                other => Err(format!(
                    "unknown action `{other}` (take, list, restore, auto)"
                )),
            },
            "rewind" => {
                let frame = number("frame", 0);
                let at = app.rewind(frame).map_err(|e| e.to_string())?;
                Ok(format!(
                    "rewound to the snapshot at frame {at}; emit or step to run forward"
                ))
            }
            "what_if" => self.what_if(app, args),
            "asm" => {
                let system = required("system")?;
                let binary = text("binary")
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::env::current_exe().ok())
                    .ok_or("no binary")?;
                match text("baseline") {
                    Some(baseline) => {
                        super::asm::diff(std::path::Path::new(baseline), &binary, system)
                    }
                    None => super::asm::system(&binary, system).map(|code| code.text()),
                }
            }
            other => Err(format!("unknown tool `{other}`; call tools/list")),
        }
    }

    fn overview(&self, app: &mut App<S, L>) -> String {
        let world = &app.world;
        let entities: usize = world
            .tables
            .iter()
            .filter(|t| !t.resource)
            .map(|t| t.entities.len())
            .sum();
        let tables = world
            .tables
            .iter()
            .filter(|t| !t.entities.is_empty())
            .count();
        let systems = app.systems();
        let tasks = app.tasks();
        let history = app.world.history();
        let labels: Vec<_> = self.labels.iter().map(|(n, _)| *n).collect();
        format!(
            "frame {} (tick {}), {}\n{entities} entities in {tables} tables; {} component types\n{} systems ({} hooks), {} tasks\n{} value records ({} dropped); auto snapshots at {:?}\nlabels you can emit/step: {}\nNext: `systems`, `entities`, `changes`, `tasks`; then `entity`, `explain`, `timeline`, `history`, `why_not`.",
            history.frames().len(),
            app.world.tick(),
            if self.paused { "paused" } else { "running" },
            app.world.component_names().len(),
            systems.len(),
            systems.iter().filter(|s| s.hook).count(),
            tasks.len(),
            history.records().count(),
            history.dropped(),
            app.snapshot_frames(),
            if labels.is_empty() {
                "(none registered)".to_string()
            } else {
                labels.join(", ")
            },
        )
    }

    /// Run a label for some frames in two forks, one with edits, and diff
    /// them: the effect of the edits alone.
    fn what_if(&mut self, app: &mut App<S, L>, args: &Json) -> Result<String, String> {
        let label = args
            .get("label")
            .and_then(Json::as_str)
            .ok_or("missing `label`")?
            .to_string();
        let frames = args.get("frames").and_then(Json::as_u64).unwrap_or(1);
        let edits: Vec<(Entity, String, String, String)> = match args.get("edits") {
            Some(Json::Arr(edits)) => edits
                .iter()
                .map(|edit| {
                    let field =
                        |k: &str| edit.get(k).and_then(Json::as_str).unwrap_or("").to_string();
                    Ok((
                        parse_entity(&app.world, &field("entity"))?,
                        field("component"),
                        field("field"),
                        field("value"),
                    ))
                })
                .collect::<Result<_, String>>()?,
            _ => Vec::new(),
        };
        let (_, action) = self
            .labels
            .iter_mut()
            .find(|(n, _)| *n == label)
            .ok_or_else(|| format!("label `{label}` is not registered with the session"))?;
        let mut run = |app: &mut App<S, L>, fork: &mut Fork<S>, edit: bool| -> Result<(), String> {
            app.in_fork(fork, |app| {
                if edit {
                    for (entity, component, field, value) in &edits {
                        app.world.set_field(*entity, component, field, value)?;
                    }
                }
                action(app, LabelAction::Emit(frames));
                Ok(())
            })
        };
        let mut base = app.fork().map_err(|e| e.to_string())?;
        let mut edited = app.fork().map_err(|e| e.to_string())?;
        run(app, &mut base, false)?;
        run(app, &mut edited, true)?;
        let diff = base.world().diff(edited.world());
        let mut out = format!(
            "after {frames} frame(s) of `{label}`, with the edits vs without ({} spawned, {} despawned, {} values differ):\n",
            diff.spawned.len(),
            diff.despawned.len(),
            diff.changed.len()
        );
        for e in diff.spawned.iter().take(10) {
            let _ = writeln!(out, "  only with edits: {}", id(*e));
        }
        for e in diff.despawned.iter().take(10) {
            let _ = writeln!(out, "  only without edits: {}", id(*e));
        }
        for (e, component, without, with) in diff.changed.iter().take(30) {
            let show =
                |v: &Option<super::Value>| v.as_ref().map_or("—".to_string(), |v| v.to_string());
            let _ = writeln!(
                out,
                "  {} {}: {} → {}",
                id(*e),
                short_name(component),
                show(without),
                show(with)
            );
        }
        out.push_str("The app's own world is untouched.");
        Ok(out)
    }
}

fn component_list<S: Storage>(world: &World<S>, present: &[bool]) -> String {
    world
        .component_names()
        .iter()
        .zip(present)
        .filter(|(_, p)| **p)
        .map(|(n, _)| short_name(n).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
fn page(offset: usize, limit: usize, total: usize, what: &str) -> String {
    let end = (offset + limit).min(total);
    if end < total {
        format!(
            "showing {what} {}..{end} of {total}; pass offset={end} for more",
            offset
        )
    } else {
        format!("{total} {what}")
    }
}
fn rpc_error(id: Json, code: i64, message: &str) -> String {
    Json::object([
        ("jsonrpc", Json::from("2.0")),
        ("id", id),
        (
            "error",
            Json::object([
                ("code", Json::Num(code as f64)),
                ("message", Json::from(message)),
            ]),
        ),
    ])
    .to_string()
}

const INSTRUCTIONS: &str = "A live static-ecs app. Start with `overview`. Entities are `12v0`, systems `#3`. \
Lists page with offset/limit. `changes` and `explain` say who wrote what (system, commands, or a World call with file:line). \
`history` answers questions over recorded frames (conditions like `x < 3 && y > 0`, `== Idle`). `pause` holds the app between frames; \
`step` runs one system; `snapshot`/`rewind` travel in time; `what_if` runs edits in a fork and diffs.";

fn tool(
    name: &str,
    description: &str,
    properties: &[(&str, &str, &str)],
    required: &[&str],
) -> Json {
    Json::object([
        ("name", Json::from(name)),
        ("description", Json::from(description)),
        (
            "inputSchema",
            Json::object([
                ("type", Json::from("object")),
                (
                    "properties",
                    Json::Obj(
                        properties
                            .iter()
                            .map(|&(key, kind, about)| {
                                let schema = if kind == "array" {
                                    Json::object([
                                        ("type", Json::from("array")),
                                        ("items", Json::object([("type", Json::from("string"))])),
                                        ("description", Json::from(about)),
                                    ])
                                } else if kind == "edits" {
                                    Json::object([
                                        ("type", Json::from("array")),
                                        ("description", Json::from(about)),
                                        (
                                            "items",
                                            Json::object([
                                                ("type", Json::from("object")),
                                                (
                                                    "properties",
                                                    Json::object([
                                                        (
                                                            "entity",
                                                            Json::object([(
                                                                "type",
                                                                Json::from("string"),
                                                            )]),
                                                        ),
                                                        (
                                                            "component",
                                                            Json::object([(
                                                                "type",
                                                                Json::from("string"),
                                                            )]),
                                                        ),
                                                        (
                                                            "field",
                                                            Json::object([(
                                                                "type",
                                                                Json::from("string"),
                                                            )]),
                                                        ),
                                                        (
                                                            "value",
                                                            Json::object([(
                                                                "type",
                                                                Json::from("string"),
                                                            )]),
                                                        ),
                                                    ]),
                                                ),
                                            ]),
                                        ),
                                    ])
                                } else {
                                    Json::object([
                                        ("type", Json::from(kind)),
                                        ("description", Json::from(about)),
                                    ])
                                };
                                (key.to_string(), schema)
                            })
                            .collect(),
                    ),
                ),
                (
                    "required",
                    Json::Arr(required.iter().map(|&r| Json::from(r)).collect()),
                ),
            ]),
        ),
    ])
}

fn tools() -> Json {
    const OFFSET: (&str, &str, &str) = ("offset", "integer", "skip this many items");
    const LIMIT: (&str, &str, &str) = ("limit", "integer", "at most this many items");
    Json::Arr(vec![
        tool(
            "overview",
            "Counts, current frame, registered labels, and where to go next. Call first.",
            &[],
            &[],
        ),
        tool(
            "systems",
            "Systems and hooks in dispatch order: #id, name, label, position in its registration.",
            &[
                ("label", "string", "only this label"),
                (
                    "detail",
                    "string",
                    "`full` adds registration sites and accesses",
                ),
                OFFSET,
                LIMIT,
            ],
            &[],
        ),
        tool(
            "entities",
            "Without filters: table shapes and counts. With `with`/`without` component names: entity ids.",
            &[
                ("with", "array", "components every entity has"),
                ("without", "array", "components none has"),
                OFFSET,
                LIMIT,
            ],
            &[],
        ),
        tool(
            "entity",
            "Every component of one entity, as values.",
            &[("id", "string", "entity id like 12v0")],
            &["id"],
        ),
        tool(
            "changes",
            "Live rows added or changed after a tick or frame, each with who wrote it.",
            &[
                ("since_tick", "integer", "exclusive"),
                ("since_frame", "integer", "changes from this frame on"),
                OFFSET,
                LIMIT,
            ],
            &[],
        ),
        tool(
            "explain",
            "Provenance: who wrote a component's value, what that system reads, and who wrote those, back `depth` levels.",
            &[
                ("entity", "string", "entity id"),
                ("component", "string", "component name"),
                ("depth", "integer", "default 2"),
            ],
            &["entity", "component"],
        ),
        tool(
            "timeline",
            "An entity's structural events and recorded values, oldest first (works for despawned entities).",
            &[("entity", "string", "entity id"), OFFSET, LIMIT],
            &["entity"],
        ),
        tool(
            "history",
            "Questions over recorded frames. mode=first: first frame a condition held; when: runs of frames; values: an entity's recorded values; respond: violations of `whenever condition, then within N frames`.",
            &[
                ("component", "string", "component name"),
                (
                    "condition",
                    "string",
                    "e.g. `< 10`, `x > 3 && y < 0`, `== Idle`, `contains foo`",
                ),
                ("entity", "string", "restrict to one entity"),
                ("mode", "string", "first | when | values | respond"),
                (
                    "then_component",
                    "string",
                    "respond: component of the response",
                ),
                ("then", "string", "respond: the response condition"),
                ("within", "integer", "respond: frames allowed"),
                LIMIT,
            ],
            &["component"],
        ),
        tool(
            "why_not",
            "Why an entity is not a row of a system's queries: missing components, filters, unmentioned Disabling components, change windows.",
            &[
                ("system", "string", "system id like #3"),
                ("entity", "string", "entity id"),
            ],
            &["system", "entity"],
        ),
        tool(
            "why_component",
            "Why the world stores a component: systems using it, components requiring it, builder calls.",
            &[("component", "string", "component name")],
            &["component"],
        ),
        tool(
            "why_order",
            "Why one system runs before another (chain, parallel group, graph, registration order, labels).",
            &[
                ("first", "string", "system id"),
                ("second", "string", "system id"),
            ],
            &["first", "second"],
        ),
        tool(
            "tasks",
            "Task console: each task's state, awaited label, polls, wakes, idle time.",
            &[],
            &[],
        ),
        tool(
            "emit",
            "Run a registered label's whole frame `count` times.",
            &[
                ("label", "string", "label name"),
                ("count", "integer", "default 1"),
            ],
            &["label"],
        ),
        tool(
            "step",
            "Run the next single system, condition, or task of a label's pass; reports what it changed.",
            &[("label", "string", "label name")],
            &["label"],
        ),
        tool(
            "pause",
            "Hold the app between frames (its loop blocks in `pump`) until `resume`.",
            &[],
            &[],
        ),
        tool("resume", "Let the app's loop run again.", &[], &[]),
        tool("quit", "End `serve`.", &[], &[]),
        tool(
            "set",
            "Write a scalar field of a component (like a debugger writes memory). Empty field writes a scalar component.",
            &[
                ("entity", "string", "entity id"),
                ("component", "string", "component name"),
                ("field", "string", "path like `pos.x` or `0`"),
                ("value", "string", "literal"),
            ],
            &["entity", "component", "value"],
        ),
        tool(
            "snapshot",
            "Time travel. action=take|list|restore|auto (auto: snapshot every `every` frames, keep `keep`).",
            &[
                ("action", "string", "take | list | restore | auto"),
                ("id", "integer", "restore: snapshot id"),
                ("every", "integer", "auto"),
                ("keep", "integer", "auto"),
            ],
            &[],
        ),
        tool(
            "rewind",
            "Restore the latest automatic snapshot at or before `frame`.",
            &[("frame", "integer", "frame to rewind to")],
            &["frame"],
        ),
        tool(
            "what_if",
            "Counterfactual: run `label` for `frames` in a fork with edits and in one without, and diff them. The app is untouched.",
            &[
                ("label", "string", "label name"),
                ("frames", "integer", "default 1"),
                ("edits", "edits", "fields to set first"),
            ],
            &["label"],
        ),
        tool(
            "asm",
            "A system's machine code from a binary built with line tables (CARGO_PROFILE_RELEASE_DEBUG=line-tables-only); with `baseline`, a diff against another build.",
            &[
                (
                    "system",
                    "string",
                    "function path, or file:line of its body",
                ),
                ("binary", "string", "default: this executable"),
                ("baseline", "string", "binary to diff against"),
            ],
            &["system"],
        ),
    ])
}

/// Minimal JSON: enough for JSON-RPC.
pub mod json {
    use std::fmt;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }
    impl From<&str> for Json {
        fn from(s: &str) -> Self {
            Json::Str(s.to_string())
        }
    }
    impl Json {
        pub fn object<K: Into<String>, const N: usize>(entries: [(K, Json); N]) -> Json {
            Json::Obj(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
        }
        pub fn get(&self, key: &str) -> Option<&Json> {
            match self {
                Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Json::Str(s) => Some(s),
                _ => None,
            }
        }
        pub fn as_u64(&self) -> Option<u64> {
            match self {
                Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
                Json::Str(s) => s.parse().ok(),
                _ => None,
            }
        }
    }
    impl fmt::Display for Json {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Json::Null => f.write_str("null"),
                Json::Bool(b) => write!(f, "{b}"),
                Json::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
                Json::Num(n) => write!(f, "{n}"),
                Json::Str(s) => {
                    f.write_str("\"")?;
                    for c in s.chars() {
                        match c {
                            '"' => f.write_str("\\\"")?,
                            '\\' => f.write_str("\\\\")?,
                            '\n' => f.write_str("\\n")?,
                            '\r' => f.write_str("\\r")?,
                            '\t' => f.write_str("\\t")?,
                            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                            c => write!(f, "{c}")?,
                        }
                    }
                    f.write_str("\"")
                }
                Json::Arr(items) => {
                    f.write_str("[")?;
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            f.write_str(",")?;
                        }
                        write!(f, "{item}")?;
                    }
                    f.write_str("]")
                }
                Json::Obj(entries) => {
                    f.write_str("{")?;
                    for (i, (k, v)) in entries.iter().enumerate() {
                        if i > 0 {
                            f.write_str(",")?;
                        }
                        write!(f, "{}:{v}", Json::Str(k.clone()))?;
                    }
                    f.write_str("}")
                }
            }
        }
    }
    /// Parse one JSON document.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("trailing characters at {}", p.i));
        }
        Ok(v)
    }
    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl Parser<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn expect(&mut self, word: &str) -> Result<(), String> {
            if self.s[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                Ok(())
            } else {
                Err(format!("expected `{word}` at {}", self.i))
            }
        }
        fn value(&mut self) -> Result<Json, String> {
            self.ws();
            match self.s.get(self.i).copied().ok_or("unexpected end")? {
                b'n' => self.expect("null").map(|_| Json::Null),
                b't' => self.expect("true").map(|_| Json::Bool(true)),
                b'f' => self.expect("false").map(|_| Json::Bool(false)),
                b'"' => self.string().map(Json::Str),
                b'[' => {
                    self.i += 1;
                    let mut items = Vec::new();
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Json::Arr(items));
                    }
                    loop {
                        items.push(self.value()?);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                return Ok(Json::Arr(items));
                            }
                            _ => return Err(format!("expected , or ] at {}", self.i)),
                        }
                    }
                }
                b'{' => {
                    self.i += 1;
                    let mut entries = Vec::new();
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Json::Obj(entries));
                    }
                    loop {
                        self.ws();
                        let key = self.string()?;
                        self.ws();
                        self.expect(":")?;
                        entries.push((key, self.value()?));
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(Json::Obj(entries));
                            }
                            _ => return Err(format!("expected , or }} at {}", self.i)),
                        }
                    }
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len()
                        && matches!(
                            self.s[self.i],
                            b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                        )
                    {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.s[start..self.i])
                        .ok()
                        .and_then(|n| n.parse().ok())
                        .map(Json::Num)
                        .ok_or_else(|| format!("bad value at {start}"))
                }
            }
        }
        fn string(&mut self) -> Result<String, String> {
            if self.s.get(self.i) != Some(&b'"') {
                return Err(format!("expected string at {}", self.i));
            }
            self.i += 1;
            let mut out = Vec::new();
            loop {
                let b = *self.s.get(self.i).ok_or("unterminated string")?;
                self.i += 1;
                match b {
                    b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                    b'\\' => {
                        let e = *self.s.get(self.i).ok_or("bad escape")?;
                        self.i += 1;
                        let c = match e {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'u' => {
                                let hex = std::str::from_utf8(
                                    self.s.get(self.i..self.i + 4).ok_or("bad escape")?,
                                )
                                .map_err(|e| e.to_string())?;
                                self.i += 4;
                                let mut code =
                                    u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                // Surrogate pairs.
                                if (0xD800..0xDC00).contains(&code)
                                    && self.s[self.i..].starts_with(b"\\u")
                                {
                                    let low = std::str::from_utf8(&self.s[self.i + 2..self.i + 6])
                                        .map_err(|e| e.to_string())?;
                                    let low =
                                        u32::from_str_radix(low, 16).map_err(|e| e.to_string())?;
                                    self.i += 6;
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                }
                                char::from_u32(code).unwrap_or('\u{fffd}')
                            }
                            other => other as char,
                        };
                        let mut buf = [0; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                    _ => out.push(b),
                }
            }
        }
    }
}
