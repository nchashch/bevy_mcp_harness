//! Property tests for the BRP game tools (`super` = `crate::brp`).
//!
//! Three targets, per the test-suite decision — the places where coordinate-space and
//! combinatorial bugs actually lived:
//!
//! 1. **`parse_crop`** — for ANY four numbers the parse is either a clean rejection or four
//!    non-negative rounded pixel counts. No crop value can poison a later comparison.
//! 2. **`encode_served_view`** — for ANY frame size, crop rect, and downscale limit: the
//!    served view decodes as a PNG, its dims follow the crop-then-fit formula exactly, and
//!    the crop is clamped to the frame's intersection.
//! 3. **The interaction fold** (`ui_dump_snapshot`) — over random UI trees with random
//!    hover/press states: every clickable node is a row reading the strongest state of the
//!    nodes whose first clickable ancestor it is (Pressed > Hovered > Idle); covered
//!    non-clickable rows fold away; uncovered non-clickable rows keep hover from their
//!    subtree (the hover-map pass marks whole ancestor chains). Unlike the Tier-2 scene
//!    tests, this explores the fold's combinatorics (nested panels, sibling buttons, mixed
//!    states) rather than three hand-picked states.
//!
//! The tree property boots the real render-less composition per case (UI layout is
//! render-free logic) — the same canary rationale as the Tier-2 scene tests: the fixture is
//! the real `UiStack`/`ComputedNode` pipeline, not a mock of it. Cases are reduced (64) to
//! keep the suite fast; `PROPTEST_CASES` overrides for a deeper run.

use super::ui_dump_method;
use bevy::ecs::system::In;
use bevy::picking::backend::HitData;
use bevy::picking::hover::HoverMap;
use bevy::prelude::*;
use bevy::ui::Pressed;
use proptest::prelude::*;
use serde_json::json;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 1. parse_crop — clean rejection or four non-negative rounded pixel counts.
// ---------------------------------------------------------------------------

/// The numeric domain the crop param actually arrives from (an LLM writing JSON): normal
/// positives, negatives, non-finite specials, and huge magnitudes.
fn crop_number() -> impl Strategy<Value = f64> {
    prop_oneof![
        6 => 0.0..1e6f64,
        2 => -1e6f64..0.0,
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
        1 => (-1e300f64..1e300).prop_filter("finite only", |x| x.is_finite()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parse_crop_is_a_clean_reject_or_four_rounded_counts(
        numbers in prop::collection::vec(crop_number(), 4),
    ) {
        let parsed = super::parse_crop(&json!(numbers));
        let all_valid = numbers.iter().all(|n| n.is_finite() && *n >= 0.0);
        match parsed {
            Some(rect) => {
                prop_assert!(all_valid, "accepted {numbers:?} with an invalid member");
                for (input, output) in numbers.iter().zip(rect.iter()) {
                    // Round-to-pixel with the same saturating cast for out-of-u32-range
                    // magnitudes — huge positives clamp to u32::MAX, they don't wrap.
                    prop_assert_eq!(*output, input.round() as u32);
                }
            }
            None => prop_assert!(!all_valid, "rejected an all-valid crop {numbers:?}"),
        }
    }

    #[test]
    fn parse_crop_rejects_wrong_shape(
        numbers in prop::collection::vec(crop_number(), 0..8),
    ) {
        prop_assume!(numbers.len() != 4);
        prop_assert_eq!(super::parse_crop(&json!(numbers)), None);
    }
}

// ---------------------------------------------------------------------------
// 2. encode_served_view — dims follow crop-then-fit exactly; output decodes.
// ---------------------------------------------------------------------------

/// The served-view dims contract, spelled out: the crop degrades to its intersection with the
/// frame, then the LONG EDGE of the cropped region is fitted to `max_dimension` (aspect
/// preserved, no upscale, 1px floor per side).
fn expected_view_size(
    frame: (u32, u32),
    crop: Option<[u32; 4]>,
    max_dimension: Option<u32>,
) -> (u32, u32) {
    let (fw, fh) = frame;
    let (cw, ch) = match crop {
        None => (fw, fh),
        Some([x, y, w, h]) => {
            let x = x.min(fw.saturating_sub(1));
            let y = y.min(fh.saturating_sub(1));
            (w.min(fw - x).max(1), h.min(fh - y).max(1))
        }
    };
    match max_dimension {
        Some(max) => {
            let long = cw.max(ch);
            if long > max {
                let scale = max as f64 / long as f64;
                (
                    ((cw as f64 * scale).round().max(1.0) as u32).max(1),
                    ((ch as f64 * scale).round().max(1.0) as u32).max(1),
                )
            } else {
                (cw, ch)
            }
        }
        None => (cw, ch),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn served_view_dims_follow_the_contract(
        frame_w in 1u32..=300,
        frame_h in 1u32..=300,
        crop in prop::option::of((
            0u32..=400, 0u32..=400, 1u32..=400, 1u32..=400,
        )),
        max_dimension in prop::option::of(1u32..=500),
    ) {
        let png = super::tests::tiny_png(frame_w, frame_h);
        let crop = crop.map(|(x, y, w, h)| [x, y, w, h]);
        let (bytes, size) = super::encode_served_view(&png, crop, max_dimension)
            .unwrap_or_else(|| panic!("encode failed: frame {frame_w}x{frame_h}"));
        let (expected_w, expected_h) =
            expected_view_size((frame_w, frame_h), crop, max_dimension);
        prop_assert_eq!(
            size,
            [expected_w, expected_h],
            "frame {}x{}, crop {:?}, max {:?}",
            frame_w,
            frame_h,
            crop,
            max_dimension
        );
        // Whatever dims it claims, the bytes decode to exactly those dims.
        let decoded = image::load_from_memory(&bytes).unwrap();
        prop_assert_eq!((decoded.width(), decoded.height()), (size[0], size[1]));
    }

    #[test]
    fn served_view_crop_pixels_come_from_the_crop_rect(
        frame_w in 2u32..=60,
        frame_h in 2u32..=60,
        crop_x in 0u32..=40,
        crop_y in 0u32..=40,
        crop_w in 1u32..=40,
        crop_h in 1u32..=40,
    ) {
        let png = super::tests::tiny_png(frame_w, frame_h);
        let (bytes, size) =
            super::encode_served_view(&png, Some([crop_x, crop_y, crop_w, crop_h]), None).unwrap();
        // No downscale: the view IS the clamped crop, pixel-identical to the frame's sub-rect.
        let x = crop_x.min(frame_w - 1);
        let y = crop_y.min(frame_h - 1);
        let w = crop_w.min(frame_w - x);
        let h = crop_h.min(frame_h - y);
        prop_assert_eq!(
            size,
            [w, h],
            "crop {},{},{}x{} on {}x{}",
            crop_x,
            crop_y,
            crop_w,
            crop_h,
            frame_w,
            frame_h
        );
        let view = image::load_from_memory(&bytes).unwrap().to_rgb8();
        let frame = image::load_from_memory(&png).unwrap().to_rgb8();
        for vy in 0..h {
            for vx in 0..w {
                prop_assert_eq!(view.get_pixel(vx, vy), frame.get_pixel(x + vx, y + vy));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. The interaction fold over random UI trees.
// ---------------------------------------------------------------------------

/// A generated UI tree. `clickable` nodes carry `Interaction` (the harness's default
/// convention); `pressed`/`hovered` are the bevy-0.20 widget-state components.
#[derive(Debug, Clone)]
struct PNode {
    clickable: bool,
    pressed: bool,
    hovered: bool,
    children: Vec<PNode>,
}

fn pnode_strategy() -> impl Strategy<Value = PNode> {
    let leaf = (any::<bool>(), any::<bool>(), any::<bool>())
        .prop_map(|(clickable, pressed, hovered)| PNode {
            clickable,
            pressed,
            hovered,
            children: Vec::new(),
        });
    // Depth ≤ 4, ≤ ~16 nodes, ≤ 3 children per level: enough combinatorics to hit nested
    // panels, sibling buttons, and mixed states; small enough that the per-case app boot
    // keeps the suite fast.
    leaf.prop_recursive(
        4,
        16,
        3,
        |inner| {
            (
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                proptest::collection::vec(inner, 1..=3),
            )
                .prop_map(|(clickable, pressed, hovered, children)| PNode {
                    clickable,
                    pressed,
                    hovered,
                    children,
                })
        },
    )
}

/// Preorder flatten (parents before children). Each entry: label (preorder index), flags, and
/// the parent's label (None for roots).
#[allow(clippy::type_complexity)]
fn flatten(node: &PNode) -> Vec<(usize, bool, bool, bool, Option<usize>)> {
    fn walk(
        node: &PNode,
        label: &mut usize,
        parent: Option<usize>,
        out: &mut Vec<(usize, bool, bool, bool, Option<usize>)>,
    ) {
        let own = *label;
        *label += 1;
        out.push((own, node.clickable, node.pressed, node.hovered, parent));
        for child in &node.children {
            walk(child, label, Some(own), out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut 0, None, &mut out);
    out
}

/// Strongest state over a subtree: Pressed (2) > Hovered (1) > none (0).
fn subtree_states(node: &PNode) -> (bool, bool) {
    let mut pressed = node.pressed;
    let mut hovered = node.hovered;
    for child in &node.children {
        let (cp, ch) = subtree_states(child);
        pressed |= cp;
        hovered |= ch;
    }
    (pressed, hovered)
}

fn rank(interaction: Option<&str>) -> u8 {
    match interaction {
        Some("Pressed") => 2,
        Some("Hovered") => 1,
        _ => 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn interaction_fold_matches_first_clickable_ancestor_groups(
        tree in pnode_strategy(),
    ) {
        let flat = flatten(&tree);
        eprintln!("FLAT: {flat:?}");
        prop_assert!(!flat.is_empty());

        let mut app = super::tests::no_render_app();
        let flat_for_spawn = flat.clone();
        app.add_systems(
            Startup,
            move |mut commands: Commands| {
                // Parents spawn before children (preorder), so each node's parent id exists
                // when the child is spawned.
                let mut by_label: HashMap<usize, Entity> = HashMap::new();
                for (label, clickable, pressed, _, parent) in &flat_for_spawn {
                    let mut entity = commands.spawn((
                        Name::new(format!("n{label}")),
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(0.0),
                            top: Val::Px(0.0),
                            width: Val::Px(10.0),
                            height: Val::Px(10.0),
                            ..default()
                        },
                        Text::new(format!("t{label}")),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                    ));
                    if let Some(parent) = parent {
                        entity.insert(ChildOf(by_label[parent]));
                    }
                    let id = entity.id();
                    if *clickable {
                        #[expect(deprecated)]
                        entity.insert(bevy::ui::Interaction::None);
                    }
                    if *pressed {
                        entity.insert(Pressed);
                    }
                    by_label.insert(*label, id);
                }
            },
        );
        for _ in 0..3 {
            app.update();
        }

        // Hover state: the hover map is inserted directly (picking would recompute it from
        // pointer input, which has no hits in the fixture).
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<Camera>>()
            .single(app.world())
            .unwrap();
        let label_to_entity: HashMap<usize, Entity> = app
            .world_mut()
            .query_filtered::<(Entity, &Name), With<Text>>()
            .iter(app.world())
            .map(|(entity, name)| {
                let label: usize = name.as_str().trim_start_matches('n').parse().unwrap();
                (label, entity)
            })
            .collect();
        let mut hits = bevy::ecs::entity::EntityHashMap::default();
        for (label, _, _, hovered, _) in &flat {
            if *hovered {
                hits.insert(label_to_entity[label], HitData::new(camera, 0.0, None, None));
            }
        }
        if !hits.is_empty() {
            app.world_mut().insert_resource(HoverMap(
                bevy::platform::collections::HashMap::from([(
                    bevy::picking::pointer::PointerId::Mouse,
                    hits,
                )]),
            ));
        }

        let dump = ui_dump_method(In(Some(json!({}))), app.world_mut()).unwrap();
        let rows = dump["nodes"].as_array().cloned().unwrap_or_default();
        let row_of_entity = |entity: Entity| {
            rows.iter().find(|row| row["entity"] == json!(entity)).cloned()
        };

        // The interaction contract, per node:
        // - a clickable node keeps its row and reads the strongest state of its GROUP —
        //   itself plus every node whose FIRST clickable ancestor is this node (press
        //   propagates only that far; hover propagates up whole chains via the hover-map
        //   pass, which for group members is the same set);
        // - a non-clickable node with a clickable ancestor is folded away;
        // - an uncovered non-clickable node keeps its row: own `Pressed` reads Pressed,
        //   subtree hover reads Hovered, otherwise no interaction field.
        let parent_of: HashMap<usize, Option<usize>> = flat
            .iter()
            .map(|(label, .., parent)| (*label, *parent))
            .collect();
        let clickable_of_label = |label: usize| -> bool {
            flat.iter().find(|(l, ..)| *l == label).unwrap().1
        };
        let first_clickable = |label: usize| -> Option<usize> {
            let mut current = Some(label);
            for _ in 0..16 {
                let node_label = current?;
                if clickable_of_label(node_label) {
                    return Some(node_label);
                }
                current = parent_of[&node_label];
            }
            None
        };
        let subtree_hovered_of_label = |label: usize| -> bool {
            fn find(node: &PNode, label: usize) -> Option<&PNode> {
                // Preorder labels: walk with a counter.
                fn rec<'a>(node: &'a PNode, label: usize, next: &mut usize) -> Option<&'a PNode> {
                    let own = *next;
                    *next += 1;
                    if own == label {
                        return Some(node);
                    }
                    for child in &node.children {
                        if let Some(found) = rec(child, label, next) {
                            return Some(found);
                        }
                    }
                    None
                }
                rec(node, label, &mut 0)
            }
            subtree_states(find(&tree, label).unwrap()).1
        };

        // Group membership: for every node, its first clickable ancestor (or itself).
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for (label, ..) in &flat {
            if let Some(anchor) = first_clickable(*label) {
                groups.entry(anchor).or_default().push(*label);
            }
        }

        for (label, node_clickable, node_pressed, ..) in &flat {
            let entity = label_to_entity[label];
            let row = row_of_entity(entity);
            match first_clickable(*label) {
                Some(anchor) if *node_clickable => {
                    let group = &groups[&anchor];
                    let group_pressed = group
                        .iter()
                        .any(|n| flat.iter().any(|(l, _, p, _, _)| l == n && *p));
                    let group_hovered = group
                        .iter()
                        .any(|n| {
                            flat.iter().any(|(l, _, _, h, _)| l == n && *h)
                        }) || group.iter().any(|n| subtree_hovered_of_label(*n));
                    let expected = if group_pressed {
                        "Pressed"
                    } else if group_hovered {
                        "Hovered"
                    } else {
                        "Idle"
                    };
                    let row = row.unwrap_or_else(|| {
                        panic!("clickable node {label} missing from the dump")
                    });
                    prop_assert_eq!(row["interaction"].as_str(), Some(expected), "node {} — dump {}", label, dump);
                }
                Some(_) => {
                    prop_assert!(row.is_none(), "node {} should be folded under its clickable ancestor", label);
                }
                None => {
                    // Uncovered: hover still propagates up the chain from any hovered
                    // descendant; press only reads on the node itself.
                    let subtree_hovered = subtree_hovered_of_label(*label);
                    let expected: Option<&str> = if *node_pressed {
                        Some("Pressed")
                    } else if subtree_hovered {
                        Some("Hovered")
                    } else {
                        None
                    };
                    match expected {
                        Some(state) => {
                            let row = row.unwrap_or_else(|| panic!("node {label} missing"));
                            prop_assert_eq!(row["interaction"].as_str(), Some(state), "node {} — dump {}", label, dump);
                        }
                        None => {
                            if let Some(row) = row {
                                prop_assert!(
                                row.get("interaction").is_none(),
                                "node {} unexpectedly has {}",
                                label,
                                row["interaction"]
                            );
                            }
                        }
                    }
                }
            }
            let _ = rank(None::<&str>);
        }
    }
}
