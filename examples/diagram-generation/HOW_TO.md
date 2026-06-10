# Mermaid Diagram Generation

`typed-fsm` supports automatic generation of [Mermaid.js](https://mermaid.js.org/) state diagrams. 
---

## CLI Tool (`fsm-gen`)
This is the recommended approach for generating documentation and CI artifacts. It scans your entire source tree and produces diagrams in a single pass, correctly resolving hierarchies even across different files.

### Usage
Run the tool from your project root:
```bash
cargo run -p fsm-gen
```

### Configuration (`.typed-fsm.toml`)
You can control the CLI's behavior globally by placing a `.typed-fsm.toml` file in your project root.

```toml
[mermaid]
scan_dir = "src"       # Where to scan for FSMs (directory or single file)
output_dir = "docs/diagrams"
mode = "Hierarchical" # Options: "Simple" (single level) | "Hierarchical" (nested states)
breakdown = "Both"    # Options: "None" | "Flat" | "Nested" | "Both"
include_guards = true # Whether to show guards (default: true)
```

### Overrides
You can override the `.typed-fsm.toml` options using command line args.

For example, to hide guards:
```bash
cargo run -p fsm-gen -- --include-guards=false
```

To override the directory to scan to `examples/`:
```bash
cargo run -p fsm-gen -- --scan examples/
```

> **Note**: You do NOT need to add any features or extra dependencies to your project to use `fsm-gen`. It uses static analysis to generate diagrams without compiling your code.
