use proc_macro::TokenStream;
use quote::quote;
use syn::{parse::{Parse, ParseStream}, parse_macro_input, Token, Ident, braced, Expr, visit::{self, Visit}, Type, Arm, ExprIf};
use std::collections::{HashMap, HashSet};

struct FsmDefinition {
    name: Ident,
    states: Vec<StateDefinition>,
}

struct StateDefinition {
    name: Ident,
    fields: Vec<(Ident, Type)>,
    entry_block: Option<Expr>,
    process_block: Expr,
    exit_block: Option<Expr>,
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

struct TransitionInfo {
    source: String,
    target: String,
    label: String,
}

struct TransitionVisitor {
    fsm_name: String,
    source_state: String,
    current_label: Option<String>,
    transitions: Vec<TransitionInfo>,
}

impl<'ast> Visit<'ast> for TransitionVisitor {
    fn visit_arm(&mut self, i: &'ast Arm) {
        let pat = &i.pat;
        let mut label = clean_tokens(quote!(#pat).to_string());
        if let Some((_, guard)) = &i.guard {
            let guard_str = clean_tokens(quote!(#guard).to_string());
            label.push_str(&format!(" [if {}]", guard_str));
        }
        
        let old_label = self.current_label.take();
        self.current_label = Some(label);
        visit::visit_arm(self, i);
        self.current_label = old_label;
    }

    fn visit_expr_if(&mut self, i: &'ast ExprIf) {
        let cond = &i.cond;
        let label = format!("[if {}]", clean_tokens(quote!(#cond).to_string()));
        
        let old_label = self.current_label.take();
        self.current_label = Some(label);
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

fn clean_tokens(s: String) -> String {
    s.replace('\n', " ")
     .replace('\r', " ")
     .split_whitespace()
     .collect::<Vec<_>>()
     .join(" ")
     .replace(" : ", ":")
     .replace(" :: ", "::")
}

impl TransitionVisitor {
    fn extract_target_state(&mut self, expr: &Expr) -> Option<String> {
        let target = match expr {
            Expr::Path(p) => p.path.segments.last()?.ident.to_string(),
            Expr::Struct(s) => s.path.segments.last()?.ident.to_string(),
            Expr::Call(c) => {
                if let Expr::Path(p) = &*c.func {
                    p.path.segments.last()?.ident.to_string()
                } else {
                    return None;
                }
            }
            _ => {
                let s = quote!(#expr).to_string().replace(" ", "");
                let t = if s.contains("::") {
                    s.split("::").nth(1).unwrap_or(s.split("::").last()?).to_string()
                } else {
                    s
                };
                t.split('(').next()?.split('{').next()?.trim().to_string()
            }
        };
        
        if target == self.fsm_name || target == "Self" {
            None
        } else {
            Some(target)
        }
    }
}

// Visitor to discover behavioral sub-FSMs instantiated in entry/process blocks
struct SubFsmVisitor {
    fsm_name: String,
    discovered: HashSet<String>,
}

impl<'ast> Visit<'ast> for SubFsmVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() >= 2 {
            if let Some(first) = path.segments.first() {
                let first_str = first.ident.to_string();
                
                // Heuristic: Must start with uppercase
                if let Some(first_char) = first_str.chars().next() {
                    if first_char.is_uppercase() {
                        // Avoid common standard library types and the parent FSM itself
                        if first_str != "Self" && first_str != self.fsm_name && first_str != "Transition" && first_str != "Option" && first_str != "Result" && first_str != "String" {
                            
                            // EXCLUSION HEURISTIC: Avoid types that are obviously not FSMs
                            let is_event = first_str.ends_with("Event") || first_str.ends_with("Evt");
                            let is_context = first_str.ends_with("Context") || first_str.ends_with("Ctx");
                            let is_state = first_str.ends_with("State");
                            
                            if !is_event && !is_context && !is_state {
                                // Only include if it's likely a type (CamelCase) or explicitly ends in FSM
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

#[proc_macro]
pub fn generate_diagram(input: TokenStream) -> TokenStream {
    let fsm = parse_macro_input!(input as FsmDefinition);
    let fsm_name = &fsm.name;
    let fsm_name_str = fsm_name.to_string();
    
    let mut node_defs = Vec::new();
    let mut edge_defs = Vec::new();
    let mut subfsm_types = HashMap::new();
    
    for state in &fsm.states {
        let state_name_str = state.name.to_string();
        
        let mut subfsm_quoted = Vec::new();
        
        // 1. Structural Sub-FSMs (from fields)
        for (_, f_type) in &state.fields {
            subfsm_types.insert(quote!(#f_type).to_string(), f_type.clone());
            subfsm_quoted.push(quote! {
                if options.mode == ::typed_fsm::diagram_helpers::DiagramMode::Hierarchical {
                    use ::typed_fsm::diagram_helpers::{FsmMetadataHigh, FsmMetadataLow, Tag};
                    use ::typed_fsm::mermaid_builder::traits::*;
                    let mut sub_builder = ::typed_fsm::mermaid_builder::prelude::StateDiagramBuilder::default();
                    (&&&Tag).populate_child::<#f_type>(&mut sub_builder, options, core::marker::PhantomData)?;
                    node_builder = node_builder.inner_diagram(::typed_fsm::mermaid_builder::prelude::StateDiagram::from(sub_builder))?;
                }
            });
        }
        
        // 2. Behavioral Sub-FSMs (discovered from code blocks)
        let mut subfsm_visitor = SubFsmVisitor {
            fsm_name: fsm_name_str.clone(),
            discovered: HashSet::new(),
        };
        
        if let Some(entry) = &state.entry_block {
            subfsm_visitor.visit_expr(entry);
        }
        subfsm_visitor.visit_expr(&state.process_block);
        if let Some(exit) = &state.exit_block {
            subfsm_visitor.visit_expr(exit);
        }
        
        for discovered_type_str in subfsm_visitor.discovered {
            if let Ok(f_type) = syn::parse_str::<Type>(&discovered_type_str) {
                subfsm_types.insert(discovered_type_str, f_type.clone());
                
                subfsm_quoted.push(quote! {
                    if options.mode == ::typed_fsm::diagram_helpers::DiagramMode::Hierarchical {
                        use ::typed_fsm::diagram_helpers::{FsmMetadataHigh, FsmMetadataLow, Tag};
                        use ::typed_fsm::mermaid_builder::traits::*;
                        let mut sub_builder = ::typed_fsm::mermaid_builder::prelude::StateDiagramBuilder::default();
                        (&&&Tag).populate_child::<#f_type>(&mut sub_builder, options, core::marker::PhantomData)?;
                        node_builder = node_builder.inner_diagram(::typed_fsm::mermaid_builder::prelude::StateDiagram::from(sub_builder))?;
                    }
                });
            }
        }
        
        node_defs.push(quote! {
            let mut node_builder = ::typed_fsm::mermaid_builder::prelude::StateNodeBuilder::default()
                .label(#state_name_str)?;
            #(#subfsm_quoted)*
            let node = builder.node(node_builder)?;
            nodes.insert(#state_name_str, node);
        });

        let mut visitor = TransitionVisitor {
            fsm_name: fsm_name.to_string(),
            source_state: state_name_str.clone(),
            current_label: None,
            transitions: Vec::new(),
        };
        visitor.visit_expr(&state.process_block);
        
        for trans in visitor.transitions {
            let src = trans.source;
            let target = trans.target;
            let label = trans.label;
            edge_defs.push(quote! {
                all_edges.push((#src, #target, #label));
            });
        }
    }

    let mut export_breakdown_flat_calls = Vec::new();
    let mut export_breakdown_nested_calls = Vec::new();
    for (type_str, f_type) in subfsm_types {
        let dir_name = type_str.split("::").last().unwrap().to_string();
        
        export_breakdown_flat_calls.push(quote! {
            {
                use ::typed_fsm::diagram_helpers::{ExportTag, ExportHighPriority, ExportLowPriority};
                let _ = (&&&ExportTag).export_breakdown_flat::<#f_type>(path, options);
            }
        });

        export_breakdown_nested_calls.push(quote! {
            {
                use ::typed_fsm::diagram_helpers::{ExportTag, ExportHighPriority, ExportLowPriority};
                let child_path = path.join(#dir_name);
                let _ = (&&&ExportTag).export_breakdown_nested::<#f_type>(&child_path, options);
            }
        });
    }

    let output = quote! {
        #[cfg(feature = "diagram")]
        impl ::typed_fsm::diagram_helpers::FsmMetadata for #fsm_name {
            fn fsm_name() -> &'static str {
                stringify!(#fsm_name)
            }

            fn populate_diagram(builder: &mut ::typed_fsm::mermaid_builder::prelude::StateDiagramBuilder, options: &::typed_fsm::diagram_helpers::DiagramOptions) -> Result<(), ::std::boxed::Box<dyn ::core::error::Error>> {
                use ::std::collections::HashMap;
                use ::typed_fsm::mermaid_builder::traits::*;
                let mut nodes = HashMap::new();
                let mut all_edges = Vec::new();

                #(#node_defs)*
                #(#edge_defs)*

                for (src_name, dst_name, label) in all_edges {
                    if let (Some(src), Some(dst)) = (nodes.get(src_name), nodes.get(dst_name)) {
                        builder.edge(
                            ::typed_fsm::mermaid_builder::prelude::StateEdgeBuilder::default()
                                .source(src.clone())?
                                .destination(dst.clone())?
                                .label(label)?
                        )?;
                    }
                }
                Ok(())
            }
        }

        #[cfg(all(feature = "diagram", feature = "std"))]
        impl ::typed_fsm::diagram_helpers::FsmExporter for #fsm_name {
            fn fsm_name() -> &'static str {
                stringify!(#fsm_name)
            }

            fn save_diagrams(path: &::std::path::Path, options: &::typed_fsm::diagram_helpers::DiagramOptions) -> ::std::io::Result<()> {
                ::std::fs::create_dir_all(path)?;
                let filename = ::std::format!("{}.mermaid", stringify!(#fsm_name));
                ::std::fs::write(path.join(filename), #fsm_name::mermaid_diagram_ext(options))?;
                
                use ::typed_fsm::diagram_helpers::{BreakdownMode, DiagramMode, DiagramOptions};
                match options.breakdown {
                    BreakdownMode::None => {},
                    BreakdownMode::Flat => {
                        let b_path = path.join("breakdown");
                        let sub_opts = DiagramOptions { 
                            mode: DiagramMode::Flat, 
                            breakdown: BreakdownMode::None, 
                            excluded_types: options.excluded_types 
                        };
                        {
                            let path = &b_path;
                            let options = &sub_opts;
                            #(#export_breakdown_flat_calls)*
                        }
                    }
                    BreakdownMode::Nested => {
                        let b_path = path.join("breakdown");
                        let sub_opts = DiagramOptions { 
                            mode: DiagramMode::Hierarchical, 
                            breakdown: BreakdownMode::None, 
                            excluded_types: options.excluded_types 
                        };
                        {
                            let path = &b_path;
                            let options = &sub_opts;
                            #(#export_breakdown_nested_calls)*
                        }
                    }
                    BreakdownMode::Both => {
                        // Flat
                        let b_path_flat = path.join("breakdown_flat");
                        let sub_opts_flat = DiagramOptions { 
                            mode: DiagramMode::Flat, 
                            breakdown: BreakdownMode::None, 
                            excluded_types: options.excluded_types 
                        };
                        {
                            let path = &b_path_flat;
                            let options = &sub_opts_flat;
                            #(#export_breakdown_flat_calls)*
                        }
                        
                        // Nested
                        let b_path_nested = path.join("breakdown_nested");
                        let sub_opts_nested = DiagramOptions { 
                            mode: DiagramMode::Hierarchical, 
                            breakdown: BreakdownMode::None, 
                            excluded_types: options.excluded_types 
                        };
                        {
                            let path = &b_path_nested;
                            let options = &sub_opts_nested;
                            #(#export_breakdown_nested_calls)*
                        }
                    }
                }
                Ok(())
            }

            fn save_breakdown_flat(path: &::std::path::Path, options: &::typed_fsm::diagram_helpers::DiagramOptions) -> ::std::io::Result<()> {
                ::std::fs::create_dir_all(path)?;
                let filename = ::std::format!("{}.mermaid", stringify!(#fsm_name));
                ::std::fs::write(path.join(filename), #fsm_name::mermaid_diagram_ext(options))?;
                
                #(#export_breakdown_flat_calls)*
                Ok(())
            }

            fn save_breakdown_nested(path: &::std::path::Path, options: &::typed_fsm::diagram_helpers::DiagramOptions) -> ::std::io::Result<()> {
                ::std::fs::create_dir_all(path)?;
                let filename = ::std::format!("{}.mermaid", stringify!(#fsm_name));
                ::std::fs::write(path.join(filename), #fsm_name::mermaid_diagram_ext(options))?;
                
                #(#export_breakdown_nested_calls)*
                Ok(())
            }
        }

        impl #fsm_name {
            /// Generates a Mermaid.js state diagram string for this state machine with default options.
            #[cfg(feature = "diagram")]
            pub fn mermaid_diagram() -> String {
                Self::mermaid_diagram_ext(&::typed_fsm::diagram_helpers::DiagramOptions::default())
            }

            /// Generates a Mermaid.js state diagram string for this state machine with custom options.
            #[cfg(feature = "diagram")]
            pub fn mermaid_diagram_ext(options: &::typed_fsm::diagram_helpers::DiagramOptions) -> String {
                use ::typed_fsm::mermaid_builder::prelude::*;
                let mut builder = StateDiagramBuilder::default();
                let _ = <Self as ::typed_fsm::diagram_helpers::FsmMetadata>::populate_diagram(&mut builder, options);
                StateDiagram::from(builder).to_string()
            }

            /// Recursively exports Mermaid diagrams for this and all sub-FSMs to a directory tree with default options.
            #[cfg(all(feature = "diagram", feature = "std"))]
            pub fn save_diagrams<P: AsRef<::std::path::Path>>(path: P) -> ::std::io::Result<()> {
                Self::save_diagrams_ext(path, &::typed_fsm::diagram_helpers::DiagramOptions::default())
            }

            /// Recursively exports Mermaid diagrams for this and all sub-FSMs to a directory tree with custom options.
            #[cfg(all(feature = "diagram", feature = "std"))]
            pub fn save_diagrams_ext<P: AsRef<::std::path::Path>>(path: P, options: &::typed_fsm::diagram_helpers::DiagramOptions) -> ::std::io::Result<()> {
                use ::typed_fsm::diagram_helpers::FsmExporter;
                <Self as FsmExporter>::save_diagrams(path.as_ref(), options)
            }
        }
    };
    
    output.into()
}
