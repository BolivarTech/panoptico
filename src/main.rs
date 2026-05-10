// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! AI Code Reviewer CLI entry point.
//!
//! Parses command-line arguments, optionally loads a TOML config file,
//! and delegates execution to [`PrAiReviewer`].

use std::process;

use clap::{Parser, Subcommand};

use panoptico::config::{
    parse_backend, parse_parallel, ConfigFile, ReviewConfig, DEFAULT_CONFIG_FILENAME,
};
use panoptico::{Command, PrAiReviewer};

/// AI-powered code review CLI for Pull Requests using Claude.
#[derive(Parser)]
#[command(name = "Panoptico", version, about)]
struct Cli {
    /// Path to TOML configuration file.
    #[arg(long, short, default_value = DEFAULT_CONFIG_FILENAME)]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

/// Available CLI subcommands.
///
/// `Review` is intentionally large — clap derive does not support `Box<T>`.
/// The enum is constructed once at startup and immediately destructured.
#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Test API connection and print diagnostics.
    Test {
        /// Azure AI Foundry endpoint URL.
        #[arg(long, env = "AZURE_AI_ENDPOINT")]
        endpoint: Option<String>,

        /// Model deployment name.
        #[arg(long)]
        model: Option<String>,
    },

    /// Review PR changes against base branch.
    Review {
        /// Git reference to diff against.
        #[arg(long)]
        base_ref: Option<String>,

        /// Git reference to diff towards (default: HEAD).
        #[arg(long)]
        target_ref: Option<String>,

        /// Backend: "azure", "anthropic", "aws-bedrock", or "claude-code".
        #[arg(long)]
        backend: Option<String>,

        /// Model deployment name.
        #[arg(long)]
        model: Option<String>,

        /// Fallback model for rate-limit retries.
        #[arg(long)]
        fallback_model: Option<String>,

        /// API endpoint URL.
        #[arg(long, env = "AZURE_AI_ENDPOINT")]
        endpoint: Option<String>,

        /// Output raw JSON instead of human-readable text.
        #[arg(long)]
        json: bool,

        /// Write output to a file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<String>,

        /// File extension patterns (comma-separated).
        #[arg(long, value_delimiter = ',')]
        extensions: Vec<String>,

        /// Maximum lines per review batch.
        #[arg(long)]
        max_lines: Option<usize>,

        /// Path to a text file with a custom system prompt.
        #[arg(long)]
        system_prompt: Option<String>,

        /// Path to custom review instructions file.
        #[arg(long)]
        instructions: Option<String>,

        /// Batch parallelization: "none", "hybrid", or "full".
        #[arg(long)]
        parallel: Option<String>,

        /// Maximum concurrent API calls in parallel/hybrid modes.
        #[arg(long)]
        max_concurrent: Option<usize>,

        /// Enable prompt caching.
        #[arg(long, conflicts_with = "no_cache")]
        cache: bool,

        /// Disable prompt caching.
        #[arg(long, conflicts_with = "cache")]
        no_cache: bool,

        /// Enable cost report after review.
        #[arg(long, conflicts_with = "no_cost_report")]
        cost_report: bool,

        /// Disable cost report after review.
        #[arg(long, conflicts_with = "cost_report")]
        no_cost_report: bool,

        /// Password to decrypt encrypted API key.
        #[arg(long)]
        key_password: Option<String>,

        /// Read decryption password from stdin.
        #[arg(long, conflicts_with = "key_password")]
        key_password_stdin: bool,

        /// Enable semantic context extraction (sends complete functions to LLM).
        #[arg(long, conflicts_with = "no_semantic")]
        semantic: bool,

        /// Disable semantic extraction (use raw diff fragments).
        #[arg(long, conflicts_with = "semantic")]
        no_semantic: bool,
    },

    /// Configuration management commands.
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },
}

/// Configuration subcommands.
#[derive(Subcommand)]
enum ConfigCommands {
    /// Generate a default panoptico.toml configuration file.
    Init,

    /// Generate a default ai-prompt.txt system prompt template.
    InitPrompt,

    /// Encrypt an API key and print the base64 blob for TOML storage.
    EncryptKey {
        /// Password for key derivation.
        #[arg(long)]
        password: String,

        /// Plaintext API key to encrypt.
        #[arg(long)]
        api_key: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Load TOML config if the file exists; start from defaults otherwise.
    let config_path = std::path::Path::new(&cli.config);
    let mut config = if config_path.exists() {
        match ConfigFile::from_file(config_path) {
            Ok(file) => match file.into_review_config() {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("Warning: failed to load {}: {}", cli.config, e);
                ReviewConfig::default()
            }
        }
    } else {
        eprintln!(
            "Warning: '{}' not found, using built-in defaults",
            cli.config
        );
        ReviewConfig::default()
    };

    // Apply CLI overrides on top of TOML/defaults.
    let command = match cli.command {
        Commands::Test { endpoint, model } => {
            if let Some(ep) = endpoint {
                config.endpoint = Some(ep);
            }
            if let Some(m) = model {
                config.model = m;
            }
            Command::Test
        }
        Commands::Review {
            base_ref,
            target_ref,
            backend,
            model,
            fallback_model,
            endpoint,
            json,
            output,
            extensions,
            max_lines,
            system_prompt,
            instructions,
            parallel,
            max_concurrent,
            cache,
            no_cache,
            cost_report,
            no_cost_report,
            key_password,
            key_password_stdin,
            semantic,
            no_semantic,
        } => {
            if let Some(br) = base_ref {
                config.base_ref = br;
            }
            if let Some(tr) = target_ref {
                config.target_ref = tr;
            }
            if let Some(b) = backend {
                match parse_backend(&b) {
                    Ok(bt) => config.backend = bt,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        process::exit(1);
                    }
                }
            }
            if let Some(m) = model {
                config.model = m;
            }
            if let Some(fm) = fallback_model {
                config.fallback_model = Some(fm);
            }
            if let Some(ep) = endpoint {
                config.endpoint = Some(ep);
            }
            if json {
                config.json_output = true;
            }
            if let Some(path) = output {
                config.output_path = Some(path);
            }
            if !extensions.is_empty() {
                config.extensions = extensions;
            }
            if let Some(ml) = max_lines {
                config.max_lines_per_batch = ml;
            }
            if let Some(path) = system_prompt {
                config.system_prompt_path = Some(path);
            }
            if let Some(path) = instructions {
                config.instructions_path = Some(path);
            }
            if let Some(p) = parallel {
                match parse_parallel(&p) {
                    Ok(pm) => config.parallel = pm,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        process::exit(1);
                    }
                }
            }
            if let Some(mc) = max_concurrent {
                config.max_concurrent = mc;
            }
            if cache {
                config.cache_enabled = true;
            }
            if no_cache {
                config.cache_enabled = false;
            }
            if cost_report {
                config.cost_report = true;
            }
            if no_cost_report {
                config.cost_report = false;
            }
            if let Some(kp) = key_password {
                config.key_password = Some(kp);
            } else if key_password_stdin {
                let mut pwd = String::new();
                std::io::stdin()
                    .read_line(&mut pwd)
                    .expect("Failed to read password from stdin");
                config.key_password = Some(pwd.trim_end().to_string());
            }
            if semantic {
                config.semantic = true;
            }
            if no_semantic {
                config.semantic = false;
            }
            Command::Review
        }
        Commands::Config { action } => match action {
            ConfigCommands::Init => Command::GenerateConfig,
            ConfigCommands::InitPrompt => Command::GeneratePrompt,
            ConfigCommands::EncryptKey { password, api_key } => {
                let path = if config_path.exists() {
                    Some(cli.config.clone())
                } else {
                    None
                };
                Command::EncryptKey {
                    password,
                    api_key,
                    config_path: path,
                }
            }
        },
    };

    let reviewer = PrAiReviewer::new(config);
    if let Err(e) = reviewer.run(command).await {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
