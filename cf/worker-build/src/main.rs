use std::{
    convert::TryInto,
    env::{self, VarError},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Debug, Clone, Parser)]
struct Args {
    /// The path to the workers project to build
    cargo_dir: PathBuf,
    /// The output directory
    #[arg(long, default_value = "build")]
    out_dir: PathBuf,
}

/// Paths to all the locations we will operate on.
#[derive(Debug)]
struct Dirs {
    cargo_dir: PathBuf,
    out_dir: PathBuf,
    worker_dir: PathBuf,
}

impl Dirs {
    fn from_args(args: &Args) -> Result<Self> {
        let cargo_dir = args
            .cargo_dir
            .canonicalize()
            .context(format!("failed to locate cargo_dir {:?}", args.cargo_dir))?;

        fs::create_dir_all(&args.out_dir).context(format!("failed to create out_dir {:?}", args.out_dir))?;
        let out_dir = args.out_dir
            .canonicalize()
            .context("failed to canonicalize out_dir")?;
        let mut worker_dir = out_dir.to_owned();
        worker_dir.push("worker");

        Ok(Self {
            cargo_dir,
            out_dir,
            worker_dir,
        })
    }

    pub fn worker_path(&self, name: impl AsRef<str>) -> PathBuf {
        self.worker_dir.to_owned().join(name.as_ref())
    }

    pub fn output_path(&self, name: impl AsRef<str>) -> PathBuf {
        self.out_dir.to_owned().join(name.as_ref())
    }
}

const OUT_NAME: &str = "index";

const WASM_IMPORT: &str = r#"let wasm;
export function __wbg_set_wasm(val) {
    wasm = val;
}

"#;

const WASM_IMPORT_REPLACEMENT: &str = r#"
import wasm from './glue.js';

export function getMemory() {
    return wasm.memory;
}
"#;

pub fn main() -> Result<()> {
    let args = Args::parse();

    Builder::new(args)?.build()?;

    Ok(())
}

#[derive(Debug)]
struct Builder {
    #[allow(dead_code)]
    args: Args,
    dirs: Dirs,
}

impl Builder {
    fn new(args: Args) -> Result<Self> {
        let dirs = Dirs::from_args(&args)?;

        Ok(Self { args, dirs })
    }

    fn build(&self) -> Result<()> {
        // Our tests build the bundle ourselves.
        if !cfg!(test) {
            self.wasm_pack_build().context("failed wasm_pack build")?;
        }

        let with_coredump = env::var("COREDUMP").is_ok();
        if with_coredump {
            println!("Adding wasm coredump");
            self.wasm_coredump()?;
        }

        self.create_worker_dir()
            .context("failed to create worker dir")?;
        self.copy_generated_code_to_worker_dir()
            .context("failed to copy generated code")?;
        self.use_glue_import()
            .context("failed to use glue import")?;

        write_string_to_file(
            self.dirs.worker_path("glue.js"),
            include_str!("./js/glue.js"),
        )?;
        write_string_to_file(
            self.dirs.worker_path("shim.js"),
            include_str!("./js/shim.js"),
        )?;

        self.bundle()?;
        self.remove_unused_js()?;

        Ok(())
    }

    fn wasm_coredump(&self) -> Result<()> {
        let coredump_flags = env::var("COREDUMP_FLAGS");
        let coredump_flags: Vec<&str> = if let Ok(flags) = &coredump_flags {
            flags.split(' ').collect()
        } else {
            vec![]
        };

        let mut child = Command::new("wasm-coredump-rewriter")
            .args(coredump_flags)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|err| {
                anyhow::anyhow!("failed to spawn wasm-coredump-rewriter: {err}\n\nIn case you are missing the binary, you can install it using: `cargo install wasm-coredump-rewriter`.")
            })?;

        let input_filename = self.dirs.output_path("index_bg.wasm");

        let input_bytes = {
            let mut input = File::open(input_filename.clone())
                .map_err(|err| anyhow::anyhow!("failed to open input file: {err}"))?;

            let mut input_bytes = Vec::new();
            input
                .read_to_end(&mut input_bytes)
                .map_err(|err| anyhow::anyhow!("failed to open input file: {err}"))?;

            input_bytes
        };

        {
            let child_stdin = child.stdin.as_mut().unwrap();
            child_stdin
                .write_all(&input_bytes)
                .map_err(|err| anyhow::anyhow!("failed to write input file to rewriter: {err}"))?;
            // Close stdin to finish and avoid indefinite blocking
        }

        let output = child
            .wait_with_output()
            .map_err(|err| anyhow::anyhow!("failed to get rewriter's status: {err}"))?;

        if output.status.success() {
            // Open the input file again with truncate to write the output
            let mut f = fs::OpenOptions::new()
                .truncate(true)
                .write(true)
                .open(input_filename)
                .map_err(|err| anyhow::anyhow!("failed to open output file: {err}"))?;
            f.write_all(&output.stdout)
                .map_err(|err| anyhow::anyhow!("failed to write output file: {err}"))?;

            Ok(())
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow::anyhow!(format!(
                "failed to run Wasm coredump rewriter: {stdout}\n{stderr}"
            )))
        }
    }

    fn wasm_pack_build(&self) -> Result<()> {
        let mut command = Command::new("wasm-pack");
        command
            .arg("build")
            .arg("--no-typescript")
            .args(["--target", "bundler"])
            .arg("--out-dir")
            .arg(&self.dirs.out_dir)
            .args(["--out-name", OUT_NAME])
            .arg(&self.dirs.cargo_dir);
        eprintln!("{command:?}");
        let exit_status = command.spawn()?.wait()?;

        match exit_status.success() {
            true => Ok(()),
            false => anyhow::bail!("wasm-pack exited with status {}", exit_status),
        }
    }

    fn create_worker_dir(&self) -> Result<()> {
        let worker_dir = &self.dirs.worker_dir;

        // remove anything that already exists
        if worker_dir.is_dir() {
            fs::remove_dir_all(worker_dir).context(format!("failed to remove {worker_dir:?}"))?
        } else if worker_dir.is_file() {
            fs::remove_file(worker_dir).context(format!("failed to remove {worker_dir:?}"))?
        };

        // create the worker dir
        fs::create_dir(worker_dir).context(format!("failed to create dir {worker_dir:?}"))?;

        Ok(())
    }

    fn copy_generated_code_to_worker_dir(&self) -> Result<()> {
        let glue_src = self.dirs.output_path(format!("{OUT_NAME}_bg.js"));
        let glue_dest = self.dirs.worker_path(format!("{OUT_NAME}_bg.js"));

        let wasm_src = self.dirs.output_path(format!("{OUT_NAME}_bg.wasm"));
        let wasm_dest = self.dirs.worker_path(format!("{OUT_NAME}.wasm"));

        // wasm-bindgen supports adding arbitrary JavaScript for a library, so we need to move that as well.
        // https://rustwasm.github.io/wasm-bindgen/reference/js-snippets.html
        let snippets_src = self.dirs.output_path("snippets");
        let snippets_dest = self.dirs.worker_path("snippets");

        for (src, dest) in [
            (glue_src, glue_dest),
            (wasm_src, wasm_dest),
            (snippets_src, snippets_dest),
        ] {
            if !src.exists() {
                continue;
            }

            fs::rename(src, dest)?;
        }

        Ok(())
    }

    // Replaces the wasm import with an import that instantiates the WASM modules itself.
    fn use_glue_import(&self) -> Result<()> {
        let bindgen_glue_path = self.dirs.worker_path(format!("{OUT_NAME}_bg.js"));
        let old_bindgen_glue = read_file_to_string(&bindgen_glue_path)?;
        let fixed_bindgen_glue = old_bindgen_glue.replace(WASM_IMPORT, WASM_IMPORT_REPLACEMENT);
        write_string_to_file(bindgen_glue_path, fixed_bindgen_glue)?;
        Ok(())
    }

    // Bundles the snippets and worker-related code into a single file.
    fn bundle(&self) -> Result<()> {
        let no_minify = !matches!(env::var("NO_MINIFY"), Err(VarError::NotPresent));

        let mut esbuild_command = Command::new("npx");
        esbuild_command.arg("esbuild");

        esbuild_command.args([
            "--external:./index.wasm",
            "--external:cloudflare:sockets",
            "--format=esm",
            "--bundle",
            "./shim.js",
            "--outfile=shim.mjs",
        ]);

        if !no_minify {
            esbuild_command.arg("--minify");
        }

        eprintln!("{esbuild_command:?}");

        let exit_status = esbuild_command
            .current_dir(&self.dirs.worker_dir)
            .spawn()
            .context("esbuild1")?
            .wait()
            .context("esbuild2")?;

        match exit_status.success() {
            true => Ok(()),
            false => anyhow::bail!("esbuild exited with status {}", exit_status),
        }
    }

    // After bundling there's no reason why we'd want to upload our now un-used JavaScript so we'll
    // delete it.
    fn remove_unused_js(&self) -> Result<()> {
        let snippets_dir = self.dirs.worker_path("snippets");

        if snippets_dir.exists() {
            std::fs::remove_dir_all(&snippets_dir)?;
        }

        for to_remove in [
            format!("{OUT_NAME}_bg.js"),
            "shim.js".into(),
            "glue.js".into(),
        ] {
            std::fs::remove_file(self.dirs.worker_path(to_remove))?;
        }

        Ok(())
    }
}

fn read_file_to_string<P: AsRef<Path>>(path: P) -> Result<String> {
    let file_size = path.as_ref().metadata()?.len().try_into()?;
    let mut file = File::open(path)?;
    let mut buf = Vec::with_capacity(file_size);
    file.read_to_end(&mut buf)?;
    String::from_utf8(buf).map_err(anyhow::Error::from)
}

fn write_string_to_file<P: AsRef<Path>>(path: P, contents: impl AsRef<str>) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents.as_ref().as_bytes())?;

    Ok(())
}
