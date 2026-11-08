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
    // The artifacts: the module is named after the binary (`athena-studio`, hyphenated
    // — the `wasm::MODULE_FILE` face); the glue script rides its §25 name
    // (`athena_studio.js`, underscored). The served URL face (`/athena_studio.wasm`)
    // is a separate surface from the artifact path.
    let artifacts: Vec<String> = plan
        .artifacts
        .iter()
        .map(|artifact| artifact.display().to_string())
        .collect();
    assert!(
        artifacts
            .iter()
            .any(|text| text.ends_with("/athena-studio.wasm")),
        "the wasm artifact must ride the toolchain's output name: {artifacts:?}"
    );
    assert!(
        artifacts
            .iter()
            .any(|text| text.ends_with("/athena_studio.js")),
        "the glue artifact must ride its §25 name: {artifacts:?}"
    );
    for text in &artifacts {
        let file = text.rsplit('/').next().expect("the file name");
        assert!(
            file == athena_studio::wasm::MODULE_FILE || file == "athena_studio.js",
            "an artifact outside the pipeline's two faces: {text}"
        );
    }
    // The lib's wasm path helper agrees.
    assert_eq!(
        athena_studio::wasm::module_path(std::path::Path::new("/root")),
        std::path::PathBuf::from("/root/target/wasm32-unknown-unknown/release/athena-studio.wasm")
    );
}
