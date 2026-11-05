//! static-ui-layout against clay.h on random trees: every container box and
//! every text line must match.
use static_ui_clay_oracle::{Clay, Output, Rng, Shape, port, random_case};
use static_ui_layout::{Rect, TextCache};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-3 * (1.0 + a.abs().max(b.abs()))
}
fn same_rect(a: &Rect, b: &Rect) -> bool {
    close(a.x, b.x) && close(a.y, b.y) && close(a.width, b.width) && close(a.height, b.height)
}
fn compare(case: usize, clay: &Output, ours: &Output) -> Result<(), String> {
    for (i, (c, o)) in clay.boxes.iter().zip(&ours.boxes).enumerate() {
        if !same_rect(c, o) {
            return Err(format!("case {case}: node {i}: clay {c:?}, ours {o:?}"));
        }
    }
    if clay.lines.len() != ours.lines.len() {
        return Err(format!("case {case}: {} text lines in clay, {} in ours\nclay {:?}\nours {:?}", clay.lines.len(), ours.lines.len(), clay.lines, ours.lines));
    }
    for (i, (c, o)) in clay.lines.iter().zip(&ours.lines).enumerate() {
        let same = close(c.x, o.x) && close(c.y, o.y) && close(c.width, o.width) && close(c.height, o.height) && c.length == o.length;
        if !same {
            return Err(format!("case {case}: line {i}: clay {c:?}, ours {o:?}"));
        }
    }
    Ok(())
}

#[test]
fn random_trees_lay_out_exactly_as_clay_does() {
    let cases = std::env::var("CLAY_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(100_000);
    let mut clay = Clay::new(8192);
    let mut cache = TextCache::default();
    let mut rng = Rng::new(0x5eed);
    let mut stalled = 0;
    for case in 0..cases {
        let shape = Shape { floating_percent: [0, 8, 20][case % 3], ..Shape::default() };
        let tree = random_case(&mut rng, shape);
        if std::env::var_os("CLAY_TRACE").is_some() {
            eprintln!("case {case}");
        }
        // Two frames: floating elements attached to later elements read
        // the previous frame's boxes in both. Trees on which Clay would
        // loop forever are skipped (and counted).
        let Some(ours) = port(&tree, &mut cache, 2) else {
            stalled += 1;
            continue;
        };
        let (expected, errors) = clay.layout(&tree, 2);
        assert_eq!(errors, 0, "case {case}: clay reported errors for {tree:?}");
        if let Err(message) = compare(case, &expected, &ours) {
            panic!("{message}\n{tree:#?}");
        }
    }
    eprintln!("{cases} trees, {stalled} skipped where Clay loops forever");
    assert!(stalled * 100 < cases, "too many degenerate trees: {stalled}");
}
