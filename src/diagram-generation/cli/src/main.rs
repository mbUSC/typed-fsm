use std::path::Path;
use std::fs;
use std::collections::{HashMap, HashSet};
use clap::Parser;
use serde::Deserialize;
use syn::{visit::{self, Visit}, Macro};
use walkdir::WalkDir;
use typed_fsm_diagram_core::{FsmDefinition, generate_mermaid_simple, generate_mermaid_hierarchical, SubFsmVisitor};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory or file to scan for state_machine! macros
    #[arg(short, long)]
    src: Option<String>,

    /// Path to the configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Override the output directory specified in the config
    #[arg(short, long)]
    output: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Config {
    #[serde(default)]
    mermaid: MermaidConfig,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum DiagramMode {
    Simple,
    Hierarchical,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum BreakdownMode {
    None,
    Flat,
    Nested,
    Both,
}

#[derive(Deserialize, Debug)]
struct MermaidConfig {
    #[serde(default = "default_scan_dir")]
    scan_dir: String,
    #[serde(default = "default_output_dir")]
    output_dir: String,
    #[serde(default = "default_mode")]
    mode: DiagramMode,
    #[serde(default = "default_breakdown")]
    breakdown: BreakdownMode,
}

impl Default for MermaidConfig {
    fn default() -> Self {
        Self {
            scan_dir: default_scan_dir(),
            output_dir: default_output_dir(),
            mode: default_mode(),
            breakdown: default_breakdown(),
        }
    }
}

fn default_scan_dir() -> String { "scan".to_string() }
fn default_output_dir() -> String { "target/docs/diagrams".to_string() }
fn default_mode() -> DiagramMode { DiagramMode::Hierarchical }
fn default_breakdown() -> BreakdownMode { BreakdownMode::Flat }

struct FsmFinder {
    found: Vec<FsmDefinition>,
}

impl<'ast> Visit<'ast> for FsmFinder {
    fn visit_macro(&mut self, i: &'ast Macro) {
        if i.path.is_ident("state_machine") {
            if let Ok(fsm) = i.parse_body::<FsmDefinition>() {
                self.found.push(fsm);
            }
        }
        visit::visit_macro(self, i);
    }
}

fn process_file(path: &Path, finder: &mut FsmFinder) -> anyhow::Result<()> {
    let content = fs::read_to_string(path)?;
    if let Ok(syntax) = syn::parse_file(&content) {
        finder.visit_file(&syntax);
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    
    let config_path = args.config.unwrap_or_else(|| ".typed-fsm.toml".to_string());
    let config: Config = if Path::new(&config_path).exists() {
        let content = fs::read_to_string(config_path)?;
        toml::from_str(&content)?
    } else {
        Config::default()
    };
    
    let output_dir = args.output.unwrap_or(config.mermaid.output_dir.clone());
    let src_input = args.src.unwrap_or(config.mermaid.scan_dir.clone());
    
    let src_path = Path::new(&src_input);
    if !src_path.exists() {
        anyhow::bail!("Error: Source path '{}' does not exist.", src_input);
    }

    println!("Scanning {} for state_machine! macros...", src_input);
    let mut finder = FsmFinder { found: vec![] };

    if src_path.is_file() {
        process_file(src_path, &mut finder)?;
    } else {
        for entry in WalkDir::new(src_path).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().map_or(false, |ext| ext == "rs") {
                process_file(entry.path(), &mut finder)?;
            }
        }
    }
    
    if finder.found.is_empty() {
        println!("No state_machine! macros found in the specified source.");
        return Ok(());
    }

    // Clean start: Delete output directory if it exists to remove stale diagrams
    if Path::new(&output_dir).exists() {
        println!("Cleaning output directory: {}", output_dir);
        fs::remove_dir_all(&output_dir)?;
    }
    fs::create_dir_all(&output_dir)?;

    println!("Found {} FSM definitions.", finder.found.len());
    let fsm_map: HashMap<String, &FsmDefinition> = finder.found.iter().map(|f| (f.name.to_string(), f)).collect();

    // Identify Root FSMs (those not referenced by any other FSM)
    let mut all_children = HashSet::new();
    for fsm in &finder.found {
        let mut visitor = SubFsmVisitor {
            fsm_name: fsm.name.to_string(),
            discovered: HashSet::new(),
        };
        for state in &fsm.states {
            if let Some(entry) = &state.entry_block { visitor.visit_expr(entry); }
            visitor.visit_expr(&state.process_block);
            if let Some(exit) = &state.exit_block { visitor.visit_expr(exit); }
            for (_, f_type) in &state.fields { visitor.visit_type(f_type); }
        }
        for child in visitor.discovered {
            all_children.insert(child);
        }
    }

    let root_fsms: Vec<&FsmDefinition> = if src_path.is_file() {
        finder.found.iter().collect()
    } else {
        finder.found.iter()
            .filter(|f| !all_children.contains(&f.name.to_string()))
            .collect()
    };

    println!("Identified {} root FSM(s) for organization.", root_fsms.len());

    for fsm in root_fsms {
        let name = fsm.name.to_string();
        let fsm_output_dir = Path::new(&output_dir).join(&name);
        fs::create_dir_all(&fsm_output_dir)?;
        
        let content = match config.mermaid.mode {
            DiagramMode::Simple => generate_mermaid_simple(fsm),
            DiagramMode::Hierarchical => generate_mermaid_hierarchical(fsm, &fsm_map),
        };
        
        let path = fsm_output_dir.join(format!("{}.mermaid", name));
        fs::write(&path, content)?;
        println!("Generated root: {}", path.display());

        match config.mermaid.breakdown {
            BreakdownMode::Flat => save_breakdown(fsm, &fsm_map, &fsm_output_dir, "breakdown", false)?,
            BreakdownMode::Nested => save_breakdown(fsm, &fsm_map, &fsm_output_dir, "breakdown", true)?,
            BreakdownMode::Both => {
                save_breakdown(fsm, &fsm_map, &fsm_output_dir, "breakdown_flat", false)?;
                save_breakdown(fsm, &fsm_map, &fsm_output_dir, "breakdown_nested", true)?;
            },
            BreakdownMode::None => {},
        }
    }
    
    Ok(())
}

fn save_breakdown(
    fsm: &FsmDefinition, 
    all_fsms: &HashMap<String, &FsmDefinition>, 
    fsm_output_dir: &Path, 
    sub_dir: &str, 
    nested: bool
) -> anyhow::Result<()> {
    let mut discovered = HashSet::new();
    collect_all_subfsms(fsm, all_fsms, &mut discovered);

    if discovered.is_empty() {
        return Ok(());
    }

    let target_dir = fsm_output_dir.join(sub_dir);
    fs::create_dir_all(&target_dir)?;

    for sub_name in discovered {
        if let Some(sub_fsm) = all_fsms.get(&sub_name) {
            let content = if nested {
                generate_mermaid_hierarchical(sub_fsm, all_fsms)
            } else {
                generate_mermaid_simple(sub_fsm)
            };
            let path = target_dir.join(format!("{}.mermaid", sub_name));
            fs::write(path, content)?;
        }
    }
    Ok(())
}

fn collect_all_subfsms(
    fsm: &FsmDefinition, 
    all_fsms: &HashMap<String, &FsmDefinition>, 
    discovered: &mut HashSet<String>
) {
    let mut visitor = SubFsmVisitor {
        fsm_name: fsm.name.to_string(),
        discovered: HashSet::new(),
    };
    for state in &fsm.states {
        if let Some(entry) = &state.entry_block { visitor.visit_expr(entry); }
        visitor.visit_expr(&state.process_block);
        if let Some(exit) = &state.exit_block { visitor.visit_expr(exit); }
        for (_, f_type) in &state.fields { visitor.visit_type(f_type); }
    }

    for sub_name in visitor.discovered {
        if all_fsms.contains_key(&sub_name) && !discovered.contains(&sub_name) {
            discovered.insert(sub_name.clone());
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                collect_all_subfsms(sub_fsm, all_fsms, discovered);
            }
        }
    }
}
