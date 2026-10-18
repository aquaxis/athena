//! The Phase-7 pipeline's conformance (P7-01): the plan and the pipeline shell agree —
//! both name the wasm target, the build command, and the produced artifacts.

#[test]
fn the_build_plan_and_the_script_agree() {
    let plan = athena_studio::BuildPlan::phase7();
    let script = std::fs::read_to_string("../scripts/build-studio.sh").expect("read");
    for command in plan.commands {
        // The script carries the command's face (the target build + the toolchain add).
        match command {
            "rustup target add wasm32-unknown-unknown" => {
                assert!(
                    script.contains("rustup target add"),
                    "the toolchain add rides: {command}"
                );
            }
            "cargo build -p athena-studio --target wasm32-unknown-unknown --release" => {
                assert!(
                    script.contains("cargo build -p athena-studio --target \"$target\""),
                    "{command}"
                );
                assert!(script.contains("wasm32-unknown-unknown"));
            }
            other => panic!("unplanned command face: {other}"),
        }
    }
    // The artifacts: the wasm module at the target's release area.
    for artifact in plan.artifacts {
        let text = artifact.display().to_string();
        assert!(
            text.contains("athena_studio.wasm") || text.contains("athena_studio.js"),
            "{text}"
        );
    }
    // The lib's wasm path helper agrees.
    assert_eq!(
        athena_studio::wasm::module_path(std::path::Path::new("/root")),
        std::path::PathBuf::from("/root/target/wasm32-unknown-unknown/release/athena_studio.wasm")
    );
}
