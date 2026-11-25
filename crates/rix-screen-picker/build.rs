fn main() {
    slint_build::compile_with_config(
        "ui/picker.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("Failed to compile Slint UI for rix-screen-picker");
}
