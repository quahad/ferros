#[cfg(not(workaround_build))]
fn main() {
    cargo_5730::run_build_script();
}


#[cfg(workaround_build)]
fn main() {
    use ferros_build::*;
    use std::path::Path;
    use std::env;

    println!("cargo:rerun-if-env-changed=TEST_CASE");

    let test_case = match env::var("TEST_CASE") {
        Ok(val) => val,
        Err(_) => "root_task_runs".to_string(),
    };

    println!("cargo:rustc-cfg=test_case=\"{}\"", test_case);

    let out_dir = Path::new(&std::env::var_os("OUT_DIR").unwrap()).to_owned();
    let bin_dir = out_dir.join("..").join("..").join("..");
    let resources = out_dir.join("resources.rs");

    let elf_proc = ElfResource {
        path: bin_dir.join("elf-process"),
        image_name: "elf-process".to_owned(),
        type_name: "ElfProcess".to_owned(),
        stack_size_bits: None,
    };

    embed_resources(&resources, vec![&elf_proc as &dyn Resource]);
}
