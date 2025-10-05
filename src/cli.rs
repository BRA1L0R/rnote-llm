use core::fmt;
use std::{borrow::Cow, path::PathBuf};

use clap::{Subcommand, ValueEnum};

#[derive(Subcommand)]
pub enum Command {
    Single {
        /// Input file
        file: PathBuf,
        /// Optional output markdown file
        #[arg(required = false)]
        output_file: Option<PathBuf>,
    },
    Batch {
        recursive: bool,
        source_folder: PathBuf,
        destination_folder: PathBuf,
    },
}

#[derive(ValueEnum, Clone, Copy)]
pub enum Model {
    Gemini25Flash,
    Gemini25Pro,
}

impl Model {
    pub fn to_gemini_model(self) -> gemini_rust::Model {
        match self {
            Model::Gemini25Flash => gemini_rust::Model::Gemini25Flash,
            Model::Gemini25Pro => gemini_rust::Model::Gemini25Pro,
        }
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

#[derive(ValueEnum, Clone, Copy)]
pub enum Prompt {
    Default,
    Summarize,
    Test,
}

impl Options {
    pub fn prompt(&self) -> std::io::Result<Cow<'static, str>> {
        if let Some(custom) = &self.custom_prompt {
            return std::fs::read_to_string(custom).map(Into::into);
        }

        const DEFAULT_PROMPT: &str = include_str!("./PROMPT.txt");
        const SUMMARIZE_PROMPT: &str = include_str!("./SUMMARIZE.txt");
        const TEST_PROMPT: &str = include_str!("./TEST.txt");

        let prompt = match self.prompt {
            Prompt::Default => DEFAULT_PROMPT,
            Prompt::Summarize => SUMMARIZE_PROMPT,
            Prompt::Test => TEST_PROMPT,
        };

        Ok(prompt.into())
    }
}

impl fmt::Display for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

#[derive(clap::Parser)]
pub struct Options {
    #[arg(short, long, required = true)]
    pub key: String,

    #[arg(short, long, default_value_t = Model::Gemini25Flash)]
    pub model: Model,

    #[arg(short, long, default_value_t = Prompt::Default)]
    pub prompt: Prompt,

    #[arg(short, default_value_t = false)]
    pub skip_existing: bool,

    #[arg(short, long, required = false)]
    pub custom_prompt: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}
