use std::path::Path;
use std::fs;
use std::collections::{HashMap, HashSet};
use clap::Parser;
use serde::Deserialize;
use syn::{visit::{self, Visit}, Macro, ItemStruct, Fields};
use quote::quote;
use walkdir::WalkDir;
use typed_fsm_diagram_core::{FsmDefinition, generate_mermaid_simple, generate_mermaid_hierarchical, SubFsmVisitor};

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

fn default_scan_dir() -> String { "src".to_string() }
fn default_output_dir() -> String { "target/docs/diagrams".to_string() }
fn default_mode() -> DiagramMode { DiagramMode::Hierarchical }
fn default_breakdown() -> BreakdownMode { BreakdownMode::Flat }

struct WorkspaceFinder {
    found_fsms: Vec<FsmDefinition>,
    found_structs: HashMap<String, HashMap<String, String>>,
    found_functions: HashMap<String, HashSet<String>>,
}

impl<'ast> Visit<'ast> for WorkspaceFinder {
    fn visit_macro(&mut self, i: &'ast Macro) {
        if i.path.is_ident("state_machine") {
            if let Ok(fsm) = i.parse_body::<FsmDefinition>() {
                self.found_fsms.push(fsm);
            }
        }
        visit::visit_macro(self, i);
    }

    fn visit_item_struct(&mut self, i: &'ast ItemStruct) {
        let struct_name = i.ident.to_string();
        let mut fields = HashMap::new();
        if let Fields::Named(ref named) = i.fields {
            for field in &named.named {
                if let Some(ref ident) = field.ident {
                    let field_name = ident.to_string();
                    let ty = &field.ty;
                    let type_name = quote!(#ty).to_string().replace(" ", "");
                    fields.insert(field_name, type_name);
                }
            }
        }
        self.found_structs.insert(struct_name, fields);
        visit::visit_item_struct(self, i);
    }

    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        let func_name = i.sig.ident.to_string();
        let mut mentions = HashSet::new();
        let mut visitor = MentionVisitor { mentions: &mut mentions };
        visitor.visit_block(&i.block);
        self.found_functions.insert(func_name, mentions);
        visit::visit_item_fn(self, i);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        let func_name = i.sig.ident.to_string();
        let mut mentions = HashSet::new();
        let mut visitor = MentionVisitor { mentions: &mut mentions };
        visitor.visit_block(&i.block);
        self.found_functions.insert(func_name, mentions);
        visit::visit_impl_item_fn(self, i);
    }
}

struct MentionVisitor<'a> {
    mentions: &'a mut HashSet<String>,
}

impl<'ast, 'a> Visit<'ast> for MentionVisitor<'a> {
    fn visit_path(&mut self, i: &'ast syn::Path) {
        if i.segments.len() >= 2 {
            // Check for StateMachine::State or Self::State
            let path_str = quote!(#i).to_string().replace(" ", "");
            self.mentions.insert(path_str);
        }
        visit::visit_path(self, i);
    }
}

fn process_file(path: &Path, finder: &mut WorkspaceFinder) -> anyhow::Result<()> {
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
    let src_input = args.scan.unwrap_or(config.mermaid.scan_dir.clone());
    let include_guards = args.include_guards.unwrap_or(config.mermaid.include_guards);
    
    let src_path = Path::new(&src_input);
    if !src_path.exists() {
        anyhow::bail!("Error: Source path '{}' does not exist.", src_input);
    }

    println!("Scanning {} for state_machine! macros and context structs...", src_input);
    let mut finder = WorkspaceFinder { 
        found_fsms: vec![], 
        found_structs: HashMap::new(),
        found_functions: HashMap::new(),
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

    println!("Found {} FSM definitions, {} struct definitions, and {} functions.", 
        finder.found_fsms.len(), finder.found_structs.len(), finder.found_functions.len());
    
    let fsm_map: HashMap<String, &FsmDefinition> = finder.found_fsms.iter().map(|f| (f.name.to_string(), f)).collect();

    // Identify Root FSMs (those not referenced by any other FSM)
    let mut all_children = HashSet::new();
    for fsm in &finder.found_fsms {
        let mut visitor = SubFsmVisitor {
            fsm_name: fsm.name.to_string(),
            discovered: HashSet::new(),
            context_fields: HashSet::new(),
        };
        for state in &fsm.states {
            if let Some(entry) = &state.entry_block { visitor.visit_expr(entry); }
            visitor.visit_expr(&state.process_block);
            if let Some(exit) = &state.exit_block { visitor.visit_expr(exit); }
            for (_, f_type) in &state.fields { visitor.visit_type(f_type); }
        }

        // 1. Explicit discovery
        for child in &visitor.discovered {
            all_children.insert(child.clone());
        }

        // 2. Contextual discovery
        if let Some(ctx_type) = &fsm.context_type {
            let ctx_name = quote!(#ctx_type).to_string().replace(" ", "");
            if let Some(fields) = finder.found_structs.get(&ctx_name) {
                for field_name in visitor.context_fields {
                    if let Some(type_name) = fields.get(&field_name) {
                        let base_type = type_name.split("::").last().unwrap_or(type_name);
                        if fsm_map.contains_key(base_type) {
                            all_children.insert(base_type.to_string());
                        }
                    }
                }
            }
        }
    }

    let root_fsms: Vec<&FsmDefinition> = finder.found_fsms.iter()
        .filter(|f| !all_children.contains(&f.name.to_string()))
        .collect();

    println!("Identified {} root FSM(s) for organization.", root_fsms.len());

    for fsm in root_fsms {
        let name = fsm.name.to_string();
        let fsm_output_dir = Path::new(&output_dir).join(&name);
        fs::create_dir_all(&fsm_output_dir)?;
        
        let content = match config.mermaid.mode {
            DiagramMode::Simple => generate_mermaid_simple(fsm, include_guards, &finder.found_functions),
            DiagramMode::Hierarchical => generate_mermaid_hierarchical(fsm, &fsm_map, &finder.found_structs, include_guards, &finder.found_functions),
        };
        
        let path = fsm_output_dir.join(format!("{}.mermaid", name));
        fs::write(&path, content)?;
        println!("Generated root: {}", path.display());

        match config.mermaid.breakdown {
            BreakdownMode::Flat => save_breakdown(fsm, &fsm_map, &finder.found_structs, &fsm_output_dir, "breakdown", false, include_guards, &finder.found_functions)?,
            BreakdownMode::Nested => save_breakdown(fsm, &fsm_map, &finder.found_structs, &fsm_output_dir, "breakdown", true, include_guards, &finder.found_functions)?,
            BreakdownMode::Both => {
                save_breakdown(fsm, &fsm_map, &finder.found_structs, &fsm_output_dir, "breakdown_flat", false, include_guards, &finder.found_functions)?;
                save_breakdown(fsm, &fsm_map, &finder.found_structs, &fsm_output_dir, "breakdown_nested", true, include_guards, &finder.found_functions)?;
            },
            BreakdownMode::None => {},
        }
    }
    
    Ok(())
}

fn save_breakdown(
    fsm: &FsmDefinition, 
    all_fsms: &HashMap<String, &FsmDefinition>, 
    struct_map: &HashMap<String, HashMap<String, String>>,
    fsm_output_dir: &Path, 
    sub_dir: &str, 
    nested: bool,
    include_guards: bool,
    function_mentions: &HashMap<String, HashSet<String>>,
) -> anyhow::Result<()> {
    let mut discovered = HashSet::new();
    collect_all_subfsms(fsm, all_fsms, struct_map, &mut discovered);

    if discovered.is_empty() {
        return Ok(());
    }

    let target_dir = fsm_output_dir.join(sub_dir);
    fs::create_dir_all(&target_dir)?;

    for sub_name in discovered {
        if let Some(sub_fsm) = all_fsms.get(&sub_name) {
            let content = if nested {
                generate_mermaid_hierarchical(sub_fsm, all_fsms, struct_map, include_guards, function_mentions)
            } else {
                generate_mermaid_simple(sub_fsm, include_guards, function_mentions)
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
    struct_map: &HashMap<String, HashMap<String, String>>,
    discovered: &mut HashSet<String>
) {
    let mut visitor = SubFsmVisitor {
        fsm_name: fsm.name.to_string(),
        discovered: HashSet::new(),
        context_fields: HashSet::new(),
    };
    for state in &fsm.states {
        if let Some(entry) = &state.entry_block { visitor.visit_expr(entry); }
        visitor.visit_expr(&state.process_block);
        if let Some(exit) = &state.exit_block { visitor.visit_expr(exit); }
        for (_, f_type) in &state.fields { visitor.visit_type(f_type); }
    }

    let mut all_found = visitor.discovered;
    if let Some(ctx_type) = &fsm.context_type {
        let ctx_name = quote!(#ctx_type).to_string().replace(" ", "");
        if let Some(fields) = struct_map.get(&ctx_name) {
            for field_name in visitor.context_fields {
                if let Some(type_name) = fields.get(&field_name) {
                    let base_type = type_name.split("::").last().unwrap_or(type_name);
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
                collect_all_subfsms(sub_fsm, all_fsms, struct_map, discovered);
            }
        }
    }
}
