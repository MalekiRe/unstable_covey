#![cfg(feature = "devtools")]

use static_ecs::devtools::{
    self, ChangeKind, Step, StructuralKind, TaskState, TimelineEntry, Value, Writer, condition,
};
use static_ecs::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct Hp(i32);
#[derive(Clone, Debug, PartialEq)]
struct Damage(i32);
/// No `Debug`: described and edited through reflection.
#[derive(Clone)]
struct Armor {
    value: u8,
    tough: bool,
}
struct Update;
impl Schedule for Update {}

fn ramp(mut damage: Query<&mut Damage>) {
    damage.iter_mut().for_each(|mut d| d.0 += 1);
}
fn apply_damage(mut targets: Query<(&Damage, &mut Hp)>) {
    targets.iter_mut().for_each(|(d, mut hp)| hp.0 -= d.0);
}

fn id_of(systems: &[devtools::SystemInfo], name: &str) -> u32 {
    systems
        .iter()
        .find(|s| s.name.ends_with(name))
        .unwrap_or_else(|| panic!("no system named {name}"))
        .id
}

#[test]
fn changes_report_who_wrote_them_with_source_locations() {
    let mut app = App::new()
        .add_systems(Update, (ramp, apply_damage).chain())
        .add_systems(
            Update,
            |mut hp: Query<(Entity, &Hp)>, mut commands: Commands<_>| {
                for (entity, hp) in hp.iter_mut() {
                    if hp.0 <= 0 {
                        commands.despawn(entity);
                    }
                }
            },
        )
        .build();
    let systems = app.systems();
    let (ramp_id, apply_id) = (id_of(&systems, "ramp"), id_of(&systems, "apply_damage"));
    let spawned_at = line!() + 1;
    let e = app.world.spawn_bundle((Hp(3), Damage(1)));
    let spawn = app.world.changes_since(0);
    assert!(
        spawn
            .iter()
            .all(|c| c.entity == e && c.kind == ChangeKind::Added)
    );
    assert!(
        matches!(spawn[0].writer, Writer::World(at) if at.line() == spawned_at && at.file().ends_with("devtools.rs")),
        "{:?}",
        spawn[0].writer
    );

    let before = app.world.tick();
    app.emit(&Update);
    let changes = app.world.changes_since(before);
    let writer_of = |name: &str| {
        changes
            .iter()
            .find(|c| c.component.ends_with(name))
            .map(|c| c.writer)
    };
    assert_eq!(writer_of("Damage"), Some(Writer::System(ramp_id)));
    assert_eq!(writer_of("Hp"), Some(Writer::System(apply_id)));
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(1)));

    // The despawn is attributed to the commands of the system that queued it.
    app.emit(&Update);
    assert!(!app.world.contains(e));
    let timeline = app.world.timeline(e);
    let despawned = timeline
        .iter()
        .find_map(|entry| match entry {
            TimelineEntry::Structural(s) if s.kind == StructuralKind::Despawned => Some(s),
            _ => None,
        })
        .expect("a despawn event");
    let Writer::Commands { system, at } = despawned.writer else {
        panic!("despawn by {:?}", despawned.writer)
    };
    assert!(
        devtools::system(system)
            .unwrap()
            .name
            .contains("{{closure}}")
    );
    assert!(at.file().ends_with("devtools.rs"));
    assert!(
        matches!(&timeline[0], TimelineEntry::Structural(s) if s.kind == StructuralKind::Spawned)
    );
}

#[test]
fn history_answers_questions_over_frames_and_explains_values() {
    let mut app = App::new()
        .add_systems(Update, (ramp, apply_damage).chain())
        .build();
    let e = app.world.spawn_bundle((Hp(20), Damage(0)));
    for _ in 0..5 {
        app.emit(&Update);
    }
    // Damage ramps 1..=5, so Hp is 19, 17, 14, 10, 5 after frames 0..=4.
    let history = app.world.history();
    assert_eq!(history.frames().len(), 5);
    let hp_at = |frame| history.value_at(e, "Hp", frame).cloned();
    assert_eq!(
        hp_at(0),
        Some(Value::Tuple {
            name: "Hp".into(),
            items: vec![Value::Int(19)]
        })
    );
    assert_eq!(
        hp_at(3),
        Some(Value::Tuple {
            name: "Hp".into(),
            items: vec![Value::Int(10)]
        })
    );
    let below = condition("< 12").unwrap();
    assert_eq!(history.first(Some(e), "Hp", &below).unwrap().frame, 3);
    assert_eq!(history.when(Some(e), "Hp", &below), vec![(e, 3, 4)]);
    let between = condition("0 >= 10 && 0 <= 17").unwrap();
    assert_eq!(history.when(Some(e), "Hp", &between), vec![(e, 1, 3)]);

    // Hp was written by apply_damage, which read Damage, written by ramp.
    let why = app.world.explain(e, "Hp", 2).unwrap();
    assert_eq!(
        why.writer.system().and_then(devtools::system).unwrap().name,
        std::any::type_name_of_val(&apply_damage)
    );
    let input = &why.inputs[0];
    assert!(input.component.ends_with("Damage"));
    assert_eq!(
        input
            .writer
            .system()
            .and_then(devtools::system)
            .unwrap()
            .name,
        std::any::type_name_of_val(&ramp)
    );
    assert!(why.render().contains("reads:"));
}

#[test]
fn stepping_runs_one_system_or_condition_at_a_time() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                ramp,
                apply_damage.run_if(|mut hp: Query<&Hp>| hp.iter_mut().any(|hp| hp.0 > 0)),
            )
                .chain(),
        )
        .build();
    let e = app.world.spawn_bundle((Hp(20), Damage(0)));
    let first = app.step(&Update);
    assert!(matches!(first.step, Some(Step::System { ran: true, .. })));
    assert_eq!(app.world.get::<Damage>(e), Some(&Damage(1)));
    assert_eq!(
        app.world.get::<Hp>(e),
        Some(&Hp(20)),
        "apply_damage has not run yet"
    );
    assert!(
        first
            .changes
            .iter()
            .any(|c| c.component.ends_with("Damage"))
    );
    let guard = app.step(&Update);
    assert!(matches!(
        guard.step,
        Some(Step::Condition { result: true, .. })
    ));
    let applied = app.step(&Update);
    assert!(matches!(applied.step, Some(Step::System { ran: true, .. })));
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(19)));
    let end = app.step(&Update);
    assert!(end.finished && end.step.is_none());
    assert_eq!(app.world.history().frames().len(), 1);
    // A new pass starts over.
    assert_eq!(app.step(&Update).index, 0);
}

#[derive(Debug)]
struct Dead;
#[test]
fn why_not_names_the_mismatch_of_each_query() {
    let mut app = App::new()
        .add_systems(
            Update,
            |_: Query<&Hp, Without<Dead>>, _: Query<&Damage, Changed<Damage>>| {},
        )
        .register::<Disabled>()
        .build();
    let id = id_of(&app.systems(), "{{closure}}");
    let e = app.world.spawn_bundle((Hp(1), Dead));
    let reasons = app.why_not(id, e).unwrap();
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(
        reasons[0].contains("Without<Dead>") && reasons[0].contains("has `Dead`"),
        "{}",
        reasons[0]
    );
    assert!(reasons[1].contains("lacks `Damage`"), "{}", reasons[1]);

    let f = app.world.spawn_bundle((Hp(1), Damage(1), Disabled));
    let reasons = app.why_not(id, f).unwrap();
    assert!(
        reasons[0].contains("`Disabled`") && reasons[0].contains("MayHave<Disabled>"),
        "{reasons:?}"
    );

    let g = app.world.spawn_bundle((Hp(1), Damage(1)));
    app.emit(&Update);
    let reasons = app.why_not(id, g).unwrap();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(
        reasons[0].contains("Changed<Damage>") && reasons[0].contains("changed at"),
        "{}",
        reasons[0]
    );
}

struct Later;
impl Schedule for Later {}
#[test]
fn task_console_shows_what_each_task_waits_for() {
    let mut app = App::new()
        .register::<Hp>()
        .add_plugins(task(|world: AsyncWorld<_>| async move {
            world.bridge(Update, |_: Query<&Hp>| ()).await.unwrap();
            world.bridge(Later, |_: Query<&Hp>| ()).await.unwrap();
        }))
        .build();
    let tasks = app.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::NotStarted);
    assert!(tasks[0].location.file().ends_with("devtools.rs"));
    app.emit(&Update);
    app.emit(&Update);
    let status = &app.tasks()[0];
    assert!(
        matches!(status.state, TaskState::WaitingFor(label) if label.ends_with("Later")),
        "{status:?}"
    );
    assert!(status.polls >= 1);
    assert_eq!(status.idle_frames, 1);
    app.emit(&Later);
    assert_eq!(app.tasks()[0].state, TaskState::Finished);
}

#[test]
fn rewinding_and_forking_restore_and_diverge() {
    let mut app = App::new()
        .add_systems(Update, (ramp, apply_damage).chain())
        .build();
    app.set_auto_snapshots(1, 8);
    let e = app.world.spawn_bundle((Hp(100), Damage(0)));
    for _ in 0..4 {
        app.emit(&Update);
    }
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(90)));
    assert_eq!(app.snapshot_frames(), vec![1, 2, 3, 4]);
    assert_eq!(app.rewind(2).unwrap(), 2);
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(97)));
    // Running forward from the rewound state repeats history exactly.
    app.emit(&Update);
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(94)));

    let mut fork = app.fork().unwrap();
    let other = app.in_fork(&mut fork, |app| {
        let other = app.world.spawn_bundle((Hp(5), Damage(10)));
        app.emit(&Update);
        app.emit(&Update);
        other
    });
    // The fork ran two frames and has an entity the app does not.
    assert_eq!(fork.world().get::<Hp>(e), Some(&Hp(85)));
    assert!(!app.world.contains(other));
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(94)));
    let diff = app.world.diff(fork.world());
    assert_eq!(diff.spawned, vec![other]);
    assert!(
        diff.changed
            .iter()
            .any(|(entity, name, _, _)| *entity == e && name.ends_with("Hp"))
    );
    // Query caches follow the swap: the app's systems still see its rows.
    app.emit(&Update);
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(90)));
}

#[derive(Clone, Debug, Default)]
struct Wings;
#[derive(Clone, Debug)]
struct Bird;
impl Requires for Bird {
    type Required = (Wings,);
}
#[test]
fn explains_why_components_exist_and_why_systems_are_ordered() {
    let mut app = App::new()
        .add_systems(
            Update,
            ((ramp, apply_damage).chain(), |_: Query<&Bird>| {}).parallel(),
        )
        .register::<Bird>()
        .build();
    let systems = app.systems();
    let (ramp_id, apply_id, birds) = (
        id_of(&systems, "ramp"),
        id_of(&systems, "apply_damage"),
        id_of(&systems, "{{closure}}"),
    );
    let ramp_info = systems.iter().find(|s| s.id == ramp_id).unwrap();
    assert_eq!(ramp_info.path, "parallel[0] > chain[0]");
    assert!(ramp_info.writes().any(|w| w.ends_with("Damage")));
    assert!(app.why_order(ramp_id, apply_id).unwrap().contains("chain"));
    assert!(
        app.why_order(apply_id, birds)
            .unwrap()
            .starts_with("no order")
    );

    let wings = app.why_component("Wings").unwrap();
    assert!(
        wings.iter().any(|r| r.contains("`Bird` requires it")),
        "{wings:?}"
    );
    let bird = app.why_component("Bird").unwrap();
    assert!(
        bird.iter().any(|r| r.starts_with("`register::<Bird>`")),
        "{bird:?}"
    );
    assert!(bird.iter().any(|r| r.contains("uses it: Read")), "{bird:?}");
    let hp = app.why_component("Hp").unwrap();
    assert!(
        hp.iter()
            .any(|r| r.contains("apply_damage") && r.contains("Write")),
        "{hp:?}"
    );
}

#[test]
fn values_are_described_and_edited_without_debug() {
    let mut app = App::new().register::<Armor>().register::<Hp>().build();
    let e = app.world.spawn_bundle((
        Armor {
            value: 3,
            tough: false,
        },
        Hp(1),
    ));
    let armor = app.world.describe_component(e, "Armor").unwrap();
    assert_eq!(armor.get("value"), Some(&Value::Int(3)));
    assert_eq!(armor.get("tough"), Some(&Value::Bool(false)));
    assert_eq!(
        app.world.editable_fields("Armor").unwrap(),
        vec!["value", "tough"]
    );
    let before = app.world.tick();
    assert_eq!(
        app.world.set_field(e, "Armor", "value", "200"),
        Ok(Value::Int(200))
    );
    assert!(
        app.world.set_field(e, "Armor", "value", "300").is_err(),
        "u8 overflow"
    );
    assert_eq!(
        app.world.set_field(e, "Armor", "tough", "true"),
        Ok(Value::Bool(true))
    );
    assert_eq!(app.world.get::<Armor>(e).unwrap().value, 200);
    assert!(app.world.get::<Armor>(e).unwrap().tough);
    let edits = app.world.changes_since(before);
    assert!(edits.iter().all(|c| c.writer == Writer::Edit));
    assert_eq!(app.world.set_field(e, "Hp", "0", "7"), Ok(Value::Int(7)));
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(7)));
}

#[test]
fn debug_output_parses_into_values() {
    use static_ecs::devtools::value::parse_debug;
    let parsed = parse_debug(&format!(
        "{:?}",
        (
            Some(vec![1.5f32, -2.0]),
            "a\"b",
            'x',
            [("k", 3u8)],
            std::collections::BTreeMap::from([(1, "one")])
        )
    ));
    let Value::Tuple { items, .. } = &parsed else {
        panic!("{parsed:?}")
    };
    assert_eq!(
        items[0],
        Value::Tuple {
            name: "Some".into(),
            items: vec![Value::List(vec![Value::Float(1.5), Value::Float(-2.0)])]
        }
    );
    assert_eq!(items[1], Value::Str("a\"b".into()));
    assert_eq!(items[2], Value::Char('x'));
    assert_eq!(
        items[4],
        Value::Map(vec![(Value::Int(1), Value::Str("one".into()))])
    );
    #[derive(Debug)]
    #[allow(dead_code)]
    struct Pos {
        x: f64,
        tag: Option<&'static str>,
    }
    let pos = parse_debug(&format!("{:?}", Pos { x: 1e-9, tag: None }));
    assert_eq!(pos.get("x"), Some(&Value::Float(1e-9)));
    assert_eq!(pos.get("tag"), Some(&Value::Name("None".into())));
}

/// The text of an MCP tool reply.
fn reply_text(reply: std::sync::mpsc::Receiver<String>) -> String {
    use static_ecs::devtools::mcp::json;
    let reply = json::parse(&reply.try_recv().expect("a reply")).unwrap();
    let result = reply.get("result").unwrap_or_else(|| panic!("{reply}"));
    let content = result.get("content").unwrap();
    let json::Json::Arr(items) = content else {
        panic!("{content}")
    };
    items[0].get("text").unwrap().as_str().unwrap().to_string()
}
/// The raw JSON of an MCP tool reply marked as an error.
fn error_reply(reply: std::sync::mpsc::Receiver<String>) -> String {
    let text = reply.try_recv().expect("a reply");
    assert!(text.contains("\"isError\":true"), "{text}");
    text
}

#[test]
fn mcp_tools_serve_a_live_app_on_its_own_thread() {
    use static_ecs::devtools::mcp::{Json, Session};
    let mut app = App::new()
        .add_systems(Update, (ramp, apply_damage).chain())
        .build();
    let e = app.world.spawn_bundle((Hp(50), Damage(0)));
    let (mut session, mut client) = Session::channel();
    session.label("Update", Update);

    let init = client.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    );
    let list = client.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let none = client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    session.pump(&mut app);
    assert!(init.try_recv().unwrap().contains("\"serverInfo\""));
    let tools = list.try_recv().unwrap();
    for tool in [
        "overview", "explain", "history", "step", "what_if", "why_not", "asm",
    ] {
        assert!(
            tools.contains(&format!("\"name\":\"{tool}\"")),
            "{tool} missing"
        );
    }
    assert!(none.try_recv().is_err(), "notifications get no reply");

    let args = |pairs: &[(&str, &str)]| {
        Json::Obj(
            pairs
                .iter()
                .map(|&(k, v)| (k.to_string(), Json::from(v)))
                .collect(),
        )
    };
    let emitted = client.call(
        "emit",
        Json::object([("label", Json::from("Update")), ("count", Json::Num(3.0))]),
    );
    session.pump(&mut app);
    assert!(reply_text(emitted).starts_with("emitted `Update` 3x; now frame 3"));
    // Damage ramps 1, 2, 3.
    assert_eq!(app.world.get::<Hp>(e), Some(&Hp(44)));

    let entity = format!("{}v{}", e.index(), e.generation());
    let overview = client.call("overview", args(&[]));
    let shown = client.call("entity", args(&[("id", &entity)]));
    let explained = client.call("explain", args(&[("entity", &entity), ("component", "Hp")]));
    let first = client.call(
        "history",
        args(&[
            ("component", "Hp"),
            ("condition", "< 48"),
            ("mode", "first"),
        ]),
    );
    let stepped = client.call("step", args(&[("label", "Update")]));
    let missing = client.call("entity", args(&[("id", "999v0")]));
    session.pump(&mut app);
    let overview = reply_text(overview);
    assert!(overview.starts_with("frame 3"), "{overview}");
    assert!(overview.contains("labels you can emit/step: Update"));
    assert!(reply_text(shown).contains("Hp = Hp(44)"));
    let explained = reply_text(explained);
    assert!(
        explained.contains("apply_damage")
            && explained.contains("reads:")
            && explained.contains("ramp"),
        "{explained}"
    );
    assert!(reply_text(first).starts_with("first at frame 1"));
    assert!(reply_text(stepped).starts_with("step 0: system #"));
    assert!(error_reply(missing).contains("not alive"));

    // With Damage raised, Hp drops further in the edited fork only.
    let what_if = client.call(
        "what_if",
        Json::object([
            ("label", Json::from("Update")),
            ("frames", Json::Num(1.0)),
            (
                "edits",
                Json::Arr(vec![args(&[
                    ("entity", &entity),
                    ("component", "Damage"),
                    ("field", "0"),
                    ("value", "20"),
                ])]),
            ),
        ]),
    );
    let before = app.world.get::<Hp>(e).cloned();
    session.pump(&mut app);
    let what_if = reply_text(what_if);
    assert!(
        what_if.contains("Hp: Hp(") && what_if.contains("Damage: Damage("),
        "{what_if}"
    );
    assert_eq!(
        app.world.get::<Hp>(e).cloned(),
        before,
        "the app is untouched"
    );

    // Pause holds `pump` until resume.
    let paused = client.call("pause", args(&[]));
    let resumed = client.call("resume", args(&[]));
    session.pump(&mut app);
    assert!(reply_text(paused).starts_with("paused"));
    assert_eq!(reply_text(resumed), "resumed");
}

#[test]
fn assembly_is_collected_through_inline_chains() {
    use static_ecs::devtools::asm::{Target, scan};
    let objdump = "\
<frame>:
game::apply_damage:
/src/game.rs:10
inlined by /src/game.rs:30 (run)
\tmov    %rax,%rbx
\tcall   1234 <helper+0x10>
other::finish:
/src/other.rs:5
\tret
";
    let lines = || objdump.lines().map(str::to_string);
    let (code, saw) = scan(lines(), &mut Target::Function("apply_damage".into()));
    assert!(saw);
    assert_eq!(
        code.instructions,
        vec!["mov %rax,%rbx", "call <addr> <helper>"]
    );
    let mut closure = Target::Lines {
        file: "game.rs".into(),
        first: 28,
        last: Some(31),
    };
    let (code, _) = scan(lines(), &mut closure);
    assert_eq!(
        code.instructions.len(),
        2,
        "matched through the `inlined by` frame"
    );
    assert!(
        code.stats()
            .starts_with("2 instructions (0 vector, 1 calls, 0 branches) in frame")
    );
}
