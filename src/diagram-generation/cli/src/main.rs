use clap::Parser;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use ra_ap_syntax::{ast::{self, AstNode, HasName}, match_ast, SourceFile, Edition};
use typed_fsm_diagram_core::{
    analyze_function, collect_referenced_fsms, generate_mermaid_hierarchical,
    generate_mermaid_simple, parse_macro_body, FsmDefinition, FunctionRegistry,
};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, rename_all = "snake_case")]
struct Args {
    /// Source directory or file to scan for state_machine! macros
    #[arg(short, long)]
    scan: Option<String>,

    /// Path to the configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Override the output directory specified in the config
    #[arg(short, long)]
    output: Option<String>,

    /// Include guards in the generated diagrams
    #[arg(
        long = "include-guards",
        alias = "include_guards",
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    include_guards: Option<bool>,
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
    #[serde(default)]
    include_guards: bool,
}

impl Default for MermaidConfig {
    fn default() -> Self {
        Self {
            scan_dir: default_scan_dir(),
            output_dir: default_output_dir(),
            mode: default_mode(),
            breakdown: default_breakdown(),
            include_guards: true,
        }
    }
}

fn default_scan_dir() -> String {
    "src".to_string()
}
fn default_output_dir() -> String {
    "target/docs/diagrams".to_string()
}
fn default_mode() -> DiagramMode {
    DiagramMode::Hierarchical
}
fn default_breakdown() -> BreakdownMode {
    BreakdownMode::Flat
}

struct WorkspaceFinder {
    found_fsms: Vec<FsmDefinition>,
    found_structs: HashMap<String, HashMap<String, String>>,
    found_functions: FunctionRegistry,
    found_aliases: HashMap<String, String>,
}

impl WorkspaceFinder {
    fn scan_node(&mut self, node: ra_ap_syntax::SyntaxNode, source_path: &Path) {
        for child in node.descendants() {
            match_ast! {
                match child {
                    ast::MacroCall(it) => {
                        if it.path().map(|p| p.syntax().text().to_string() == "state_machine").unwrap_or(false) {
                            if let Some(token_tree) = it.token_tree() {
                                match parse_macro_body(token_tree) {
                                    Ok(fsm) => self.found_fsms.push(fsm),
                                    Err(e) => eprintln!(
                                        "warning: failed to parse state_machine! macro in {}: {}",
                                        source_path.display(),
                                        e,
                                    ),
                                }
                            }
                        }
                    },
                    ast::Struct(it) => {
                        if let Some(name) = it.name() {
                            let struct_name = name.text().to_string();
                            let mut fields = HashMap::new();
                            if let Some(field_list) = it.field_list() {
                                match field_list {
                                    ast::FieldList::RecordFieldList(list) => {
                                        for field in list.fields() {
                                            if let (Some(f_name), Some(f_type)) = (field.name(), field.ty()) {
                                                fields.insert(f_name.text().to_string(), f_type.syntax().text().to_string().replace(" ", ""));
                                            }
                                        }
                                    },
                                    ast::FieldList::TupleFieldList(list) => {
                                        for (i, field) in list.fields().enumerate() {
                                            if let Some(f_type) = field.ty() {
                                                fields.insert(i.to_string(), f_type.syntax().text().to_string().replace(" ", ""));
                                            }
                                        }
                                    }
                                }
                            }
                            self.found_structs.insert(struct_name, fields);
                        }
                    },
                    ast::TypeAlias(it) => {
                        if let (Some(name), Some(ty)) = (it.name(), it.ty()) {
                            self.found_aliases.insert(name.text().to_string(), ty.syntax().text().to_string().replace(" ", ""));
                        }
                    },
                    ast::Fn(it) => {
                        if let Some((name, info)) = analyze_function(&it) {
                            self.found_functions.record(name, info);
                        }
                    },
                    _ => ()
                }
            }
        }
    }

    fn resolve_type(&self, type_name: &str) -> String {
        let mut current = type_name.to_string();
        let mut seen = HashSet::new();
        while let Some(aliased) = self.found_aliases.get(&current) {
            if seen.contains(aliased) { break; } // prevent infinite loops
            seen.insert(aliased.clone());
            current = aliased.clone();
        }
        current
    }
}

fn process_file(path: &Path, finder: &mut WorkspaceFinder) -> anyhow::Result<()> {
    let content = fs::read_to_string(path)?;
    let parse = SourceFile::parse(&content, Edition::Edition2021);
    finder.scan_node(parse.tree().syntax().clone(), path);
    Ok(())
}

/// Walk `root` and delete every regular file ending in `.mermaid`, then
/// remove any directories left empty. Touches nothing else, so a
/// misconfigured `output_dir` pointing at an unintended place no longer
/// destroys unrelated data.
fn clean_stale_diagrams(root: &Path) -> anyhow::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    // contents_first: delete files before their parent dirs.
    for entry in WalkDir::new(root).contents_first(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().is_file() {
            if path.extension().map_or(false, |ext| ext == "mermaid") {
                let _ = fs::remove_file(path);
            }
        } else if entry.file_type().is_dir() && path != root {
            // Only remove if empty — leaves unrelated subdirs alone.
            let _ = fs::remove_dir(path);
        }
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
    let src_input = args.scan.unwrap_or(config.mermaid.scan_dir.clone());
    let include_guards = args.include_guards.unwrap_or(config.mermaid.include_guards);

    let src_path = Path::new(&src_input);
    if !src_path.exists() {
        anyhow::bail!("Error: Source path '{}' does not exist.", src_input);
    }

    println!(
        "Scanning {} for state_machine! macros and context structs...",
        src_input
    );
    let mut finder = WorkspaceFinder {
        found_fsms: vec![],
        found_structs: HashMap::new(),
        found_functions: FunctionRegistry::new(),
        found_aliases: HashMap::new(),
    };

    if src_path.is_file() {
        process_file(src_path, &mut finder)?;
    } else {
        for entry in WalkDir::new(src_path).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().map_or(false, |ext| ext == "rs") {
                process_file(entry.path(), &mut finder)?;
            }
        }
    }

    if finder.found_fsms.is_empty() {
        println!("No state_machine! macros found in the specified source.");
        return Ok(());
    }

    // Clean start: drop only `.mermaid` files (and now-empty FSM subdirs)
    // under the output directory. We deliberately do NOT `remove_dir_all` —
    // that footgun has cost users data when the configured output_dir pointed
    // somewhere unexpected. Leave unrelated files in place.
    if Path::new(&output_dir).exists() {
        println!("Cleaning stale .mermaid files under: {}", output_dir);
        clean_stale_diagrams(Path::new(&output_dir))?;
    }
    fs::create_dir_all(&output_dir)?;

    println!(
        "Found {} FSM definitions, {} struct definitions, {} aliases, and {} functions.",
        finder.found_fsms.len(),
        finder.found_structs.len(),
        finder.found_aliases.len(),
        finder.found_functions.len()
    );

    let fsm_map: HashMap<String, &FsmDefinition> = finder
        .found_fsms
        .iter()
        .map(|f| (f.name.to_string(), f))
        .collect();

    // Identify Root FSMs (those not referenced by any other FSM)
    let mut all_children = HashSet::new();
    for fsm in &finder.found_fsms {
        all_children.extend(collect_referenced_fsms(
            fsm,
            &finder.found_structs,
            |t| finder.resolve_type(t),
        ));
    }

    let root_fsms: Vec<&FsmDefinition> = finder
        .found_fsms
        .iter()
        .filter(|f| !all_children.contains(&f.name.to_string()))
        .collect();

    println!(
        "Identified {} root FSM(s) for organization.",
        root_fsms.len()
    );

    for fsm in root_fsms {
        let name = fsm.name.to_string();
        let fsm_output_dir = Path::new(&output_dir).join(&name);
        fs::create_dir_all(&fsm_output_dir)?;

        let resolver = |t: &str| finder.resolve_type(t);

        let content = match config.mermaid.mode {
            DiagramMode::Simple => {
                generate_mermaid_simple(fsm, include_guards, &finder.found_functions)?
            }
            DiagramMode::Hierarchical => generate_mermaid_hierarchical(
                fsm,
                &fsm_map,
                &finder.found_structs,
                include_guards,
                &finder.found_functions,
                resolver,
            )?,
        };

        let path = fsm_output_dir.join(format!("{}.mermaid", name));
        fs::write(&path, content)?;
        println!("Generated root: {}", path.display());

        match config.mermaid.breakdown {
            BreakdownMode::Flat => save_breakdown(
                fsm,
                &fsm_map,
                &finder.found_structs,
                &fsm_output_dir,
                "breakdown",
                false,
                include_guards,
                &finder.found_functions,
                resolver,
            )?,
            BreakdownMode::Nested => save_breakdown(
                fsm,
                &fsm_map,
                &finder.found_structs,
                &fsm_output_dir,
                "breakdown",
                true,
                include_guards,
                &finder.found_functions,
                resolver,
            )?,
            BreakdownMode::Both => {
                save_breakdown(
                    fsm,
                    &fsm_map,
                    &finder.found_structs,
                    &fsm_output_dir,
                    "breakdown_flat",
                    false,
                    include_guards,
                    &finder.found_functions,
                    resolver,
                )?;
                save_breakdown(
                    fsm,
                    &fsm_map,
                    &finder.found_structs,
                    &fsm_output_dir,
                    "breakdown_nested",
                    true,
                    include_guards,
                    &finder.found_functions,
                    resolver,
                )?;
            }
            BreakdownMode::None => {}
        }
    }

    Ok(())
}

fn save_breakdown<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    struct_map: &HashMap<String, HashMap<String, String>>,
    fsm_output_dir: &Path,
    sub_dir: &str,
    nested: bool,
    include_guards: bool,
    function_registry: &FunctionRegistry,
    resolve_type: F,
) -> anyhow::Result<()>
where
    F: Fn(&str) -> String + Copy,
{
    let mut discovered = HashSet::new();
    collect_all_subfsms(fsm, all_fsms, struct_map, &mut discovered, resolve_type);

    if discovered.is_empty() {
        return Ok(());
    }

    let target_dir = fsm_output_dir.join(sub_dir);
    fs::create_dir_all(&target_dir)?;

    for sub_name in discovered {
        if let Some(sub_fsm) = all_fsms.get(&sub_name) {
            let content = if nested {
                generate_mermaid_hierarchical(
                    sub_fsm,
                    all_fsms,
                    struct_map,
                    include_guards,
                    function_registry,
                    resolve_type,
                )?
            } else {
                generate_mermaid_simple(sub_fsm, include_guards, function_registry)?
            };
            let path = target_dir.join(format!("{}.mermaid", sub_name));
            fs::write(path, content)?;
        }
    }
    Ok(())
}

fn collect_all_subfsms<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    struct_map: &HashMap<String, HashMap<String, String>>,
    discovered: &mut HashSet<String>,
    resolve_type: F,
) where
    F: Fn(&str) -> String + Copy,
{
    let referenced = collect_referenced_fsms(fsm, struct_map, resolve_type);

    for sub_name in referenced {
        if all_fsms.contains_key(&sub_name) && !discovered.contains(&sub_name) {
            discovered.insert(sub_name.clone());
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                collect_all_subfsms(sub_fsm, all_fsms, struct_map, discovered, resolve_type);
            }
        }
    }
}
