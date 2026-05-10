// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Core application class — orchestrates the full review pipeline.
//!
//! [`Panoptico`] is the main entry point of the library. It holds
//! a [`ReviewConfig`] and exposes [`run`](Panoptico::run) to
//! dispatch test or review commands.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::backend::anthropic::AnthropicBackend;
use crate::backend::azure_foundry::AzureFoundryBackend;
use crate::backend::bedrock::BedrockBackend;
use crate::backend::claude_code::ClaudeCodeBackend;
use crate::backend::{CodeReview, ReviewBackend, ReviewRequest};
use crate::batch::group_into_batches;
use crate::config::{
    BackendType, ConfigFile, CredentialSourceType, ParallelMode, ReviewConfig,
    DEFAULT_CONFIG_FILENAME,
};
use crate::context::{build_file_context, FileContext};
use crate::credential;
use crate::error::ReviewError;
use crate::finding_id::assign_finding_ids;
use crate::git::GitDiff;
use crate::hunk::parse_hunks;
use crate::languages::{
    fallback_extract, group_semantic_batches, LanguageExtractor, LanguageRegistry,
    DEFAULT_MAX_SEMANTIC_TOKENS,
};
use crate::metrics::ReviewMetrics;
use crate::prompt::{
    build_review_request, build_semantic_review_request, build_synthesis_request,
    format_context_table, DEFAULT_SYSTEM_PROMPT, SEMANTIC_SYSTEM_PROMPT,
};
use crate::validator::validate_findings;

/// Default output filename for the system prompt template.
const DEFAULT_PROMPT_FILENAME: &str = "ai-prompt.txt";

/// Maximum tokens for the test connection request.
const TEST_CONNECTION_MAX_TOKENS: u32 = 256;

/// Input price per million tokens (USD) for cost reporting.
const INPUT_PRICE_PER_MTOK: f64 = 3.0;

/// Output price per million tokens (USD) for cost reporting.
const OUTPUT_PRICE_PER_MTOK: f64 = 15.0;

/// Action to execute on the reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Test the API connection and print diagnostics.
    Test,
    /// Run a full code review on the current PR diff.
    Review,
    /// Encrypt an API key and store it in the TOML config.
    EncryptKey {
        /// Password for key derivation.
        password: String,
        /// Plaintext API key to encrypt.
        api_key: String,
        /// Path to the TOML config file to update (if it exists).
        config_path: Option<String>,
    },
    /// Generate a default TOML configuration file.
    GenerateConfig,
    /// Generate a default system prompt template file.
    GeneratePrompt,
}

/// AI-powered code reviewer for Pull Requests.
///
/// Orchestrates the complete review pipeline:
/// 1. Extract git diff between base ref and HEAD
/// 2. Parse diff into atomic hunks
/// 3. Group hunks into batches respecting line limits
/// 4. Send each batch to Claude for review (map phase)
/// 5. Validate batch findings (pre-synthesis hallucination filter)
/// 6. Synthesize batch reviews into a single report (reduce phase)
/// 7. Validate synthesized findings (post-synthesis safety net)
/// 8. Assign deterministic finding IDs
/// 9. Output or post results
///
/// # Examples
///
/// ```
/// use panoptico::{Panoptico, Command};
/// use panoptico::config::ReviewConfig;
///
/// let config = ReviewConfig::default();
/// let reviewer = Panoptico::new(config);
/// assert_eq!(reviewer.config().model, "claude-sonnet-4-5");
/// ```
pub struct Panoptico {
    /// Session configuration.
    config: ReviewConfig,
}

impl Panoptico {
    /// Create a new reviewer with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Review session configuration.
    pub fn new(config: ReviewConfig) -> Self {
        Self { config }
    }

    /// Return a reference to the current configuration.
    pub fn config(&self) -> &ReviewConfig {
        &self.config
    }

    /// Execute a command (test or review).
    ///
    /// Dispatches to [`test_connection`](Self::test_connection) or
    /// [`run_review`](Self::run_review) based on the command variant.
    ///
    /// # Arguments
    ///
    /// * `command` - The action to execute.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError`] if any pipeline step fails.
    pub async fn run(&self, command: Command) -> Result<(), ReviewError> {
        match command {
            Command::Test => self.test_connection().await,
            Command::Review => {
                let review = self.run_review().await?;
                self.output_review(&review)
            }
            Command::EncryptKey {
                password,
                api_key,
                config_path,
            } => self.encrypt_key(&password, &api_key, config_path.as_deref()),
            Command::GenerateConfig => self.generate_config(),
            Command::GeneratePrompt => self.generate_prompt(),
        }
    }

    /// Test the API connection and print diagnostics.
    ///
    /// Sends a minimal review request to verify the backend is
    /// reachable and the credentials are valid.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] if the connection fails.
    pub async fn test_connection(&self) -> Result<(), ReviewError> {
        println!("Testing API connection...");
        println!("  Backend:  {:?}", self.config.backend);
        println!("  Model:    {}", self.config.model);

        let backend = self.build_backend().await?;

        let request = ReviewRequest {
            system_prompt: "You are a test. Reply with a minimal JSON review.".to_string(),
            custom_instructions: None,
            diff_content: "+// test".to_string(),
            batch_number: 1,
            total_batches: 1,
            file_info: "test.txt".to_string(),
            model: self.config.model.clone(),
            max_tokens: TEST_CONNECTION_MAX_TOKENS,
        };

        let response = backend.review(&request).await?;
        println!("  Connection OK");
        println!(
            "  Tokens: {} in / {} out",
            response.usage.input_tokens, response.usage.output_tokens
        );
        Ok(())
    }

    /// Execute the full review pipeline.
    ///
    /// Runs all pipeline stages: diff extraction, hunk parsing,
    /// batching, map-reduce review, validation.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError`] on git, API, or parsing failures.
    pub async fn run_review(&self) -> Result<CodeReview, ReviewError> {
        // 1. Extract git diff
        let diff = GitDiff::extract(
            &self.config.base_ref,
            &self.config.target_ref,
            &self.config.extensions,
        )?;
        if diff.files.is_empty() {
            return Ok(CodeReview {
                summary: "No files changed.".to_string(),
                findings: vec![],
            });
        }

        // 2. Parse hunks
        let mut all_hunks = Vec::new();
        for (file, content) in &diff.files {
            all_hunks.extend(parse_hunks(file, content));
        }
        if all_hunks.is_empty() {
            return Ok(CodeReview {
                summary: "No reviewable hunks found.".to_string(),
                findings: vec![],
            });
        }

        // 2b. Build file context metadata
        let file_contexts: HashMap<String, FileContext> = diff
            .files
            .iter()
            .map(|(file, content)| (file.clone(), build_file_context(file, content)))
            .collect();

        // 3. Group into batches
        let batches = group_into_batches(all_hunks, self.config.max_lines_per_batch);
        let total_batches = u32::try_from(batches.len())
            .map_err(|_| ReviewError::Config("Too many batches".into()))?;

        // 4. Load custom system prompt from file (or use default)
        let system_prompt = self.resolve_system_prompt()?;

        // 5. Load custom instructions
        let custom_instructions = self
            .config
            .instructions_path
            .as_ref()
            .map(|path| {
                std::fs::read_to_string(path).map_err(|e| {
                    ReviewError::Config(format!("Failed to read instructions '{}': {}", path, e))
                })
            })
            .transpose()?;

        // 6. Create backend
        let backend: Arc<dyn ReviewBackend> = Arc::from(self.build_backend().await?);

        // 7. Build review requests — branch on semantic mode
        let requests: Vec<ReviewRequest> = if self.config.semantic {
            self.build_semantic_requests(
                &diff.files,
                &file_contexts,
                &system_prompt,
                custom_instructions.as_deref(),
                &batches,
                total_batches,
            )
        } else {
            build_diff_requests(
                &batches,
                total_batches,
                &self.config.model,
                &system_prompt,
                custom_instructions.as_deref(),
                &file_contexts,
            )
        };

        // 8. Map phase — dispatch with configured parallelism
        let mode = self.config.effective_parallel();
        let mut metrics = ReviewMetrics::new();
        let batch_reviews = self
            .dispatch_reviews(backend.clone(), requests, mode, &mut metrics)
            .await?;

        // 8b. Pre-synthesis validation — remove hallucinated findings before synthesis
        let valid_files: HashSet<String> = diff.files.keys().cloned().collect();
        let batch_reviews: Vec<CodeReview> = batch_reviews
            .into_iter()
            .map(|r| validate_findings(r, &valid_files))
            .collect();

        // 9. Reduce phase — synthesize if multiple batches
        let review = if batch_reviews.len() == 1 {
            batch_reviews
                .into_iter()
                .next()
                .ok_or_else(|| ReviewError::Api("No batch reviews produced".to_string()))?
        } else {
            let synthesis_request = build_synthesis_request(&batch_reviews, &self.config.model);
            let response = backend.review(&synthesis_request).await?;
            metrics.track(&response.usage);
            response.review
        };

        // 10. Post-synthesis validation — safety net for synthesis hallucinations
        let mut review = validate_findings(review, &valid_files);

        // 11. Assign deterministic finding IDs
        assign_finding_ids(&mut review);

        // 12. Print cost report if enabled
        if self.config.cost_report {
            let cost = metrics.calculate_cost(INPUT_PRICE_PER_MTOK, OUTPUT_PRICE_PER_MTOK);
            eprintln!(
                "Cost: {} batches | {} in / {} out tokens | ${:.4}",
                metrics.batch_count, metrics.total_input_tokens, metrics.total_output_tokens, cost
            );
        }

        Ok(review)
    }

    /// Output a review result based on configuration.
    ///
    /// Formats the review as JSON (`--json`) or human-readable text,
    /// then writes to a file (`--output`) or stdout.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Parse`] on serialization failure, or
    /// [`ReviewError::Io`] if the output file cannot be written.
    fn output_review(&self, review: &CodeReview) -> Result<(), ReviewError> {
        let content = if self.config.json_output {
            serde_json::to_string_pretty(review).map_err(|e| ReviewError::Parse(e.to_string()))?
        } else {
            format_human_readable(review)
        };

        if let Some(ref path) = self.config.output_path {
            std::fs::write(path, &content).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("Failed to write review to '{}': {}", path, e),
                )
            })?;
            println!("Review written to {}", path);
        } else {
            println!("{}", content);
        }
        Ok(())
    }

    /// Encrypt an API key and print the base64 blob.
    ///
    /// Uses [`credential::encrypt_api_key`](crate::credential::encrypt_api_key)
    /// to produce an encrypted blob suitable for the `api_key_encrypted`
    /// field in the `[azure]` TOML section.
    ///
    /// When `config_path` points to an existing TOML file, the
    /// `api_key_encrypted` field inside the `[azure]` section is
    /// updated in-place (preserving comments and formatting).
    ///
    /// # Arguments
    ///
    /// * `password` - Password for key derivation.
    /// * `api_key` - Plaintext API key to encrypt.
    /// * `config_path` - Optional path to the TOML config file to update.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] on encryption failure, or
    /// [`ReviewError::Io`] if the TOML file cannot be read or written.
    fn encrypt_key(
        &self,
        password: &str,
        api_key: &str,
        config_path: Option<&str>,
    ) -> Result<(), ReviewError> {
        let encrypted = credential::encrypt_api_key(password, api_key)?;
        println!("{}", encrypted);

        if let Some(path) = config_path {
            let path = Path::new(path);
            if path.exists() {
                self.update_toml_api_key(path, &encrypted)?;
            }
        }
        Ok(())
    }

    /// Update `api_key_encrypted` in the `[azure]` section of a TOML file.
    ///
    /// Reads the file, parses it with `toml_edit` to preserve comments
    /// and formatting, ensures the `[azure]` table exists, sets the
    /// `api_key_encrypted` field, and writes back.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the TOML configuration file.
    /// * `encrypted` - The encrypted blob to store.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Io`] on read/write failure, or
    /// [`ReviewError::Config`] if the file is not valid TOML.
    fn update_toml_api_key(&self, path: &Path, encrypted: &str) -> Result<(), ReviewError> {
        use toml_edit::DocumentMut;

        let content = std::fs::read_to_string(path)?;
        let mut doc: DocumentMut = content.parse().map_err(|e| {
            ReviewError::Config(format!("Failed to parse '{}': {}", path.display(), e))
        })?;

        // Ensure [azure] table exists.
        if !doc.contains_key("azure") {
            doc["azure"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["azure"]["api_key_encrypted"] = toml_edit::value(encrypted);

        std::fs::write(path, doc.to_string())?;
        eprintln!("Updated api_key_encrypted in {}", path.display());
        Ok(())
    }

    /// Generate a default TOML configuration file in the working directory.
    ///
    /// Writes [`ConfigFile::template()`] to [`DEFAULT_CONFIG_FILENAME`].
    /// Returns an error if the file already exists to prevent accidental
    /// overwriting.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] if the file already exists or
    /// cannot be written.
    fn generate_config(&self) -> Result<(), ReviewError> {
        let path = Path::new(DEFAULT_CONFIG_FILENAME);
        if path.exists() {
            return Err(ReviewError::Config(format!(
                "{} already exists; remove it first or use a different directory",
                DEFAULT_CONFIG_FILENAME
            )));
        }
        std::fs::write(path, ConfigFile::template())?;
        println!("Created {}", DEFAULT_CONFIG_FILENAME);
        Ok(())
    }

    /// Generate a default system prompt template file (`ai-prompt.txt`).
    ///
    /// Writes [`DEFAULT_SYSTEM_PROMPT`] to `ai-prompt.txt` in the
    /// working directory. Refuses to overwrite an existing file.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] if the file already exists, or
    /// [`ReviewError::Io`] if the file cannot be written.
    fn generate_prompt(&self) -> Result<(), ReviewError> {
        let path = Path::new(DEFAULT_PROMPT_FILENAME);
        if path.exists() {
            return Err(ReviewError::Config(format!(
                "{} already exists; remove it first or use a different directory",
                DEFAULT_PROMPT_FILENAME
            )));
        }
        std::fs::write(path, DEFAULT_SYSTEM_PROMPT)?;
        println!("Created {}", DEFAULT_PROMPT_FILENAME);
        Ok(())
    }

    /// Resolve the system prompt for the review session.
    ///
    /// If [`system_prompt_path`](ReviewConfig::system_prompt_path) is set,
    /// reads the file contents. Otherwise falls back to the built-in
    /// [`DEFAULT_SYSTEM_PROMPT`].
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] if the file path is set but
    /// the file cannot be read.
    fn resolve_system_prompt(&self) -> Result<String, ReviewError> {
        match &self.config.system_prompt_path {
            Some(path) => std::fs::read_to_string(path).map_err(|e| {
                ReviewError::Config(format!("Failed to read system prompt '{}': {}", path, e))
            }),
            None => Ok(self.config.system_prompt.clone()),
        }
    }

    /// Build a [`CredentialSource`](credential::CredentialSource) from config.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] when `Encrypted` or `Vault` source
    /// is selected but required fields are empty or missing.
    fn build_credential_source(&self) -> Result<credential::CredentialSource, ReviewError> {
        match self.config.credential_source {
            CredentialSourceType::Env => Ok(credential::CredentialSource::Env),
            CredentialSourceType::Keyring => Ok(credential::CredentialSource::Keyring),
            CredentialSourceType::Encrypted => {
                let blob = self.config.api_key_encrypted.as_deref().unwrap_or("");
                if blob.is_empty() {
                    return Err(ReviewError::Config(
                        "credential_source is 'encrypted' but api_key_encrypted is empty"
                            .to_string(),
                    ));
                }
                Ok(credential::CredentialSource::Encrypted {
                    api_key_encrypted: blob.to_string(),
                })
            }
            CredentialSourceType::Vault => {
                let url = self.config.vault_url.as_deref().unwrap_or("");
                let name = self.config.vault_secret_name.as_deref().unwrap_or("");
                if url.is_empty() {
                    return Err(ReviewError::Config(
                        "credential_source is 'vault' but vault_url is empty".to_string(),
                    ));
                }
                if name.is_empty() {
                    return Err(ReviewError::Config(
                        "credential_source is 'vault' but vault_secret_name is empty".to_string(),
                    ));
                }
                Ok(credential::CredentialSource::Vault {
                    vault_url: url.to_string(),
                    vault_secret_name: name.to_string(),
                })
            }
        }
    }

    /// Create the appropriate backend based on configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] if required settings are missing,
    /// or credential resolution fails.
    async fn build_backend(&self) -> Result<Box<dyn ReviewBackend>, ReviewError> {
        match self.config.backend {
            BackendType::ClaudeCode => Ok(Box::new(ClaudeCodeBackend::new())),
            BackendType::AwsBedrock => {
                let region = self.config.endpoint.as_deref().unwrap_or("us-east-1");
                Ok(Box::new(BedrockBackend::new(region)))
            }
            BackendType::AzureAiFoundry => {
                let endpoint = self.config.endpoint.as_deref().ok_or_else(|| {
                    ReviewError::Config("Azure AI Foundry endpoint not configured".to_string())
                })?;
                let source = self.build_credential_source()?;
                let password = self.config.key_password.as_deref();
                let api_key = source.resolve(password).await?;
                Ok(Box::new(AzureFoundryBackend::new(endpoint, &api_key)))
            }
            BackendType::Anthropic => {
                let source = self.build_credential_source()?;
                let password = self.config.key_password.as_deref();
                let api_key = source.resolve(password).await?;
                Ok(Box::new(AnthropicBackend::new(&api_key)))
            }
        }
    }

    /// Build semantic review requests using full file content extraction.
    ///
    /// Extracts complete code units (functions, structs, classes) from source
    /// files and groups them into token-limited batches. Falls back to
    /// diff-only if no semantic units are extracted.
    fn build_semantic_requests(
        &self,
        files: &HashMap<String, String>,
        file_contexts: &HashMap<String, FileContext>,
        system_prompt: &str,
        custom_instructions: Option<&str>,
        diff_batches: &[crate::batch::Batch],
        total_diff_batches: u32,
    ) -> Vec<ReviewRequest> {
        let registry = LanguageRegistry::new();
        let mut all_units = Vec::new();

        for (file, diff_content) in files {
            let ctx = build_file_context(file, diff_content);
            // Read full file from working tree (best-effort)
            let full_content = match std::fs::read_to_string(file) {
                Ok(content) => content,
                Err(e) => {
                    eprintln!(
                        "Warning: cannot read '{}' for semantic extraction: {}. \
                         Using diff content as fallback.",
                        file, e
                    );
                    let changed_lines = extract_changed_lines(diff_content);
                    all_units.extend(fallback_extract(diff_content, file, &changed_lines, &ctx));
                    continue;
                }
            };
            let changed_lines = extract_changed_lines(diff_content);

            if let Some(extractor) = registry.get(file) {
                let mut units = extractor.extract_units(&full_content, file, &changed_lines);
                for unit in &mut units {
                    unit.context = ctx.clone();
                }
                all_units.extend(units);
            } else {
                all_units.extend(fallback_extract(&full_content, file, &changed_lines, &ctx));
            }
        }

        if all_units.is_empty() {
            // No units extracted — fall back to diff-only for all files
            return build_diff_requests(
                diff_batches,
                total_diff_batches,
                &self.config.model,
                system_prompt,
                custom_instructions,
                file_contexts,
            );
        }

        let semantic_prompt = if system_prompt == DEFAULT_SYSTEM_PROMPT {
            SEMANTIC_SYSTEM_PROMPT.to_string()
        } else {
            system_prompt.to_string()
        };

        let semantic_batches = group_semantic_batches(all_units, DEFAULT_MAX_SEMANTIC_TOKENS);
        let total = u32::try_from(semantic_batches.len()).expect("batch count within u32");
        semantic_batches
            .iter()
            .enumerate()
            .map(|(i, batch)| {
                build_semantic_review_request(
                    batch,
                    u32::try_from(i + 1).expect("index"),
                    total,
                    &self.config.model,
                    &semantic_prompt,
                    custom_instructions,
                )
            })
            .collect()
    }

    /// Dispatch review requests using the configured parallelism mode.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] if any backend call fails.
    async fn dispatch_reviews(
        &self,
        backend: Arc<dyn ReviewBackend>,
        requests: Vec<ReviewRequest>,
        mode: ParallelMode,
        metrics: &mut ReviewMetrics,
    ) -> Result<Vec<CodeReview>, ReviewError> {
        match mode {
            ParallelMode::Sequential => {
                let mut reviews = Vec::with_capacity(requests.len());
                for request in &requests {
                    let response = backend.review(request).await?;
                    metrics.track(&response.usage);
                    reviews.push(response.review);
                }
                Ok(reviews)
            }
            ParallelMode::Hybrid => {
                let mut reviews = Vec::with_capacity(requests.len());
                // Batch 1 runs first to populate prompt cache.
                if let Some(first) = requests.first() {
                    let response = backend.review(first).await?;
                    metrics.track(&response.usage);
                    reviews.push(response.review);
                }
                // Remaining batches run in parallel with cache hits.
                if requests.len() > 1 {
                    let parallel_reviews =
                        self.run_parallel(backend, &requests[1..], metrics).await?;
                    reviews.extend(parallel_reviews);
                }
                Ok(reviews)
            }
            ParallelMode::Full => self.run_parallel(backend, &requests, metrics).await,
        }
    }

    /// Run requests in parallel with semaphore-bounded concurrency.
    ///
    /// Collects all results and returns successful reviews even when
    /// some batches fail. Only returns an error when ALL batches fail.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] if all batch calls fail or all
    /// spawned tasks panic.
    async fn run_parallel(
        &self,
        backend: Arc<dyn ReviewBackend>,
        requests: &[ReviewRequest],
        metrics: &mut ReviewMetrics,
    ) -> Result<Vec<CodeReview>, ReviewError> {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent));
        let mut join_set = JoinSet::new();

        for request in requests {
            let backend = backend.clone();
            let semaphore = semaphore.clone();
            let request = request.clone();
            join_set.spawn(async move {
                let _permit = semaphore
                    .acquire()
                    .await
                    .map_err(|e| ReviewError::Api(format!("Semaphore closed: {}", e)))?;
                backend.review(&request).await
            });
        }

        let mut reviews = Vec::with_capacity(requests.len());
        let mut errors = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(response)) => {
                    metrics.track(&response.usage);
                    reviews.push(response.review);
                }
                Ok(Err(e)) => {
                    eprintln!("Warning: batch review failed: {}", e);
                    errors.push(e);
                }
                Err(e) => {
                    eprintln!("Warning: batch task panicked: {}", e);
                    errors.push(ReviewError::Api(e.to_string()));
                }
            }
        }
        if reviews.is_empty() {
            return Err(errors
                .into_iter()
                .next()
                .unwrap_or_else(|| ReviewError::Api("All batch reviews failed".to_string())));
        }
        Ok(reviews)
    }
}

/// Build diff-only review requests (current v1.0 behavior + context table).
///
/// Extracted from the inline code in `run_review()` so that both the
/// diff-only and semantic paths can share this as a fallback.
fn build_diff_requests(
    batches: &[crate::batch::Batch],
    total_batches: u32,
    model: &str,
    system_prompt: &str,
    custom_instructions: Option<&str>,
    file_contexts: &HashMap<String, FileContext>,
) -> Vec<ReviewRequest> {
    batches
        .iter()
        .enumerate()
        .map(|(i, batch)| {
            let mut request = build_review_request(
                batch,
                u32::try_from(i + 1).expect("batch index within u32"),
                total_batches,
                model,
                system_prompt,
                custom_instructions,
            );
            // Prepend file context table to diff content
            let batch_files = batch.file_list();
            let contexts: Vec<(&str, &FileContext)> = batch_files
                .iter()
                .filter_map(|f| file_contexts.get(*f).map(|ctx| (*f, ctx)))
                .collect();
            if !contexts.is_empty() {
                request.diff_content = format!(
                    "{}{}",
                    format_context_table(&contexts),
                    request.diff_content
                );
            }
            request
        })
        .collect()
}

/// Extract changed line numbers (right side) from a unified diff.
///
/// Parses `@@ -a,b +c,d @@` hunk headers and tracks `+` lines to
/// build a set of changed line numbers on the new (right) side.
///
/// # Arguments
///
/// * `diff` - Raw unified diff content for a single file.
///
/// # Returns
///
/// Set of 1-indexed line numbers that were added or modified.
fn extract_changed_lines(diff: &str) -> HashSet<u32> {
    let mut changed = HashSet::new();
    let mut current_line: u32 = 0;
    for line in diff.lines() {
        if line.starts_with("@@") {
            // Parse @@ -a,b +c,d @@ header
            if let Some(plus_part) = line.split('+').nth(1) {
                if let Some(start_str) = plus_part.split([',', ' ']).next() {
                    current_line = match start_str.trim().parse::<u32>() {
                        Ok(n) if n > 0 => n,
                        _ => {
                            eprintln!(
                                "Warning: malformed hunk header, \
                                 cannot parse start line: '{}'",
                                line
                            );
                            continue;
                        }
                    };
                }
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            if current_line > 0 {
                changed.insert(current_line);
            }
            current_line = current_line.saturating_add(1);
        } else if line.starts_with('-') && !line.starts_with("---") {
            // Removed lines don't advance the right-side counter
        } else {
            current_line = current_line.saturating_add(1);
        }
    }
    changed
}

/// Format a code review as human-readable text.
///
/// Produces a summary header followed by numbered findings with
/// severity tags, file locations, and optional suggestions.
///
/// # Arguments
///
/// * `review` - The code review to format.
fn format_human_readable(review: &CodeReview) -> String {
    let mut out = String::new();
    out.push_str("=== Code Review Summary ===\n");
    out.push_str(&review.summary);
    out.push_str("\n\n");
    out.push_str(&format!("--- Findings ({}) ---\n", review.findings.len()));

    for finding in &review.findings {
        out.push_str(&format!(
            "\n[{}] {}:{} \u{2014} {}\n",
            finding.severity, finding.file, finding.line, finding.title
        ));
        if !finding.finding_id.is_empty() {
            out.push_str(&format!("  ID: {}\n", finding.finding_id));
        }
        out.push_str(&format!("  Category: {}\n", finding.category.slug()));
        out.push_str(&format!("  {}\n", finding.description));
        if !finding.suggestion.is_empty() {
            out.push_str(&format!("  Suggestion: {}\n", finding.suggestion));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendType, CredentialSourceType, ParallelMode, ReviewConfig};
    use crate::test_sync::{CwdGuard, CWD_MUTEX};

    #[test]
    fn new_creates_reviewer_with_config() {
        let config = ReviewConfig {
            model: "test-model".to_string(),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        assert_eq!(reviewer.config().model, "test-model");
    }

    #[test]
    fn config_returns_reference_to_stored_config() {
        let config = ReviewConfig {
            backend: BackendType::ClaudeCode,
            json_output: true,
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        assert_eq!(reviewer.config().backend, BackendType::ClaudeCode);
        assert!(reviewer.config().json_output);
    }

    #[test]
    fn config_preserves_all_fields() {
        let config = ReviewConfig {
            backend: BackendType::AzureAiFoundry,
            model: "claude-haiku-4-5".to_string(),
            fallback_model: Some("claude-sonnet-4-5".to_string()),
            endpoint: Some("https://test.endpoint.com/".to_string()),
            base_ref: "origin/develop".to_string(),
            target_ref: "abc123".to_string(),
            extensions: vec!["*.rs".to_string(), "*.toml".to_string()],
            max_lines_per_batch: 1000,
            system_prompt: "Custom prompt".to_string(),
            system_prompt_path: Some("custom-prompt.txt".to_string()),
            instructions_path: Some("review-instructions.md".to_string()),
            json_output: true,
            output_path: Some("report.json".to_string()),
            cache_enabled: true,
            cost_report: true,
            platform_type: Some("azure-devops".to_string()),
            org_url: Some("https://dev.azure.com/Org".to_string()),
            project: Some("MyProject".to_string()),
            parallel: ParallelMode::Hybrid,
            max_concurrent: 8,
            credential_source: CredentialSourceType::Encrypted,
            api_key_encrypted: Some("blob==".to_string()),
            vault_url: Some("https://vault.example.com".to_string()),
            vault_secret_name: Some("my-secret".to_string()),
            key_password: Some("my-password".to_string()),
            semantic: false,
        };
        let reviewer = Panoptico::new(config);
        let c = reviewer.config();
        assert_eq!(c.backend, BackendType::AzureAiFoundry);
        assert_eq!(c.model, "claude-haiku-4-5");
        assert_eq!(c.fallback_model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(c.endpoint.as_deref(), Some("https://test.endpoint.com/"));
        assert_eq!(c.base_ref, "origin/develop");
        assert_eq!(c.target_ref, "abc123");
        assert_eq!(c.extensions.len(), 2);
        assert_eq!(c.max_lines_per_batch, 1000);
        assert_eq!(c.system_prompt, "Custom prompt");
        assert_eq!(c.system_prompt_path.as_deref(), Some("custom-prompt.txt"));
        assert_eq!(
            c.instructions_path.as_deref(),
            Some("review-instructions.md")
        );
        assert!(c.json_output);
        assert_eq!(c.output_path.as_deref(), Some("report.json"));
        assert!(c.cache_enabled);
        assert!(c.cost_report);
        assert_eq!(c.platform_type.as_deref(), Some("azure-devops"));
        assert_eq!(c.org_url.as_deref(), Some("https://dev.azure.com/Org"));
        assert_eq!(c.project.as_deref(), Some("MyProject"));
        assert_eq!(c.parallel, ParallelMode::Hybrid);
        assert_eq!(c.max_concurrent, 8);
        assert_eq!(c.credential_source, CredentialSourceType::Encrypted);
        assert_eq!(c.api_key_encrypted.as_deref(), Some("blob=="));
        assert_eq!(c.vault_url.as_deref(), Some("https://vault.example.com"));
        assert_eq!(c.vault_secret_name.as_deref(), Some("my-secret"));
        assert_eq!(c.key_password.as_deref(), Some("my-password"));
    }

    #[test]
    fn command_variants_are_distinct() {
        assert_ne!(Command::Test, Command::Review);
        assert_eq!(Command::Test, Command::Test);
        assert_eq!(Command::Review, Command::Review);
        assert_eq!(Command::GenerateConfig, Command::GenerateConfig);
        assert_eq!(Command::GeneratePrompt, Command::GeneratePrompt);
        assert_ne!(Command::GeneratePrompt, Command::Test);
        assert_ne!(Command::GeneratePrompt, Command::GenerateConfig);
        assert_ne!(Command::GenerateConfig, Command::Test);
        assert_ne!(Command::GenerateConfig, Command::Review);
        let encrypt = Command::EncryptKey {
            password: "pwd".to_string(),
            api_key: "key".to_string(),
            config_path: None,
        };
        assert_ne!(encrypt, Command::Test);
        assert_ne!(encrypt, Command::Review);
    }

    #[test]
    fn generate_config_fails_when_file_exists() {
        let _lock = CWD_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join(crate::config::DEFAULT_CONFIG_FILENAME);
        std::fs::write(&file_path, "existing").unwrap();

        let _cwd = CwdGuard::new(dir.path());

        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.generate_config();

        assert!(result.is_err(), "Should refuse to overwrite existing file");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("already exists"),
            "Error should mention file already exists: {}",
            msg
        );
    }

    #[test]
    fn generate_config_creates_file() {
        let _lock = CWD_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::new(dir.path());

        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.generate_config();

        assert!(result.is_ok(), "Should create config file successfully");
        let content =
            std::fs::read_to_string(dir.path().join(crate::config::DEFAULT_CONFIG_FILENAME))
                .unwrap();
        assert!(
            content.contains("[review]"),
            "Generated file should contain [review] section"
        );
    }

    #[test]
    fn generate_prompt_creates_file() {
        let _lock = CWD_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::new(dir.path());

        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.generate_prompt();

        assert!(result.is_ok(), "Should create prompt file successfully");
        let content = std::fs::read_to_string(dir.path().join(DEFAULT_PROMPT_FILENAME)).unwrap();
        assert!(
            content.contains("expert code reviewer"),
            "Generated file should contain the default system prompt"
        );
    }

    #[test]
    fn generate_prompt_fails_when_file_exists() {
        let _lock = CWD_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join(DEFAULT_PROMPT_FILENAME);
        std::fs::write(&file_path, "existing").unwrap();

        let _cwd = CwdGuard::new(dir.path());

        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.generate_prompt();

        assert!(result.is_err(), "Should refuse to overwrite existing file");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("already exists"),
            "Error should mention file already exists: {}",
            msg
        );
    }

    #[test]
    fn resolve_system_prompt_uses_default_when_no_path() {
        let reviewer = Panoptico::new(ReviewConfig::default());
        let prompt = reviewer.resolve_system_prompt().unwrap();
        assert!(
            prompt.contains("expert code reviewer"),
            "Should use DEFAULT_SYSTEM_PROMPT when no path is set"
        );
    }

    #[test]
    fn resolve_system_prompt_reads_file_when_path_set() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("custom-prompt.txt");
        std::fs::write(&file_path, "Only report MISRA C violations.").unwrap();

        let config = ReviewConfig {
            system_prompt_path: Some(file_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let prompt = reviewer.resolve_system_prompt().unwrap();
        assert_eq!(prompt, "Only report MISRA C violations.");
    }

    #[test]
    fn resolve_system_prompt_returns_error_for_missing_file() {
        let config = ReviewConfig {
            system_prompt_path: Some("nonexistent-prompt.txt".to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.resolve_system_prompt();
        assert!(result.is_err(), "Should return error for missing file");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("Failed to read system prompt"),
            "Error should mention failed read: {}",
            msg
        );
    }

    // ── output_review tests ──────────────────────────────────

    fn sample_review() -> CodeReview {
        use crate::backend::mock::make_test_finding;
        use crate::backend::Severity;

        let mut f1 = make_test_finding("src/main.c", 42, "Buffer overflow in strcpy");
        f1.severity = Severity::Critical;
        f1.description = "Using strcpy without bounds checking.".to_string();
        f1.suggestion = "Use strncpy or snprintf instead.".to_string();

        let mut f2 = make_test_finding("src/utils.c", 15, "Unchecked return value");
        f2.description = "Return value of malloc is not checked.".to_string();
        f2.suggestion = "Add NULL check after malloc call.".to_string();

        let mut f3 = make_test_finding("src/lib.c", 88, "Good error handling pattern");
        f3.severity = Severity::Positive;
        f3.description = "Proper use of Result type for error propagation.".to_string();
        f3.suggestion = String::new();

        CodeReview {
            summary: "Buffer overflow vulnerability found in main.c".to_string(),
            findings: vec![f1, f2, f3],
        }
    }

    #[test]
    fn format_review_human_readable_header() {
        let review = sample_review();
        let text = format_human_readable(&review);
        assert!(text.starts_with("=== Code Review Summary ===\n"));
        assert!(text.contains("Buffer overflow vulnerability found in main.c"));
    }

    #[test]
    fn format_review_human_readable_finding_count() {
        let review = sample_review();
        let text = format_human_readable(&review);
        assert!(text.contains("--- Findings (3) ---"));
    }

    #[test]
    fn format_review_human_readable_severity_tags() {
        let review = sample_review();
        let text = format_human_readable(&review);
        assert!(text.contains("[CRITICAL] src/main.c:42"));
        assert!(text.contains("[WARNING] src/utils.c:15"));
        assert!(text.contains("[POSITIVE] src/lib.c:88"));
    }

    #[test]
    fn format_review_human_readable_skips_empty_suggestion() {
        let review = sample_review();
        let text = format_human_readable(&review);
        // The positive finding has an empty suggestion — no "Suggestion:" line.
        let positive_block = text.split("[POSITIVE]").nth(1).unwrap();
        assert!(
            !positive_block.contains("Suggestion:"),
            "Empty suggestion should be omitted"
        );
    }

    #[test]
    fn format_review_human_readable_includes_suggestion() {
        let review = sample_review();
        let text = format_human_readable(&review);
        assert!(text.contains("Suggestion: Use strncpy or snprintf instead."));
        assert!(text.contains("Suggestion: Add NULL check after malloc call."));
    }

    #[test]
    fn format_review_human_readable_empty_findings() {
        let review = CodeReview {
            summary: "No issues found.".to_string(),
            findings: vec![],
        };
        let text = format_human_readable(&review);
        assert!(text.contains("--- Findings (0) ---"));
    }

    #[test]
    fn output_review_json_to_stdout() {
        let config = ReviewConfig {
            json_output: true,
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let review = sample_review();
        let result = reviewer.output_review(&review);
        assert!(result.is_ok(), "JSON output should not fail");
    }

    #[test]
    fn output_review_human_readable_to_stdout() {
        let config = ReviewConfig {
            json_output: false,
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let review = sample_review();
        let result = reviewer.output_review(&review);
        assert!(result.is_ok(), "Human-readable output should not fail");
    }

    #[test]
    fn output_review_json_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("report.json");
        let config = ReviewConfig {
            json_output: true,
            output_path: Some(file_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let review = sample_review();
        reviewer.output_review(&review).unwrap();

        let content = std::fs::read_to_string(&file_path).unwrap();
        let parsed: CodeReview = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.findings.len(), 3);
        assert!(content.contains("\"severity\": \"critical\""));
    }

    #[test]
    fn output_review_text_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("report.txt");
        let config = ReviewConfig {
            json_output: false,
            output_path: Some(file_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let review = sample_review();
        reviewer.output_review(&review).unwrap();

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("=== Code Review Summary ==="));
        assert!(content.contains("[CRITICAL] src/main.c:42"));
    }

    #[test]
    fn output_review_to_invalid_path_includes_context() {
        let config = ReviewConfig {
            json_output: false,
            output_path: Some("/nonexistent/dir/report.txt".to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let review = sample_review();
        let result = reviewer.output_review(&review);
        assert!(result.is_err(), "Writing to invalid path should fail");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("Failed to write review to"),
            "Error should include file path context: {}",
            msg
        );
    }

    #[test]
    fn format_review_human_readable_all_severities() {
        use crate::backend::{Finding, Severity};
        use crate::finding_id::Category;
        let review = CodeReview {
            summary: "Mixed findings".to_string(),
            findings: vec![
                Finding {
                    severity: Severity::Critical,
                    file: "a.rs".to_string(),
                    line: 1,
                    title: "T1".to_string(),
                    description: "D1".to_string(),
                    suggestion: "S1".to_string(),
                    category: Category::default(),
                    finding_id: String::new(),
                    reasoning: String::new(),
                },
                Finding {
                    severity: Severity::Warning,
                    file: "b.rs".to_string(),
                    line: 2,
                    title: "T2".to_string(),
                    description: "D2".to_string(),
                    suggestion: "S2".to_string(),
                    category: Category::default(),
                    finding_id: String::new(),
                    reasoning: String::new(),
                },
                Finding {
                    severity: Severity::Suggestion,
                    file: "c.rs".to_string(),
                    line: 3,
                    title: "T3".to_string(),
                    description: "D3".to_string(),
                    suggestion: "S3".to_string(),
                    category: Category::default(),
                    finding_id: String::new(),
                    reasoning: String::new(),
                },
                Finding {
                    severity: Severity::Positive,
                    file: "d.rs".to_string(),
                    line: 4,
                    title: "T4".to_string(),
                    description: "D4".to_string(),
                    suggestion: String::new(),
                    category: Category::default(),
                    finding_id: String::new(),
                    reasoning: String::new(),
                },
            ],
        };
        let text = format_human_readable(&review);
        assert!(text.contains("[CRITICAL] a.rs:1"));
        assert!(text.contains("[WARNING] b.rs:2"));
        assert!(text.contains("[SUGGESTION] c.rs:3"));
        assert!(text.contains("[POSITIVE] d.rs:4"));
    }

    // ── encrypt_key TOML auto-update tests ─────────────────────

    #[test]
    fn encrypt_key_updates_toml_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("panoptico.toml");
        std::fs::write(
            &toml_path,
            "[review]\nmodel = \"claude-sonnet-4-5\"\n\n[azure]\nendpoint = \"https://example.com\"\n",
        )
        .unwrap();

        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.encrypt_key(
            "password123",
            "sk-test-key",
            Some(toml_path.to_str().unwrap()),
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(&toml_path).unwrap();
        assert!(
            content.contains("api_key_encrypted"),
            "TOML should contain api_key_encrypted after encrypt_key: {}",
            content
        );
        assert!(
            content.contains("endpoint = \"https://example.com\""),
            "Existing fields should be preserved: {}",
            content
        );
    }

    #[test]
    fn encrypt_key_preserves_toml_comments_and_fields() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("panoptico.toml");
        let original = "\
# Main review settings\n\
[review]\n\
model = \"claude-sonnet-4-5\"\n\
# Maximum lines per batch\n\
max_lines_per_batch = 500\n\
\n\
# Azure backend configuration\n\
[azure]\n\
endpoint = \"https://my-endpoint.com\"\n\
credential_source = \"encrypted\"\n";
        std::fs::write(&toml_path, original).unwrap();

        let reviewer = Panoptico::new(ReviewConfig::default());
        reviewer
            .encrypt_key("pass", "sk-key", Some(toml_path.to_str().unwrap()))
            .unwrap();

        let updated = std::fs::read_to_string(&toml_path).unwrap();
        assert!(
            updated.contains("# Main review settings"),
            "Comments should be preserved: {}",
            updated
        );
        assert!(
            updated.contains("# Maximum lines per batch"),
            "Inline comments should be preserved: {}",
            updated
        );
        assert!(
            updated.contains("# Azure backend configuration"),
            "Section comments should be preserved: {}",
            updated
        );
        assert!(
            updated.contains("model = \"claude-sonnet-4-5\""),
            "Review model should be preserved: {}",
            updated
        );
        assert!(
            updated.contains("credential_source = \"encrypted\""),
            "Credential source should be preserved: {}",
            updated
        );
        assert!(
            updated.contains("api_key_encrypted"),
            "Encrypted key should be added: {}",
            updated
        );
    }

    #[test]
    fn encrypt_key_creates_azure_section_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("panoptico.toml");
        std::fs::write(&toml_path, "[review]\nmodel = \"claude-sonnet-4-5\"\n").unwrap();

        let reviewer = Panoptico::new(ReviewConfig::default());
        reviewer
            .encrypt_key("pass", "sk-key", Some(toml_path.to_str().unwrap()))
            .unwrap();

        let content = std::fs::read_to_string(&toml_path).unwrap();
        assert!(
            content.contains("[azure]"),
            "Should create [azure] section: {}",
            content
        );
        assert!(
            content.contains("api_key_encrypted"),
            "Should set api_key_encrypted: {}",
            content
        );
    }

    #[test]
    fn encrypt_key_skips_update_when_no_config_path() {
        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.encrypt_key("pass", "sk-key", None);
        assert!(result.is_ok(), "Should succeed without config path");
    }

    #[test]
    fn encrypt_key_skips_update_when_file_missing() {
        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.encrypt_key("pass", "sk-key", Some("/nonexistent/file.toml"));
        assert!(
            result.is_ok(),
            "Should succeed when config file does not exist"
        );
    }

    // --- Phase 1 (G2): Chain-of-thought reasoning tests ---

    #[test]
    fn human_readable_excludes_reasoning() {
        use crate::backend::{Finding, Severity};
        use crate::finding_id::Category;
        let review = CodeReview {
            summary: "Test review".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 10,
                title: "Test finding".to_string(),
                description: "A description.".to_string(),
                suggestion: "A suggestion.".to_string(),
                category: Category::LogicError,
                finding_id: "abc123".to_string(),
                reasoning: "1. Does X. 2. Could fail. 3. Unlikely. 4. Suggestion.".to_string(),
            }],
        };
        let text = format_human_readable(&review);
        assert!(
            !text.contains("Does X"),
            "Human-readable output must NOT contain reasoning text"
        );
        assert!(
            !text.contains("Unlikely"),
            "Human-readable output must NOT contain reasoning text"
        );
        assert!(
            text.contains("A description."),
            "Human-readable output must still contain description"
        );
    }

    // ── build_credential_source validation tests ──────────────

    #[test]
    fn build_credential_source_env_succeeds() {
        let reviewer = Panoptico::new(ReviewConfig::default());
        let result = reviewer.build_credential_source();
        assert!(result.is_ok(), "Env source should always succeed");
    }

    #[test]
    fn build_credential_source_encrypted_missing_blob_returns_error() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Encrypted,
            api_key_encrypted: None,
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(
            matches!(result, Err(ReviewError::Config(ref msg)) if msg.contains("api_key_encrypted")),
            "Missing encrypted blob should return Config error: {:?}",
            result
        );
    }

    #[test]
    fn build_credential_source_encrypted_empty_blob_returns_error() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Encrypted,
            api_key_encrypted: Some(String::new()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(
            matches!(result, Err(ReviewError::Config(_))),
            "Empty encrypted blob should return Config error"
        );
    }

    #[test]
    fn build_credential_source_vault_missing_url_returns_error() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Vault,
            vault_url: None,
            vault_secret_name: Some("secret".to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(
            matches!(result, Err(ReviewError::Config(ref msg)) if msg.contains("vault_url")),
            "Missing vault URL should return Config error: {:?}",
            result
        );
    }

    #[test]
    fn build_credential_source_vault_missing_name_returns_error() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Vault,
            vault_url: Some("https://vault.example.com".to_string()),
            vault_secret_name: None,
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(
            matches!(result, Err(ReviewError::Config(ref msg)) if msg.contains("vault_secret_name")),
            "Missing vault secret name should return Config error: {:?}",
            result
        );
    }

    #[test]
    fn build_credential_source_vault_valid_succeeds() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Vault,
            vault_url: Some("https://vault.example.com".to_string()),
            vault_secret_name: Some("api-key".to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(result.is_ok(), "Valid vault config should succeed");
    }

    #[test]
    fn build_credential_source_encrypted_valid_succeeds() {
        let config = ReviewConfig {
            credential_source: CredentialSourceType::Encrypted,
            api_key_encrypted: Some("base64blob==".to_string()),
            ..Default::default()
        };
        let reviewer = Panoptico::new(config);
        let result = reviewer.build_credential_source();
        assert!(result.is_ok(), "Valid encrypted config should succeed");
    }

    // --- Phase 4 (G0): extract_changed_lines tests ---

    #[test]
    fn extract_changed_lines_single_hunk() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -8,6 +10,8 @@ fn main() {
     let x = 1;
+    let y = 2;
+    let z = 3;
     let w = 4;
";
        let changed = extract_changed_lines(diff);
        assert!(changed.contains(&11), "Line 11 should be changed");
        assert!(changed.contains(&12), "Line 12 should be changed");
        assert_eq!(changed.len(), 2, "Exactly 2 lines changed");
    }

    #[test]
    fn extract_changed_lines_multi_hunk() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@
 use std::io;
+use std::fs;

 fn foo() {
@@ -20,3 +21,4 @@ fn bar() {
     let a = 1;
+    let b = 2;
 }
";
        let changed = extract_changed_lines(diff);
        assert!(changed.contains(&2), "Line 2 from first hunk");
        assert!(changed.contains(&22), "Line 22 from second hunk");
        assert_eq!(changed.len(), 2, "One line changed per hunk");
    }

    #[test]
    fn extract_changed_lines_empty_diff() {
        let changed = extract_changed_lines("");
        assert!(
            changed.is_empty(),
            "Empty diff should return empty set, got: {:?}",
            changed
        );
    }

    #[test]
    fn extract_changed_lines_malformed_header_warns() {
        let diff = "\
@@ invalid header @@
+added line
";
        let changed = extract_changed_lines(diff);
        // Malformed header can't parse start line, so changed should be empty.
        assert!(
            changed.is_empty(),
            "Malformed hunk header should produce empty set, got: {:?}",
            changed
        );
    }
}
