//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), codegraph-kernel/build.rs.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(any(feature = "kotlin", feature = "dart"))]
    for (enabled, name) in [
        (cfg!(feature = "kotlin"), "kotlin"),
        (cfg!(feature = "dart"), "dart"),
    ] {
        if enabled {
            let directory = format!("vendor/{name}");
            println!("cargo:rerun-if-changed={directory}");
            cc::Build::new()
                .include(&directory)
                .file(format!("{directory}/parser.c"))
                .file(format!("{directory}/scanner.c"))
                .flag_if_supported("-Wno-unused-parameter")
                .flag_if_supported("-Wno-unused-but-set-variable")
                .flag_if_supported("-Wno-trigraphs")
                .flag_if_supported("-Wno-unused-function")
                .flag_if_supported("-Wno-unused-value")
                .flag_if_supported("-utf-8")
                .compile(&format!("tree-sitter-{name}"));
        }
    }
}
