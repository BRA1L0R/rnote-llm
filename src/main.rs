mod cli;

use std::{
    fs::ReadDir,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use base64::{Engine as _, prelude::BASE64_STANDARD};
use clap::Parser as _;
use futures::{StreamExt, TryStreamExt};
use gemini_rust::Gemini;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::LevelFilter;
use rnote_engine::{
    Engine,
    engine::{
        EngineSnapshot,
        export::{SelectionExportFormat, SelectionExportPrefs},
    },
};
use smol::Executor;

use crate::cli::{Command, Options};

async fn export_rnote_file(
    engine: &mut Engine,
    input_file: impl AsRef<Path>,
) -> anyhow::Result<Vec<u8>> {
    static EXECUTOR: Executor = Executor::new();

    let task = async move {
        let read = std::fs::read(&input_file).context("opening rnote file for byte read")?;

        let snapshot = EngineSnapshot::load_from_rnote_bytes(read)
            .await
            .context("loading file into snapshot context")?;

        let _ = engine.load_snapshot(snapshot);
        let _ = engine.select_all_strokes();

        let export_prefs = SelectionExportPrefs {
            with_background: true,
            with_pattern: false,
            optimize_printing: false,
            export_format: SelectionExportFormat::Png,
            ..Default::default()
        };

        let export = engine
            .export_selection(Some(export_prefs))
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        Ok(export)
    };

    EXECUTOR.run(task).await
}

async fn convert_note(
    client: &Gemini,
    system_prompt: impl Into<String>,
    note_png: &[u8],
) -> anyhow::Result<String> {
    let encoded = BASE64_STANDARD.encode(note_png);

    let output = client
        .generate_content()
        .with_dynamic_thinking()
        .with_system_instruction(system_prompt)
        .with_inline_data(encoded, "image/png")
        .execute()
        .await?;

    Ok(output.text())
}

async fn execute_job(
    gemini_client: &Gemini,
    system_prompt: impl Into<String>,
    skip_existing: bool,
    job: Job,
) -> anyhow::Result<()> {
    let build_message = |stage: &str| {
        format!(
            "({} -> {}) {}",
            job.input_file.to_str().unwrap_or_default(),
            job.output_file.to_str().unwrap_or_default(),
            stage
        )
    };

    job.progress_bar
        .set_message(job.input_file.to_string_lossy().into_owned());
    job.progress_bar
        .enable_steady_tick(Duration::from_millis(100));
    job.progress_bar
        .set_style(ProgressStyle::with_template("[{elapsed_precise}] {spinner} {msg}").unwrap());

    if skip_existing && tokio::fs::try_exists(&job.output_file).await? {
        job.progress_bar
            .finish_with_message(build_message("Skipping existing..."));

        return Ok(());
    }

    /*
     * Export RNote
     */
    job.progress_bar
        .set_message(build_message("Exporting RNote file..."));

    let mut engine = Engine::default();
    let note_png = export_rnote_file(&mut engine, &job.input_file).await?;

    /*
     * Convert to Markdown
     */
    job.progress_bar
        .set_message(build_message("Converting to Markdown..."));

    let converted = convert_note(gemini_client, system_prompt, &note_png).await?;
    tokio::fs::create_dir_all(job.output_file.parent().unwrap()).await?;
    tokio::fs::write(&job.output_file, converted).await?;

    job.progress_bar.finish_with_message(build_message("Done!"));
    Ok(())
}

/// Recursively search directories for files
struct DirWalker {
    /// 0 -> top level directory
    ///
    /// 1 -> top level -> subdirectory
    ///
    /// 3 -> top level -> sub -> sub-sub
    max_depth: usize,
    path_stack: Vec<ReadDir>,
}

impl DirWalker {
    fn new(path: &Path, max_depth: usize) -> std::io::Result<Self> {
        let readdir = std::fs::read_dir(path)?;
        let path_stack = vec![readdir];

        Ok(Self {
            path_stack,
            max_depth,
        })
    }
}

impl Iterator for DirWalker {
    type Item = PathBuf;

    fn next(&mut self) -> Option<Self::Item> {
        let explore = self.path_stack.last_mut()?;
        let next = explore.next();

        match next {
            Some(Ok(file)) if file.file_type().unwrap().is_file() => return Some(file.path()),
            Some(Ok(file))
                if file.file_type().unwrap().is_dir()
                    && self.path_stack.len() <= self.max_depth =>
            {
                let readdir = std::fs::read_dir(file.path()).unwrap();
                self.path_stack.push(readdir);
            }
            _ => {
                self.path_stack.pop();
            }
        }

        self.next()
    }
}

struct Job {
    progress_bar: ProgressBar,
    input_file: PathBuf,
    output_file: PathBuf,
}

impl Job {
    fn new(
        progress_bar: ProgressBar,
        input_file: impl Into<PathBuf>,
        output_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            progress_bar,
            input_file: input_file.into(),
            output_file: output_file.into(),
        }
    }

    fn from_folder(
        input_folder: &Path,
        output_folder: &Path,
        max_depth: usize,
    ) -> anyhow::Result<Vec<Job>> {
        // let readdir = std::fs::read_dir(input_folder)?;
        std::fs::create_dir(output_folder)?;
        let input_folder = input_folder.canonicalize()?;
        let output_folder = output_folder.canonicalize()?;

        let readdir = DirWalker::new(&input_folder, max_depth)?;

        let mut jobs = vec![];
        let multi = MultiProgress::new();
        let start_components = input_folder.components().count();

        for file in readdir {
            // generate relative path in respect to input_folder
            let relative_file: PathBuf = file.components().skip(start_components).collect();
            let mut output_file = output_folder.join(relative_file);

            output_file.set_extension("md");

            // create progress bar
            let pb = multi.add(ProgressBar::new_spinner());
            let job = Job::new(pb, file, output_file);

            jobs.push(job);
        }

        Ok(jobs)
    }
}

async fn run() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(LevelFilter::Info)
        .parse_default_env()
        .init();

    let cmdline = Options::parse();
    let model = cmdline.model.to_gemini_model();
    let prompt = cmdline.prompt()?;

    let gemini = Gemini::with_model(cmdline.key, model)?;

    let jobs = match cmdline.command {
        Command::Batch {
            source_folder,
            destination_folder,
        } => Job::from_folder(&source_folder, &destination_folder)?,
        Command::Single { file, output_file } => {
            let output_file = output_file.unwrap_or_else(|| file.with_extension("md"));
            vec![Job::new(ProgressBar::new_spinner(), &file, &output_file)]
        }
    };

    futures::stream::iter(jobs)
        .map(|job| execute_job(&gemini, prompt.clone(), cmdline.skip_existing, job))
        .buffer_unordered(10)
        .try_collect::<Vec<()>>()
        .await?;

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let err = run().await;
    if let Err(err) = err {
        log::error!("{err:?}");
    }
}
