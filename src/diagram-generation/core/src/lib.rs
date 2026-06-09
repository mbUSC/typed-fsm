use syn::{parse::{Parse, ParseStream}, Token, Ident, braced, Expr, visit::{self, Visit}, Type, Arm, ExprIf};
use quote::quote;
use std::collections::{HashMap, HashSet};
use mermaid_builder::prelude::*;

pub struct FsmDefinition {
    pub name: Ident,
    pub states: Vec<StateDefinition>,
}

pub struct StateDefinition {
    pub name: Ident,
    pub fields: Vec<(Ident, Type)>,
    pub entry_block: Option<Expr>,
    pub process_block: Expr,
    pub exit_block: Option<Expr>,
}

impl Parse for FsmDefinition {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Skip till Name:
        while !input.is_empty() {
            if input.peek(Ident) {
                let fork = input.fork();
                let id: Ident = fork.parse()?;
                if id == "Name" {
                    break;
                }
            }
            input.parse::<proc_macro2::TokenTree>()?;
        }
        let _: Ident = input.parse()?; // "Name"
        input.parse::<Token![:]>()?;
        let name: Ident = input.parse()?;

        // Skip till States:
        while !input.is_empty() {
            if input.peek(Ident) {
                let fork = input.fork();
                let id: Ident = fork.parse()?;
                if id == "States" {
                    break;
                }
            }
            input.parse::<proc_macro2::TokenTree>()?;
        }

        let _: Ident = input.parse()?; // "States"
        input.parse::<Token![:]>()?;
        
        let content;
        braced!(content in input);
        
        let mut states = Vec::new();
        while !content.is_empty() {
            let state_name: Ident = content.parse()?;
            
            let mut fields = Vec::new();
            if content.peek(syn::token::Brace) {
                let field_content;
                braced!(field_content in content);
                while !field_content.is_empty() {
                    let f_name: Ident = field_content.parse()?;
                    field_content.parse::<Token![:]>()?;
                    let f_type: Type = field_content.parse()?;
                    fields.push((f_name, f_type));
                    if field_content.peek(Token![,]) {
                        field_content.parse::<Token![,]>()?;
                    }
                }
            }
            
            content.parse::<Token![=>]>()?;
            
            let state_content;
            braced!(state_content in content);
            
            let mut entry_block = None;
            let mut process_block = None;
            let mut exit_block = None;
            
            while !state_content.is_empty() {
                let fork = state_content.fork();
                if let Ok(key) = fork.parse::<Ident>() {
                    if key == "process" {
                        state_content.parse::<Ident>()?;
                        state_content.parse::<Token![:]>()?;
                        process_block = Some(state_content.parse::<Expr>()?);
                    } else if key == "entry" {
                        state_content.parse::<Ident>()?;
                        state_content.parse::<Token![:]>()?;
                        entry_block = Some(state_content.parse::<Expr>()?);
                    } else if key == "exit" {
                        state_content.parse::<Ident>()?;
                        state_content.parse::<Token![:]>()?;
                        exit_block = Some(state_content.parse::<Expr>()?);
                    } else {
                        state_content.parse::<proc_macro2::TokenTree>()?;
                    }
                } else {
                    state_content.parse::<proc_macro2::TokenTree>()?;
                }
                
                if state_content.peek(Token![,]) {
                    state_content.parse::<Token![,]>()?;
                }
            }
            
            if let Some(pb) = process_block {
                states.push(StateDefinition {
                    name: state_name,
                    fields,
                    entry_block,
                    process_block: pb,
                    exit_block,
                });
            }
            
            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }

        Ok(FsmDefinition { name, states })
    }
}

pub struct TransitionInfo {
    pub source: String,
    pub target: String,
    pub label: String,
}

pub struct TransitionVisitor {
    pub fsm_name: String,
    pub source_state: String,
    pub current_label: Option<String>,
    pub transitions: Vec<TransitionInfo>,
}

impl<'ast> Visit<'ast> for TransitionVisitor {
    fn visit_arm(&mut self, i: &'ast Arm) {
        let pat = &i.pat;
        let mut label = clean_tokens(quote!(#pat).to_string());
        if let Some((_, guard)) = &i.guard {
            let guard_str = clean_tokens(quote!(#guard).to_string());
            label.push_str(&format!(" [if {}]", guard_str));
        }
        
        let old_label = self.current_label.clone();
        if let Some(ref current) = self.current_label {
            self.current_label = Some(format!("{} {}", current, label));
        } else {
            self.current_label = Some(label);
        }
        
        visit::visit_arm(self, i);
        self.current_label = old_label;
    }

    fn visit_expr_if(&mut self, i: &'ast ExprIf) {
        let cond = &i.cond;
        let label = format!("[if {}]", clean_tokens(quote!(#cond).to_string()));
        
        let old_label = self.current_label.clone();
        if let Some(ref current) = self.current_label {
            self.current_label = Some(format!("{} {}", current, label));
        } else {
            self.current_label = Some(label);
        }

        visit::visit_expr_if(self, i);
        self.current_label = old_label;
    }

    fn visit_expr_call(&mut self, i: &'ast syn::ExprCall) {
        if let Expr::Path(ref p) = *i.func {
            let path_str = quote!(#p).to_string().replace(" ", "");
            if path_str.contains("Transition::To") {
                if let Some(arg) = i.args.first() {
                    if let Some(target) = self.extract_target_state(arg) {
                        self.transitions.push(TransitionInfo {
                            source: self.source_state.clone(),
                            target,
                            label: self.current_label.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }
        visit::visit_expr_call(self, i);
    }
}

impl TransitionVisitor {
    pub fn new(fsm_name: String, source_state: String) -> Self {
        Self {
            fsm_name,
            source_state,
            current_label: None,
            transitions: Vec::new(),
        }
    }

    fn extract_target_state(&mut self, expr: &Expr) -> Option<String> {
        let s = quote!(#expr).to_string().replace(" ", "");
        // Clean up target: remove generic params, field initializers, etc.
        let s = s.split('(').next()?.split('{').next()?.trim().to_string();
        
        let target = if s.contains("::") {
            let parts: Vec<&str> = s.split("::").collect();
            if parts.len() >= 2 {
                if parts[0] == self.fsm_name || parts[0] == "Self" {
                    parts.last().unwrap_or(&"").to_string()
                } else {
                    // It's likely SomeOtherFsm::State
                    parts.last().unwrap_or(&"").to_string()
                }
            } else {
                s
            }
        } else {
            s
        };
        
        if target == self.fsm_name || target == "Self" || target.is_empty() {
            None
        } else {
            Some(target)
        }
    }
}

pub fn clean_tokens(s: String) -> String {
    s.replace('\n', " ")
     .replace('\r', " ")
     .split_whitespace()
     .collect::<Vec<_>>()
     .join(" ")
     .replace(" : ", ":")
     .replace(" :: ", "::")
}

pub struct SubFsmVisitor {
    pub fsm_name: String,
    pub discovered: HashSet<String>,
}

impl<'ast> Visit<'ast> for SubFsmVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() >= 2 {
            if let Some(first) = path.segments.first() {
                let first_str = first.ident.to_string();
                
                if let Some(first_char) = first_str.chars().next() {
                    if first_char.is_uppercase() {
                        if first_str != "Self" && first_str != self.fsm_name && first_str != "Transition" && first_str != "Option" && first_str != "Result" && first_str != "String" {
                            
                            let is_event = first_str.ends_with("Event") || first_str.ends_with("Evt");
                            let is_context = first_str.ends_with("Context") || first_str.ends_with("Ctx");
                            let is_state = first_str.ends_with("State");
                            
                            if !is_event && !is_context && !is_state {
                                let is_camel_case = first_str.chars().any(|c| c.is_lowercase());
                                let is_fsm = first_str.ends_with("FSM") || first_str.ends_with("Fsm");
                                
                                if is_camel_case || is_fsm {
                                    self.discovered.insert(first_str);
                                }
                            }
                        }
                    }
                }
            }
        }
        visit::visit_path(self, path);
    }
}

/// Generates a Mermaid.js string for a single FSM (no nesting).
pub fn generate_mermaid_simple(fsm: &FsmDefinition) -> String {
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    for state in &fsm.states {
        let state_name = state.name.to_string();
        let node_builder = StateNodeBuilder::default()
            .label(&state_name).unwrap();
        let node = builder.node(node_builder).unwrap();
        nodes.insert(state_name.clone(), node);

        let mut visitor = TransitionVisitor::new(fsm.name.to_string(), state_name);
        visitor.visit_expr(&state.process_block);
        
        for trans in visitor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            builder.edge(
                StateEdgeBuilder::default()
                    .source(src.clone()).unwrap()
                    .destination(dst.clone()).unwrap()
                    .label(&trans.label).unwrap()
            ).unwrap();
        }
    }

    StateDiagram::from(builder).to_string()
}

/// Generates a hierarchical Mermaid diagram, inlining sub-FSMs from the provided map.
pub fn generate_mermaid_hierarchical(
    fsm: &FsmDefinition, 
    all_fsms: &HashMap<String, &FsmDefinition>,
) -> String {
    let builder = populate_builder_hierarchical(fsm, all_fsms);
    StateDiagram::from(builder).to_string()
}

fn populate_builder_hierarchical(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
) -> StateDiagramBuilder {
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    for state in &fsm.states {
        let state_name = state.name.to_string();
        let mut node_builder = StateNodeBuilder::default()
            .label(&state_name).unwrap();

        // Discover sub-FSMs for this state
        let mut subfsm_visitor = SubFsmVisitor {
            fsm_name: fsm.name.to_string(),
            discovered: HashSet::new(),
        };
        if let Some(entry) = &state.entry_block { subfsm_visitor.visit_expr(entry); }
        subfsm_visitor.visit_expr(&state.process_block);
        if let Some(exit) = &state.exit_block { subfsm_visitor.visit_expr(exit); }
        for (_, f_type) in &state.fields { subfsm_visitor.visit_type(f_type); }

        for sub_name in subfsm_visitor.discovered {
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                let sub_builder = populate_builder_hierarchical(sub_fsm, all_fsms);
                node_builder = node_builder.inner_diagram(StateDiagram::from(sub_builder)).unwrap();
            }
        }

        let node = builder.node(node_builder).unwrap();
        nodes.insert(state_name.clone(), node);

        let mut trans_visitor = TransitionVisitor::new(fsm.name.to_string(), state_name);
        trans_visitor.visit_expr(&state.process_block);
        
        for trans in trans_visitor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            builder.edge(
                StateEdgeBuilder::default()
                    .source(src.clone()).unwrap()
                    .destination(dst.clone()).unwrap()
                    .label(&trans.label).unwrap()
            ).unwrap();
        }
    }

    builder
}
