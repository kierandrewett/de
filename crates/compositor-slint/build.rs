// build.rs — Compile .slint UI files via slint-build.
// The top-level Compositor.slint is compiled here; sub-components are imported
// transitively by Compositor.slint itself, so only one entry point is needed.

fn main() {
    // Compile Compositor.slint (imports all sub-components transitively).
    let config = slint_build::CompilerConfiguration::new().with_style("native".to_string());

    slint_build::compile_with_config("slint/Compositor.slint", config)
        .expect("Failed to compile slint/Compositor.slint");
}
