use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Type, visit::Visit};
use std::collections::{HashMap, HashSet};
use typed_fsm_diagram_core::{FsmDefinition, TransitionVisitor, SubFsmVisitor};

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
            context_fields: HashSet::new(),
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

        // Collect transitions twice to support toggling guards at runtime
        let mut visitor_full = TransitionVisitor::new(fsm_name_str.clone(), state_name_str.clone(), true);
        visitor_full.visit_expr(&state.process_block);
        
        let mut visitor_clean = TransitionVisitor::new(fsm_name_str.clone(), state_name_str.clone(), false);
        visitor_clean.visit_expr(&state.process_block);
        
        for (full, clean) in visitor_full.transitions.into_iter().zip(visitor_clean.transitions.into_iter()) {
            let src = full.source;
            let target = full.target;
            let label_full = full.label;
            let label_clean = clean.label;
            edge_defs.push(quote! {
                let label = if options.include_guards { #label_full } else { #label_clean };
                all_edges.push((#src, #target, label));
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
                            mode: ::typed_fsm::diagram_helpers::DiagramMode::Simple,
 
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
                            mode: ::typed_fsm::diagram_helpers::DiagramMode::Simple,
 
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
