use mermaid_builder::prelude::*;
use ra_ap_syntax::{
    ast::{self, AstNode, HasArgList},
    match_ast, Edition, SourceFile, SyntaxKind, SyntaxNode, SyntaxToken, T,
};
use std::collections::{HashMap, HashSet};

pub struct FsmDefinition {
    pub name: String,
    pub context_type: Option<String>,
    pub states: Vec<StateDefinition>,
}

pub struct StateDefinition {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub entry_block: Option<String>,
    pub process_block: String,
    pub exit_block: Option<String>,
}

pub fn clean_tokens(s: String) -> String {
    let mut res = s.replace('\n', " ").replace('\r', " ");

    res = res.replace("{", " { ").replace("}", " } ").replace(",", " , ");

    // Use natural language for logical operators and space out others
    res = res
        .replace("==", " == ")
        .replace("!=", " != ")
        .replace(">=", " >= ")
        .replace("<=", " <= ")
        .replace("&&", " and ")
        .replace("||", " or ")
        .replace("|", " | ")
        .replace("=", " = ")
        .replace(">", " > ")
        .replace("<", " < ")
        .replace("+", " + ")
        .replace("-", " - ")
        .replace("*", " * ")
        .replace("/", " / ");

    res.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("= >", "=>")
        .replace("- >", "->")
        .replace("+ =", "+=")
        .replace("- =", "-=")
        .replace("* =", "*=")
        .replace("/ =", "/=")
        .replace("! =", "!=")
        .replace("< =", "<=")
        .replace("> =", ">=")
        .replace("= =", "==")
        .replace(" : ", ":")
        .replace(" :: ", "::")
        .replace(" . ", ".")
        .replace(" .", ".")
        .replace(". ", ".")
        .replace(" ( ", "(")
        .replace(" ) ", ")")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" , ", ", ")
        .replace(" & ", "&")
        .replace(" ; ", ";")
        .replace(" [ ", "[")
        .replace(" ] ", "]")
        .replace(", }", " }") // Remove trailing comma before closing brace
        .replace(" { ", "<br/>{ ")
        .replace(" }", " }")
        .replace("! ", "!")
}

pub struct TransitionInfo {
    pub source: String,
    pub target: String,
    pub label: String,
}

pub struct TransitionExtractor<'a> {
    pub fsm_name: String,
    pub source_state: String,
    pub current_label: Option<String>,
    pub transitions: Vec<TransitionInfo>,
    pub include_guards: bool,
    pub function_mentions: &'a HashMap<String, HashSet<String>>,
    pub visited_functions: HashSet<String>,
}

impl<'a> TransitionExtractor<'a> {
    pub fn new(
        fsm_name: String,
        source_state: String,
        include_guards: bool,
        function_mentions: &'a HashMap<String, HashSet<String>>,
    ) -> Self {
        Self {
            fsm_name,
            source_state,
            current_label: None,
            transitions: Vec::new(),
            include_guards,
            function_mentions,
            visited_functions: HashSet::new(),
        }
    }

    fn merge_labels(&self, current: Option<String>, new: String, is_guard: bool) -> String {
        if let Some(c) = current {
            if is_guard {
                if c.contains("<br/>[ if ") {
                    // Combine with existing guard
                    let parts: Vec<&str> = c.split("<br/>[ if ").collect();
                    let event = parts[0];
                    let mut guard = parts[1].trim_end_matches(" ]").to_string();
                    guard.push_str(" and ");
                    guard.push_str(&new);
                    format!("{}<br/>[ if {} ]", event, guard)
                } else {
                    // Add new guard to event
                    format!("{}<br/>[ if {} ]", c, new)
                }
            } else {
                // Combine event names
                format!("{} {}", c, new)
            }
        } else if is_guard {
            format!("<br/>[ if {} ]", new)
        } else {
            new
        }
    }

    pub fn extract(&mut self, node: &SyntaxNode) {
        match_ast! {
            match node {
                ast::MatchArm(it) => {
                    let pat = it.pat().map(|p| p.syntax().text().to_string()).unwrap_or_default();
                    let pat_label = clean_tokens(pat);
                    
                    let old_label = self.current_label.clone();
                    self.current_label = Some(self.merge_labels(old_label.clone(), pat_label, false));

                    if self.include_guards {
                        if let Some(guard) = it.guard() {
                            let guard_str = clean_tokens(guard.syntax().text().to_string());
                            let mid_label = self.current_label.clone();
                            self.current_label = Some(self.merge_labels(mid_label, guard_str, true));
                        }
                    }

                    if let Some(expr) = it.expr() {
                        self.extract(expr.syntax());
                    }

                    self.current_label = old_label;
                },
                ast::IfExpr(it) => {
                    let cond_str = if let Some(cond) = it.condition() {
                        clean_tokens(cond.syntax().text().to_string())
                    } else {
                        String::new()
                    };

                    let old_label = self.current_label.clone();
                    
                    if !cond_str.is_empty() {
                        if self.include_guards {
                             // Heuristic: if it looks like an event match (if let Event::X = evt)
                             // we treat it as an event, otherwise as a guard.
                             let is_event = cond_str.contains("let") && (cond_str.contains("evt") || cond_str.contains("event"));
                             self.current_label = Some(self.merge_labels(old_label.clone(), cond_str, !is_event));
                        }
                    }

                    if let Some(block) = it.then_branch() {
                        self.extract(block.syntax());
                    }
                    if let Some(else_branch) = it.else_branch() {
                        match else_branch {
                            ast::ElseBranch::Block(b) => self.extract(b.syntax()),
                            ast::ElseBranch::IfExpr(e) => self.extract(e.syntax()),
                        }
                    }

                    self.current_label = old_label;
                },
                ast::CallExpr(it) => {
                    if let Some(expr) = it.expr() {
                        let path_str = expr.syntax().text().to_string().replace(" ", "");
                        if path_str.contains("Transition::To") {
                            if let Some(arg_list) = it.arg_list() {
                                if let Some(arg) = arg_list.args().next() {
                                    if let Some(target) = self.extract_target_state(&arg.syntax()) {
                                        self.transitions.push(TransitionInfo {
                                            source: self.source_state.clone(),
                                            target,
                                            label: self
                                                .current_label
                                                .clone()
                                                .unwrap_or_default()
                                                .trim()
                                                .to_string(),
                                        });
                                    }
                                }
                            }
                        } else {
                            let func_name = path_str.split("::").last().unwrap_or(&path_str).to_string();
                            self.follow_function(&func_name);
                        }
                    }
                    for child in node.children() {
                        self.extract(&child);
                    }
                },
                ast::MethodCallExpr(it) => {
                    if let Some(name) = it.name_ref() {
                        let method_name = name.text().to_string();
                        self.follow_function(&method_name);
                    }
                    for child in node.children() {
                        self.extract(&child);
                    }
                },
                _ => {
                    for child in node.children() {
                        self.extract(&child);
                    }
                }
            }
        }
    }

    fn follow_function(&mut self, func_name: &str) {
        if self.visited_functions.contains(func_name) {
            return;
        }
        self.visited_functions.insert(func_name.to_string());

        if let Some(mentions) = self.function_mentions.get(func_name) {
            for target in mentions {
                if target.starts_with(&format!("{}::", self.fsm_name))
                    || target.starts_with("Self::")
                {
                    let state_name = target.split("::").last().unwrap_or(target).to_string();
                    if state_name != self.fsm_name && state_name != "Self" {
                        self.transitions.push(TransitionInfo {
                            source: self.source_state.clone(),
                            target: state_name,
                            label: self
                                .current_label
                                .clone()
                                .unwrap_or_else(|| format!("(via {})", func_name))
                                .trim()
                                .to_string(),
                        });
                    }
                }
            }
        }
    }

    fn extract_target_state(&mut self, node: &SyntaxNode) -> Option<String> {
        let s = node.text().to_string().replace(" ", "");
        let s = s.split('(').next()?.split('{').next()?.trim().to_string();

        let target = if s.contains("::") {
            let parts: Vec<&str> = s.split("::").collect();
            if parts.len() >= 2 {
                if parts[0] == self.fsm_name || parts[0] == "Self" {
                    parts.last().unwrap_or(&"").to_string()
                } else {
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

pub struct SubFsmExtractor {
    pub fsm_name: String,
    pub discovered: HashSet<String>,
    pub context_fields: HashSet<String>,
}

impl SubFsmExtractor {
    pub fn new(fsm_name: String) -> Self {
        Self {
            fsm_name,
            discovered: HashSet::new(),
            context_fields: HashSet::new(),
        }
    }

    pub fn extract(&mut self, node: &SyntaxNode) {
        for child in node.descendants() {
            match_ast! {
                match child {
                    ast::Path(path) => {
                        let path_str = path.syntax().text().to_string().replace(" ", "");
                        let segments: Vec<&str> = path_str.split("::").collect();
                        if segments.len() >= 2 {
                            let first_str = segments[0];
                            if let Some(first_char) = first_str.chars().next() {
                                if first_char.is_uppercase() {
                                    if first_str != "Self"
                                        && first_str != self.fsm_name
                                        && first_str != "Transition"
                                        && first_str != "Option"
                                        && first_str != "Result"
                                        && first_str != "String"
                                    {
                                        let is_event = first_str.ends_with("Event") || first_str.ends_with("Evt");
                                        let is_context = first_str.ends_with("Context") || first_str.ends_with("Ctx");
                                        let is_state = first_str.ends_with("State");

                                        if !is_event && !is_context && !is_state {
                                            let is_camel_case = first_str.chars().any(|c| c.is_lowercase());
                                            let is_fsm = first_str.ends_with("FSM") || first_str.ends_with("Fsm");

                                            if is_camel_case || is_fsm {
                                                self.discovered.insert(first_str.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    ast::FieldExpr(it) => {
                        if let Some(name) = it.name_ref() {
                            self.context_fields.insert(name.text().to_string());
                        }
                    },
                    _ => {}
                }
            }
        }
    }
}

pub fn parse_macro_body(token_tree: ast::TokenTree) -> Option<FsmDefinition> {
    let mut name = None;
    let mut context_type = None;
    let mut states = Vec::new();

    let tokens: Vec<SyntaxToken> = token_tree
        .syntax()
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| t.kind() != SyntaxKind::WHITESPACE && t.kind() != SyntaxKind::COMMENT)
        .collect();

    let mut i = 0;
    if i < tokens.len() && tokens[i].kind() == T!['{'] {
        i += 1;
    }

    while i < tokens.len() && tokens[i].kind() != T!['}'] {
        let token = &tokens[i];
        let text = token.text();

        if text == "Name" {
            i += 1;
            if i < tokens.len() && tokens[i].kind() == T![:] {
                i += 1;
                if i < tokens.len() {
                    name = Some(tokens[i].text().to_string());
                    i += 1;
                }
            }
        } else if text == "Context" {
            i += 1;
            if i < tokens.len() && tokens[i].kind() == T![:] {
                i += 1;
                let mut ty = String::new();
                while i < tokens.len()
                    && tokens[i].kind() != T![,]
                    && tokens[i].text() != "Event"
                    && tokens[i].text() != "States"
                {
                    ty.push_str(tokens[i].text());
                    i += 1;
                }
                context_type = Some(ty);
            }
        } else if text == "States" {
            i += 1;
            if i < tokens.len() && tokens[i].kind() == T![:] {
                i += 1;
                if i < tokens.len() && tokens[i].kind() == T!['{'] {
                    i += 1;
                    while i < tokens.len() && tokens[i].kind() != T!['}'] {
                        let state_name = tokens[i].text().to_string();
                        i += 1;

                        let mut fields = Vec::new();
                        if i < tokens.len() && tokens[i].kind() == T!['{'] {
                            i += 1;
                            while i < tokens.len() && tokens[i].kind() != T!['}'] {
                                let f_name = tokens[i].text().to_string();
                                i += 1;
                                if i < tokens.len() && tokens[i].kind() == T![:] {
                                    i += 1;
                                    let mut f_type = String::new();
                                    while i < tokens.len()
                                        && tokens[i].kind() != T![,]
                                        && tokens[i].kind() != T!['}']
                                    {
                                        f_type.push_str(tokens[i].text());
                                        i += 1;
                                    }
                                    fields.push((f_name, f_type));
                                }
                                if i < tokens.len() && tokens[i].kind() == T![,] {
                                    i += 1;
                                }
                            }
                            if i < tokens.len() && tokens[i].kind() == T!['}'] {
                                i += 1;
                            }
                        }

                        if i < tokens.len() && tokens[i].kind() == T![=>] {
                            i += 1;
                        } else if i + 1 < tokens.len()
                            && tokens[i].kind() == T![=]
                            && tokens[i + 1].kind() == T![>]
                        {
                            i += 2;
                        }

                        if i < tokens.len() && tokens[i].kind() == T!['{'] {
                            let start = i;
                            let mut depth = 0;
                            while i < tokens.len() {
                                if tokens[i].kind() == T!['{'] {
                                    depth += 1;
                                } else if tokens[i].kind() == T!['}'] {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                i += 1;
                            }
                            let end = i;
                            if i < tokens.len() {
                                i += 1;
                            }

                            let mut entry_block = None;
                            let mut process_block = None;
                            let mut exit_block = None;

                            let mut j = start + 1;
                            while j < end {
                                let key = tokens[j].text();
                                if key == "entry" || key == "process" || key == "exit" {
                                    let current_key = key.to_string();
                                    j += 1;
                                    if j < end && tokens[j].kind() == T![:] {
                                        j += 1;
                                        let mut block_text = String::new();
                                        let mut inner_depth = 0;
                                        let mut pipe_count = 0;
                                        while j < end {
                                            let tk = tokens[j].kind();
                                            if tk == T!['{'] || tk == T!['('] || tk == T!['['] {
                                                inner_depth += 1;
                                            } else if tk == T!['}']
                                                || tk == T![')']
                                                || tk == T![']']
                                            {
                                                inner_depth -= 1;
                                            } else if tk == T![|] {
                                                pipe_count += 1;
                                            } else if inner_depth == 0
                                                && (pipe_count == 0 || pipe_count >= 2)
                                            {
                                                if tk == T![,] {
                                                    break;
                                                }
                                                if j + 1 < end && tokens[j + 1].kind() == T![:] {
                                                    let t = tokens[j].text();
                                                    if t == "process" || t == "exit" || t == "entry"
                                                    {
                                                        break;
                                                    }
                                                }
                                            }
                                            block_text.push_str(tokens[j].text());
                                            if j + 1 < end
                                                && !tokens[j + 1].kind().is_punct()
                                                && !tokens[j].kind().is_punct()
                                            {
                                                block_text.push(' ');
                                            }
                                            j += 1;
                                        }
                                        match current_key.as_str() {
                                            "entry" => entry_block = Some(block_text),
                                            "process" => process_block = Some(block_text),
                                            "exit" => exit_block = Some(block_text),
                                            _ => {}
                                        }
                                        if j < end && tokens[j].kind() == T![,] {
                                            j += 1;
                                        }
                                        continue;
                                    }
                                }
                                j += 1;
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
                        }

                        if i < tokens.len() && tokens[i].kind() == T![,] {
                            i += 1;
                        }
                    }
                    if i < tokens.len() && tokens[i].kind() == T!['}'] {
                        i += 1;
                    }
                }
            }
        } else if tokens[i].kind() == T![,] {
            i += 1;
        } else {
            i += 1;
        }
    }

    Some(FsmDefinition {
        name: name?,
        context_type,
        states,
    })
}

pub fn generate_mermaid_simple(
    fsm: &FsmDefinition,
    include_guards: bool,
    function_mentions: &HashMap<String, HashSet<String>>,
) -> String {
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    for state in &fsm.states {
        let state_name = state.name.clone();
        let node_builder = StateNodeBuilder::default().label(&state_name).unwrap();
        let node = builder.node(node_builder).unwrap();
        nodes.insert(state_name.clone(), node);

        let mut extractor = TransitionExtractor::new(
            fsm.name.clone(),
            state_name,
            include_guards,
            function_mentions,
        );
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());

        for trans in extractor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            let label = if trans.label.is_empty() {
                "*".to_string()
            } else {
                trans.label.replace(":", "#colon;")
            };
            builder
                .edge(
                    StateEdgeBuilder::default()
                        .source(src.clone())
                        .unwrap()
                        .destination(dst.clone())
                        .unwrap()
                        .label(&label)
                        .unwrap(),
                )
                .unwrap();
        }
    }

    StateDiagram::from(builder)
        .to_string()
        .replace("\r\n", "\n")
        .replace("    direction LR\n", "")
}

fn populate_builder_hierarchical<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    include_guards: bool,
    function_mentions: &HashMap<String, HashSet<String>>,
    resolve_type: F,
) -> StateDiagramBuilder
where
    F: Fn(&str) -> String + Copy,
{
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    let context_fields = if let Some(ctx_type) = &fsm.context_type {
        let ctx_name = resolve_type(&ctx_type.replace(" ", ""));
        context_struct_map.get(&ctx_name)
    } else {
        None
    };

    for state in &fsm.states {
        let state_name = state.name.clone();
        let mut node_builder = StateNodeBuilder::default().label(&state_name).unwrap();

        let mut subfsm_extractor = SubFsmExtractor::new(fsm.name.clone());
        if let Some(entry) = &state.entry_block {
            let parse = SourceFile::parse(entry, Edition::Edition2021);
            subfsm_extractor.extract(&parse.tree().syntax());
        }
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        subfsm_extractor.extract(&parse.tree().syntax());
        if let Some(exit) = &state.exit_block {
            let parse = SourceFile::parse(exit, Edition::Edition2021);
            subfsm_extractor.extract(&parse.tree().syntax());
        }
        for (_, f_type) in &state.fields {
            let parse = SourceFile::parse(f_type, Edition::Edition2021);
            subfsm_extractor.extract(&parse.tree().syntax());
        }

        let mut all_discovered = HashSet::new();
        for child in subfsm_extractor.discovered {
            all_discovered.insert(resolve_type(&child));
        }

        if let Some(fields) = context_fields {
            for field_name in subfsm_extractor.context_fields {
                if let Some(type_name) = fields.get(&field_name) {
                    let resolved_type = resolve_type(type_name);
                    let base_type = resolved_type.split("::").last().unwrap_or(&resolved_type);
                    if all_fsms.contains_key(base_type) {
                        all_discovered.insert(base_type.to_string());
                    }
                }
            }
        }

        for sub_name in all_discovered {
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                let sub_builder = populate_builder_hierarchical(
                    sub_fsm,
                    all_fsms,
                    context_struct_map,
                    include_guards,
                    function_mentions,
                    resolve_type,
                );
                node_builder = node_builder
                    .inner_diagram(StateDiagram::from(sub_builder))
                    .unwrap();
            }
        }

        let node = builder.node(node_builder).unwrap();
        nodes.insert(state_name.clone(), node);

        let mut trans_extractor = TransitionExtractor::new(
            fsm.name.clone(),
            state_name,
            include_guards,
            function_mentions,
        );
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        trans_extractor.extract(&parse.tree().syntax());

        for trans in trans_extractor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            let label = if trans.label.is_empty() {
                "*".to_string()
            } else {
                trans.label.replace(":", "#colon;")
            };
            builder
                .edge(
                    StateEdgeBuilder::default()
                        .source(src.clone())
                        .unwrap()
                        .destination(dst.clone())
                        .unwrap()
                        .label(&label)
                        .unwrap(),
                )
                .unwrap();
        }
    }

    builder
}

pub fn generate_mermaid_hierarchical<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    include_guards: bool,
    function_mentions: &HashMap<String, HashSet<String>>,
    resolve_type: F,
) -> String
where
    F: Fn(&str) -> String + Copy,
{
    let builder = populate_builder_hierarchical(
        fsm,
        all_fsms,
        context_struct_map,
        include_guards,
        function_mentions,
        resolve_type,
    );
    StateDiagram::from(builder)
        .to_string()
        .replace("\r\n", "\n")
        .replace("    direction LR\n", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_tokens() {
        let input = "ctx . timer . is_expired ( )".to_string();
        assert_eq!(clean_tokens(input), "ctx.timer.is_expired()");

        let input = "ButtonEvent :: Press".to_string();
        assert_eq!(clean_tokens(input), "ButtonEvent::Press");

        let input = "if ! ctx . is_active ( )".to_string();
        assert_eq!(clean_tokens(input), "if !ctx.is_active()");

        let input = "ctx.tick_count>=3".to_string();
        assert_eq!(clean_tokens(input), "ctx.tick_count >= 3");
        
        let input = "a && b || c".to_string();
        assert_eq!(clean_tokens(input), "a and b or c");

        let input = "HeaderScheduleEvent :: Apply { resweep , force_occupancy_broadcast , occupancy_changed , }".to_string();
        assert_eq!(clean_tokens(input), "HeaderScheduleEvent::Apply<br/>{ resweep, force_occupancy_broadcast, occupancy_changed }");

        let input = "Event :: A | Event :: B".to_string();
        assert_eq!(clean_tokens(input), "Event::A | Event::B");

        let input = "Apply{a,b,}".to_string();
        assert_eq!(clean_tokens(input), "Apply<br/>{ a, b }");
    }
}
