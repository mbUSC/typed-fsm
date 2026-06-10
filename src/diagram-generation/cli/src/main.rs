use clap::Parser;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use ra_ap_syntax::{ast::{self, AstNode, HasName}, match_ast, SourceFile, Edition};
use typed_fsm_diagram_core::{
    generate_mermaid_hierarchical, generate_mermaid_simple, FsmDefinition, SubFsmExtractor, parse_macro_body
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
    found_functions: HashMap<String, HashSet<String>>,
    found_aliases: HashMap<String, String>,
}

impl WorkspaceFinder {
    fn scan_node(&mut self, node: ra_ap_syntax::SyntaxNode) {
        for child in node.descendants() {
            match_ast! {
                match child {
                    ast::MacroCall(it) => {
                        if it.path().map(|p| p.syntax().text().to_string() == "state_machine").unwrap_or(false) {
                            if let Some(token_tree) = it.token_tree() {
                                if let Some(fsm) = parse_macro_body(token_tree) {
                                    self.found_fsms.push(fsm);
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
                        if let Some(name) = it.name() {
                            let func_name = name.text().to_string();
                            let mut mentions = HashSet::new();
                            if let Some(body) = it.body() {
                                for descendant in body.syntax().descendants() {
                                    if let Some(path) = ast::Path::cast(descendant) {
                                        let path_str = path.syntax().text().to_string().replace(" ", "");
                                        if path_str.contains("::") {
                                            mentions.insert(path_str);
                                        }
                                    }
                                }
                            }
                            self.found_functions.insert(func_name, mentions);
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
    finder.scan_node(parse.tree().syntax().clone());
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
        found_functions: HashMap::new(),
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

    // Clean start: Delete output directory if it exists to remove stale diagrams
    if Path::new(&output_dir).exists() {
        println!("Cleaning output directory: {}", output_dir);
        fs::remove_dir_all(&output_dir)?;
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
        let mut extractor = SubFsmExtractor::new(fsm.name.clone());
        for state in &fsm.states {
            if let Some(entry) = &state.entry_block {
                let parse = SourceFile::parse(entry, Edition::Edition2021);
                extractor.extract(&parse.tree().syntax());
            }
            let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
            extractor.extract(&parse.tree().syntax());
            if let Some(exit) = &state.exit_block {
                let parse = SourceFile::parse(exit, Edition::Edition2021);
                extractor.extract(&parse.tree().syntax());
            }
            for (_, f_type) in &state.fields {
                let parse = SourceFile::parse(f_type, Edition::Edition2021);
                extractor.extract(&parse.tree().syntax());
            }
        }

        // 1. Explicit discovery
        for child in &extractor.discovered {
            let resolved = finder.resolve_type(child);
            all_children.insert(resolved);
        }

        // 2. Contextual discovery
        if let Some(ctx_type) = &fsm.context_type {
            let ctx_name = finder.resolve_type(&ctx_type.replace(" ", ""));
            if let Some(fields) = finder.found_structs.get(&ctx_name) {
                for field_name in extractor.context_fields {
                    if let Some(type_name) = fields.get(&field_name) {
                        let resolved_type = finder.resolve_type(type_name);
                        let base_type = resolved_type.split("::").last().unwrap_or(&resolved_type);
                        if fsm_map.contains_key(base_type) {
                            all_children.insert(base_type.to_string());
                        }
                    }
                }
            }
        }
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
                generate_mermaid_simple(fsm, include_guards, &finder.found_functions)
            }
            DiagramMode::Hierarchical => generate_mermaid_hierarchical(
                fsm,
                &fsm_map,
                &finder.found_structs,
                include_guards,
                &finder.found_functions,
                resolver,
            ),
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
    function_mentions: &HashMap<String, HashSet<String>>,
    resolve_type: F,
) -> anyhow::Result<()> 
where F: Fn(&str) -> String + Copy
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
                    function_mentions,
                    resolve_type,
                )
            } else {
                generate_mermaid_simple(sub_fsm, include_guards, function_mentions)
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
) where F: Fn(&str) -> String + Copy
{
    let mut extractor = SubFsmExtractor::new(fsm.name.clone());
    for state in &fsm.states {
        if let Some(entry) = &state.entry_block {
            let parse = SourceFile::parse(entry, Edition::Edition2021);
            extractor.extract(&parse.tree().syntax());
        }
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());
        if let Some(exit) = &state.exit_block {
            let parse = SourceFile::parse(exit, Edition::Edition2021);
            extractor.extract(&parse.tree().syntax());
        }
        for (_, f_type) in &state.fields {
            let parse = SourceFile::parse(f_type, Edition::Edition2021);
            extractor.extract(&parse.tree().syntax());
        }
    }

    let mut all_found = HashSet::new();
    for child in extractor.discovered {
        all_found.insert(resolve_type(&child));
    }

    if let Some(ctx_type) = &fsm.context_type {
        let ctx_name = resolve_type(&ctx_type.replace(" ", ""));
        if let Some(fields) = struct_map.get(&ctx_name) {
            for field_name in extractor.context_fields {
                if let Some(type_name) = fields.get(&field_name) {
                    let resolved_type = resolve_type(type_name);
                    let base_type = resolved_type.split("::").last().unwrap_or(&resolved_type);
                    if all_fsms.contains_key(base_type) {
                        all_found.insert(base_type.to_string());
                    }
                }
            }
        }
    }

    for sub_name in all_found {
        if all_fsms.contains_key(&sub_name) && !discovered.contains(&sub_name) {
            discovered.insert(sub_name.clone());
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                collect_all_subfsms(sub_fsm, all_fsms, struct_map, discovered, resolve_type);
            }
        }
    }
}
