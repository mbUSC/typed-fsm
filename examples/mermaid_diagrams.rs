//! Mermaid FSM Diagram & Tree Export Example
//!
//! This example shows how to generate Mermaid.js diagrams from an existing FSM.
//! It reuses the FSM definitions from the `hierarchical` example and demonstrates
//! the various configuration "knobs" for controlling generation.
//!
//! Run with: `cargo run --example mermaid_diagrams --features "diagram std"`

#[allow(unused_imports)]
use typed_fsm::{state_machine, Transition};
use typed_fsm::diagram_helpers::{DiagramOptions, DiagramMode, BreakdownMode};

#[allow(dead_code)]
#[path = "hierarchical.rs"]
mod hierarchical;

use hierarchical::PlayerFSM;

fn main() {
    println!("=== typed-fsm Mermaid Diagrams ===\n");

    // 1. flat
    println!("1. Exporting 'flat' (single file, no substates)...");
    let options = DiagramOptions {
        mode: DiagramMode::Flat,
        breakdown: BreakdownMode::None,
        ..Default::default()
    };
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/flat", &options).expect("Failed flat");
    println!("   - Created: target/fsm_diagrams/flat/PlayerFSM.mermaid\n");

    // 2. hierarchical
    println!("2. Exporting 'hierarchical' (single file, with substates)...");
    let options = DiagramOptions {
        mode: DiagramMode::Hierarchical,
        breakdown: BreakdownMode::None,
        ..Default::default()
    };
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/hierarchical", &options).expect("Failed hierarchical");
    println!("   - Created: target/fsm_diagrams/hierarchical/PlayerFSM.mermaid\n");

    // 3. hierarchical_and_flat_breakdown
    println!("3. Exporting 'hierarchical_and_flat_breakdown'...");
    let options = DiagramOptions {
        mode: DiagramMode::Hierarchical,
        breakdown: BreakdownMode::Flat,
        ..Default::default()
    };
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/hierarchical_and_flat_breakdown", &options).expect("Failed hierarchical_and_flat_breakdown");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_flat_breakdown/PlayerFSM.mermaid");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_flat_breakdown/breakdown/PlayerFSM.mermaid (Flat)\n");

    // 4. hierarchical_and_nested_breakdown
    println!("4. Exporting 'hierarchical_and_nested_breakdown'...");
    let options = DiagramOptions {
        mode: DiagramMode::Hierarchical,
        breakdown: BreakdownMode::Nested,
        ..Default::default()
    };
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/hierarchical_and_nested_breakdown", &options).expect("Failed hierarchical_and_nested_breakdown");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_nested_breakdown/PlayerFSM.mermaid");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_nested_breakdown/breakdown/PlayerFSM.mermaid (Nested)\n");

    // 5. hierarchical_and_flat_and_nested_breakdown
    println!("5. Exporting 'hierarchical_and_flat_and_nested_breakdown'...");
    let options = DiagramOptions {
        mode: DiagramMode::Hierarchical,
        breakdown: BreakdownMode::Both,
        ..Default::default()
    };
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/hierarchical_and_flat_and_nested_breakdown", &options).expect("Failed hierarchical_and_flat_and_nested_breakdown");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_flat_and_nested_breakdown/PlayerFSM.mermaid");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_flat_and_nested_breakdown/breakdown_flat/");
    println!("   - Created: target/fsm_diagrams/hierarchical_and_flat_and_nested_breakdown/breakdown_nested/\n");

    // 6. default (Hierarchical + BreakdownMode::Both)
    println!("6. Exporting with 'default' options (Hierarchical + Both breakdowns)...");
    let options = DiagramOptions::default();
    PlayerFSM::save_diagrams_ext("target/fsm_diagrams/default", &options).expect("Failed default");
    println!("   - Created: target/fsm_diagrams/default/PlayerFSM.mermaid");
    println!("   - Created: target/fsm_diagrams/default/breakdown_flat/");
    println!("   - Created: target/fsm_diagrams/default/breakdown_nested/\n");

    println!("✅ All diagrams exported successfully!");
}
